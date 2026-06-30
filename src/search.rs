//! Mode A: locate the *emitter* of a warning by grepping the flake-input
//! source closure (and the user's own flake dir) for the warning text.
//!
//! Warning messages emitted by `lib.warn`/`builtins.warn` are usually string
//! literals in `.nix` source, so a literal-substring search across the closure
//! will find the `.nix` file + line that emits the warning. We report which
//! flake input (or "your config") that file belongs to.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use grep_regex::RegexMatcherBuilder;
use grep_searcher::{sinks::UTF8, SearcherBuilder};
use walkdir::WalkDir;

use crate::archive::ArchiveTree;
use crate::map::{Attribution, Classifier};
use crate::message::Normalized;

/// A single search hit.
#[derive(Debug, Clone)]
pub struct Hit {
    pub attribution: String,
    pub file: PathBuf,
    pub line_no: u64,
    pub line: String,
}

/// Controls search behavior.
pub struct SearchOpts {
    /// If true, report *all* matches per file (default: stop at the first
    /// match per file).
    pub all_matches: bool,
    /// If true, interpret `needle` as a regex instead of a literal.
    pub regex: bool,
    /// Only search `.nix` files (default: true). When false, search all
    /// regular files (still skips binary/hidden).
    pub nix_only: bool,
    /// Max results across the whole search. None = unlimited.
    pub max_results: Option<usize>,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            all_matches: false,
            regex: false,
            nix_only: true,
            max_results: None,
        }
    }
}

/// Run the search across the user's flake dir (if given) then each flake input
/// source tree. Returns hits in the order searched.
pub fn search(
    project_root: Option<&Path>,
    tree: &ArchiveTree,
    needle: &Normalized,
    opts: &SearchOpts,
) -> anyhow::Result<Vec<Hit>> {
    if needle.is_empty() {
        anyhow::bail!("empty search needle after normalizing the message");
    }

    let matcher = build_matcher(&needle.needle, opts.regex)?;
    let matcher = Arc::new(matcher);
    let classifier = Classifier::new(project_root, tree);

    let mut hits = Vec::new();

    if let Some(root) = project_root {
        search_dir(root, &classifier, &matcher, opts, &mut hits, "your config")?;
        if reached(&hits, opts.max_results) {
            return Ok(hits);
        }
    }

    for (name, path) in &tree.inputs {
        search_dir(path, &classifier, &matcher, opts, &mut hits, name)?;
        if reached(&hits, opts.max_results) {
            return Ok(hits);
        }
    }

    Ok(hits)
}

fn build_matcher(needle: &str, as_regex: bool) -> anyhow::Result<grep_regex::RegexMatcher> {
    let pattern = if as_regex {
        needle.to_string()
    } else {
        regex::escape(needle)
    };
    RegexMatcherBuilder::new()
        .case_insensitive(false)
        .line_terminator(Some(b'\n'))
        .build(&pattern)
        .map_err(|e| anyhow::anyhow!("invalid search needle: {e}"))
}

fn search_dir(
    dir: &Path,
    classifier: &Classifier<'_>,
    matcher: &Arc<grep_regex::RegexMatcher>,
    opts: &SearchOpts,
    hits: &mut Vec<Hit>,
    fallback_label: &str,
) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    for entry in WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_entry(|e| !is_hidden(e.file_name()))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if opts.nix_only && entry.path().extension().and_then(|s| s.to_str()) != Some("nix") {
            continue;
        }
        if reached(hits, opts.max_results) {
            return Ok(());
        }

        let file = entry.path().to_path_buf();
        let _ = search_file(matcher.clone(), &file, |line_no, line| {
            let attr = classifier.classify(&file);
            let attribution = match attr {
                Attribution::YourConfig => "your config".to_string(),
                Attribution::Input(n) => format!("input {n}"),
                Attribution::Unknown => fallback_label.to_string(),
            };
            hits.push(Hit {
                attribution,
                file: file.clone(),
                line_no,
                line: line.to_string(),
            });
            !opts.all_matches
        })?;
    }
    Ok(())
}

/// Returns the number of matches reported from this file.
fn search_file<F>(
    matcher: Arc<grep_regex::RegexMatcher>,
    file: &Path,
    mut on_hit: F,
) -> anyhow::Result<usize>
where
    F: FnMut(u64, &str) -> bool, // returns true to stop searching this file
{
    let mut count = 0usize;
    let mut searcher = SearcherBuilder::new().line_number(true).build();
    searcher.search_path(
        &*matcher,
        file,
        UTF8(|line_no, line| {
            count += 1;
            if on_hit(line_no, line) {
                return Ok(false); // stop
            }
            Ok(true) // continue
        }),
    )?;
    Ok(count)
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_str().map(|s| s.starts_with('.')).unwrap_or(false)
}

fn reached(hits: &[Hit], max: Option<usize>) -> bool {
    max.is_some_and(|m| hits.len() >= m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_literal_in_dir() {
        let tmp = tempfile_dir();
        let f = tmp.join("generic.nix");
        fs::write(
            &f,
            "let\n  x = lib.warn \"pnpm: Override nodejs-slim instead of nodejs\" nodejs;\nin x\n",
        )
        .unwrap();

        let tree = ArchiveTree {
            inputs: vec![],
            root_path: Some(tmp.clone()),
        };
        let needle = Normalized {
            message: "pnpm: Override nodejs-slim instead of nodejs".into(),
            needle: "pnpm: Override nodejs-slim instead of nodejs".into(),
            partial: false,
        };
        let opts = SearchOpts {
            nix_only: true,
            ..Default::default()
        };
        let hits = search(Some(&tmp), &tree, &needle, &opts).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line_no, 2);
        assert_eq!(hits[0].attribution, "your config");
    }

    #[test]
    fn regex_mode_matches_pattern() {
        let tmp = tempfile_dir();
        fs::write(
            tmp.join("foo.nix"),
            "lib.warn \"${name}: is deprecated\" x\n",
        )
        .unwrap();

        let tree = ArchiveTree {
            inputs: vec![],
            root_path: Some(tmp.clone()),
        };
        let needle = Normalized {
            message: ": is deprecated".into(),
            needle: "is.*deprecated".into(),
            partial: true,
        };
        let opts = SearchOpts {
            regex: true,
            nix_only: true,
            ..Default::default()
        };
        let hits = search(Some(&tmp), &tree, &needle, &opts).unwrap();
        assert_eq!(hits.len(), 1);
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nixgrep-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
