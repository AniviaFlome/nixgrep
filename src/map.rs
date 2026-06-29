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
mod tests {
    use super::*;

    fn tree() -> ArchiveTree {
        ArchiveTree {
            inputs: vec![
                (
                    "nixpkgs".to_string(),
                    PathBuf::from("/nix/store/aaa-nixpkgs-source"),
                ),
                (
                    "foo.bar".to_string(),
                    PathBuf::from("/nix/store/zzz-bar-source"),
                ),
            ],
            root_path: None,
        }
    }

    #[test]
    fn classifies_input() {
        let t = tree();
        let c = Classifier::new(None, &t);
        let p = Path::new("/nix/store/aaa-nixpkgs-source/pkgs/development/tools/pnpm/generic.nix");
        assert!(matches!(c.classify(p), Attribution::Input("nixpkgs")));
    }

    #[test]
    fn classifies_nested_input() {
        let t = tree();
        let c = Classifier::new(None, &t);
        let p = Path::new("/nix/store/zzz-bar-source/lib/foo.nix");
        assert!(matches!(c.classify(p), Attribution::Input("foo.bar")));
    }

    #[test]
    fn does_not_match_partial_hash() {
        let t = tree();
        let c = Classifier::new(None, &t);
        let p = Path::new("/nix/store/aaa-nixpkgs-source-extra/lib/foo.nix");
        assert!(matches!(c.classify(p), Attribution::Unknown));
    }

    #[test]
    fn classifies_project_root_first() {
        let t = tree();
        let c = Classifier::new(Some(Path::new("/home/me/flake")), &t);
        let p = Path::new("/home/me/flake/modules/foo.nix");
        assert!(matches!(c.classify(p), Attribution::YourConfig));
    }

    #[test]
    fn classifies_unknown() {
        let t = tree();
        let c = Classifier::new(Some(Path::new("/home/me/flake")), &t);
        let p = Path::new("/etc/passwd");
        assert!(matches!(c.classify(p), Attribution::Unknown));
    }
}
