//! Classify a file path found in a search/stack trace into a human-readable
//! attribution: either the user's own flake ("your config") or a named flake
//! input ("input nixpkgs", "input foo.bar", ...).

use std::path::{Path, PathBuf};

use crate::archive::ArchiveTree;

/// Configuration for path classification.
pub struct Classifier<'a> {
    /// The user's flake directory (project root), if known. Files under this
    /// tree are reported as "your config".
    pub project_root: Option<&'a Path>,
    /// The flattened archive tree of flake inputs.
    pub tree: &'a ArchiveTree,
}

impl<'a> Classifier<'a> {
    pub fn new(project_root: Option<&'a Path>, tree: &'a ArchiveTree) -> Self {
        Self { project_root, tree }
    }

    /// Classify `path`. Returns the input name (dotted) or the special
    /// labels `"your config"` / `"unknown"`.
    pub fn classify(&self, path: &Path) -> Attribution<'a> {
        if let Some(root) = self.project_root {
            if path_starts_with(path, root) {
                return Attribution::YourConfig;
            }
        }

        for (name, store_path) in &self.tree.inputs {
            if path_starts_with(path, store_path) {
                return Attribution::Input(name);
            }
        }

        Attribution::Unknown
    }
}

/// What a path was attributed to.
#[derive(Debug, Clone, Copy)]
pub enum Attribution<'a> {
    /// The file lives under the user's project root.
    YourConfig,
    /// The file lives under flake input `<name>`'s source store path.
    Input(&'a str),
    /// Couldn't match it to anything (e.g. a path outside the flake closure).
    Unknown,
}

fn path_starts_with(path: &Path, base: &Path) -> bool {
    let path = normalize_slashes(path);
    let base = normalize_slashes(base);
    if path == base {
        return true;
    }
    let base = base.as_os_str().to_string_lossy();
    let path = path.as_os_str().to_string_lossy();
    path.len() > base.len() && path.starts_with(&*base) && path.as_bytes()[base.len()] == b'/'
}

/// Reduce a path to a string form without trailing slashes (used for prefix
/// comparison only).
fn normalize_slashes(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    let stripped = s.trim_end_matches('/');
    PathBuf::from(stripped)
}

#[cfg(test)]
#[path = "map_tests.rs"]
mod tests;
