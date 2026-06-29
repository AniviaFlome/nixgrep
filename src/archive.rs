//! Enumerate flake-input source store paths by parsing
//! `nix flake archive --json --dry-run`.
//!
//! The JSON is a nested tree mirroring the flake's input dependency graph:
//! ```jsonc
//! {
//!   "path": "/nix/store/...-source",
//!   "inputs": {
//!     "nixpkgs": { "path": "...", "inputs": { ... } },
//!     ...
//!   }
//! }
//! ```
//! The root node is the user's flake itself. We flatten it into a list of
//! `(dotted_name, store_path)` pairs, sorted longest-path-first so that
//! prefix matching of a file path is unambiguous.

use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;

/// One node in the `nix flake archive --json` tree.
#[derive(Debug, Clone, Deserialize)]
pub struct ArchiveNode {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub inputs: std::collections::BTreeMap<String, ArchiveNode>,
}

/// A flat list of `(input_name, store_path)` pairs, plus the root flake's
/// store path (which we label as "your config" upstream).
#[derive(Debug, Clone)]
pub struct ArchiveTree {
    /// (dotted input name, source store path). The root entry uses the
    /// empty string as its name and is excluded from `inputs` — callers
    /// treat the project directory separately.
    pub inputs: Vec<(String, PathBuf)>,
    /// Store path of the root flake source, if `archive` reported one.
    #[allow(dead_code)]
    pub root_path: Option<PathBuf>,
}

/// Run `nix flake archive --json --dry-run <flake>` and parse the tree.
pub fn collect(flake: &str) -> anyhow::Result<ArchiveTree> {
    let out = run_archive(flake)?;
    let node: ArchiveNode = serde_json::from_slice(&out)
        .map_err(|e| anyhow::anyhow!("failed to parse `nix flake archive` JSON: {e}"))?;
    Ok(flatten(node))
}

fn run_archive(flake: &str) -> anyhow::Result<Vec<u8>> {
    let mut cmd = Command::new("nix");
    cmd.args(["flake", "archive", "--json", "--dry-run"]);
    if !flake.is_empty() && flake != "." {
        cmd.arg(flake);
    }
    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run `nix flake archive`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "`nix flake archive --json --dry-run` failed ({}):\n{}",
            output.status,
            stderr.trim()
        );
    }
    Ok(output.stdout)
}

/// Flatten the nested archive tree into a list of `(dotted_name, path)` pairs,
/// sorted longest-path-first. The root node's own path is returned separately
/// and is NOT included in the flat list (the caller handles the user's project
/// dir independently).
fn flatten(node: ArchiveNode) -> ArchiveTree {
    let root_path = node.path.as_deref().map(PathBuf::from);

    let mut inputs = Vec::new();
    walk(&node.inputs, String::new(), &mut inputs);

    inputs.sort_by_key(|b| std::cmp::Reverse(b.1.as_os_str().len()));

    ArchiveTree { inputs, root_path }
}

fn walk(
    inputs: &std::collections::BTreeMap<String, ArchiveNode>,
    prefix: String,
    out: &mut Vec<(String, PathBuf)>,
) {
    for (name, node) in inputs {
        let dotted = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        if let Some(p) = &node.path {
            out.push((dotted.clone(), PathBuf::from(p)));
        }
        if !node.inputs.is_empty() {
            walk(&node.inputs, dotted, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn flatten_simple() {
        let json = br#"{
            "path": "/nix/store/aaa-root-source",
            "inputs": {
                "nixpkgs": {
                    "path": "/nix/store/bbb-nixpkgs-source",
                    "inputs": {
                        "nixpkgs-lib": { "path": "/nix/store/ccc-lib-source", "inputs": {} }
                    }
                },
                "flake-utils": { "path": "/nix/store/ddd-utils-source", "inputs": {} }
            }
        }"#;
        let node: ArchiveNode = serde_json::from_slice(json).unwrap();
        let tree = flatten(node);
        assert_eq!(
            tree.root_path.as_deref(),
            Some(Path::new("/nix/store/aaa-root-source"))
        );
        let names: Vec<_> = tree.inputs.iter().map(|(n, _)| n.clone()).collect();
        assert!(names.contains(&"nixpkgs".to_string()));
        assert!(names.contains(&"nixpkgs.nixpkgs-lib".to_string()));
        assert!(names.contains(&"flake-utils".to_string()));
        assert!(
            tree.inputs[0].1.as_os_str().len() >= tree.inputs.last().unwrap().1.as_os_str().len()
        );
    }
}
