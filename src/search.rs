//! Mode A: locate the *emitter* of a warning by grepping the flake-input
//! source closure (and the user's own flake dir) for the warning text.
//!
//! Warning messages emitted by `lib.warn`/`builtins.warn` are usually string
//! literals in `.nix` source, so a literal-substring search across the closure
//! will find the `.nix` file + line that emits the warning. We report which
//! flake input (or "your config") that file belongs to.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
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

fn build_matcher(needle: &str, as_regex: bool) -> anyhow::Result<Regex> {
    let pattern = if as_regex {
        needle.to_string()
    } else {
        regex::escape(needle)
    };
    Regex::new(&pattern).map_err(|e| anyhow::anyhow!("invalid search needle: {e}"))
}

/// Search the closure once for *all* needles, returning one hit vec per needle
/// (in the same order as `needles`).
///
/// `scan` captures several warnings per build; walking the closure
/// (~800k `.nix` files on a large config) once per warning is pure waste.
/// This walks each input directory once and tests every line against every
/// needle, and searches independent input directories on separate threads
/// (bounded by available parallelism). Result order is deterministic: the
/// project root first, then inputs in tree order; within each, walk order.
/// `max_results` applies per needle ("max locate hits per warning").
pub fn search_all(
    project_root: Option<&Path>,
    tree: &ArchiveTree,
    needles: &[Normalized],
    opts: &SearchOpts,
) -> anyhow::Result<Vec<Vec<Hit>>> {
    if needles.iter().any(Normalized::is_empty) {
        anyhow::bail!("empty search needle after normalizing the message");
    }
    let matchers: Vec<Regex> = needles
        .iter()
        .map(|n| build_matcher(&n.needle, opts.regex))
        .collect::<anyhow::Result<_>>()?;

    // One shard per searched directory; each is an independent walk.
    let mut shards: Vec<(String, PathBuf)> = Vec::new();
    if let Some(root) = project_root {
        shards.push(("your config".to_string(), root.to_path_buf()));
    }
    for (name, path) in &tree.inputs {
        shards.push((name.clone(), path.clone()));
    }
    if shards.is_empty() {
        return Ok(vec![Vec::new(); needles.len()]);
    }

    let worker_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(shards.len());
    let queue = std::sync::Mutex::new(std::collections::VecDeque::from_iter(0..shards.len()));
    // Slot per shard; workers fill their slot, the main thread merges in order.
    let slots = std::sync::Mutex::new(vec![None; shards.len()]);

    std::thread::scope(|s| {
        for _ in 0..worker_count {
            s.spawn(|| {
                let classifier = Classifier::new(project_root, tree);
                loop {
                    let idx = queue.lock().unwrap().pop_front();
                    let Some(idx) = idx else { break };
                    let (label, dir) = &shards[idx];
                    let mut hits = vec![Vec::new(); needles.len()];
                    let _ = search_dir_multi(dir, &classifier, &matchers, opts, label, &mut hits);
                    slots.lock().unwrap()[idx] = Some(hits);
                }
            });
        }
    });

    let mut out = vec![Vec::new(); needles.len()];
    for slot in slots.into_inner().unwrap().into_iter().flatten() {
        for (i, mut hits) in slot.into_iter().enumerate() {
            out[i].append(&mut hits);
        }
    }
    if let Some(m) = opts.max_results {
        for v in &mut out {
            v.truncate(m);
        }
    }
    Ok(out)
}

/// Walk one directory testing every line against every matcher, appending
/// hits per needle index into `hits`.
fn search_dir_multi(
    dir: &Path,
    classifier: &Classifier<'_>,
    matchers: &[Regex],
    opts: &SearchOpts,
    fallback_label: &str,
    hits: &mut [Vec<Hit>],
) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    for entry in WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        // Same hidden-dir pruning as the single-needle search: skip hidden
        // children, but never the explicitly-passed root itself.
        .filter_entry(|e| e.depth() == 0 || !is_hidden(e.file_name()))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if opts.nix_only && entry.path().extension().and_then(|s| s.to_str()) != Some("nix") {
            continue;
        }

        let file = entry.path().to_path_buf();
        // Per-needle "already hit in this file" flags: without --all, only
        // the first match per file per needle is reported.
        let mut file_hit = vec![false; matchers.len()];
        let _ = search_file_multi(
            matchers,
            &file,
            &mut file_hit,
            opts,
            |idx, line_no, line| {
                let attr = classifier.classify(&file);
                let attribution = match attr {
                    Attribution::YourConfig => "your config".to_string(),
                    Attribution::Input(n) => format!("input {n}"),
                    Attribution::Unknown => fallback_label.to_string(),
                };
                hits[idx].push(Hit {
                    attribution,
                    file: file.clone(),
                    line_no,
                    line: line.to_string(),
                });
            },
        )?;
    }
    Ok(())
}

/// Test every line of `file` against every matcher, calling `on_hit` with the
/// needle index for each reportable match.
fn search_file_multi<F>(
    matchers: &[Regex],
    file: &Path,
    file_hit: &mut [bool],
    opts: &SearchOpts,
    mut on_hit: F,
) -> anyhow::Result<()>
where
    F: FnMut(usize, u64, &str),
{
    // Skip non-UTF8 / binary files without failing the whole search.
    let content = match fs::read_to_string(file) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    for (idx, line) in content.lines().enumerate() {
        for (i, m) in matchers.iter().enumerate() {
            if file_hit[i] && !opts.all_matches {
                continue;
            }
            if m.is_match(line) {
                file_hit[i] = true;
                // Lines are 1-based to match grep-searcher numbering.
                on_hit(i, idx as u64 + 1, line);
                if !opts.all_matches {
                    // This needle is done with this file, but other
                    // needles may still match later lines.
                    continue;
                }
            }
        }
    }
    Ok(())
}

fn search_dir(
    dir: &Path,
    classifier: &Classifier<'_>,
    matcher: &Regex,
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
        // Skip hidden files/dirs (e.g. `.git`), but never prune the search
        // root itself: an explicitly-passed root like `.` or `~/.dotfiles`
        // looks "hidden" by file name yet must still be searched.
        .filter_entry(|e| e.depth() == 0 || !is_hidden(e.file_name()))
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
        let _ = search_file(matcher, &file, |line_no, line| {
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
fn search_file<F>(matcher: &Regex, file: &Path, mut on_hit: F) -> anyhow::Result<usize>
where
    F: FnMut(u64, &str) -> bool, // returns true to stop searching this file
{
    // Skip non-UTF8 / binary files without failing the whole search.
    let content = match fs::read_to_string(file) {
        Ok(c) => c,
        Err(_) => return Ok(0),
    };
    let mut count = 0usize;
    for (idx, line) in content.lines().enumerate() {
        if matcher.is_match(line) {
            count += 1;
            // Lines are 1-based to match grep-searcher numbering.
            if on_hit(idx as u64 + 1, line) {
                break;
            }
        }
    }
    Ok(count)
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_str().map(|s| s.starts_with('.')).unwrap_or(false)
}

fn reached(hits: &[Hit], max: Option<usize>) -> bool {
    max.is_some_and(|m| hits.len() >= m)
}

#[cfg(test)]
#[path = "../tests/unit/search_tests.rs"]
mod tests;
