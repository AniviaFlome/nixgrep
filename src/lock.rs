//! Parse `flake.lock` and build a resolver from a flake-input *dotted name*
//! (e.g. `catppuccin`, `catppuccin.nixpkgs`) to its locked ref, so we can
//! construct a source URL for a file inside that input.
//!
//! `flake.lock` is a graph of nodes keyed by an opaque id. The root node's
//! `inputs` map input names → either a node-id string, or a `{"follows": ...}`
//! alias. Transitive inputs live as nested `inputs` on each node. We walk the
//! graph from the root, tracking the dotted path, and record
//! `dotted_name → locked ref` for every node that has a `locked` field.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// One node in `flake.lock`.
#[derive(Debug, Clone, Deserialize)]
pub struct LockNode {
    #[serde(default)]
    pub inputs: HashMap<String, InputRef>,
    #[serde(default)]
    pub locked: Option<LockedRef>,
}

/// An input reference: a node-id string, a follows alias, or a path array
/// (e.g. `["llm-agents", "nixpkgs"]` — a dotted path from the root).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum InputRef {
    /// A direct reference to a node id (e.g. `"nixpkgs"`).
    Id(String),
    /// A `{"follows": "path/to/input"}` alias.
    Follows { follows: String },
    /// A path array like `["foo", "nixpkgs"]` — node-id segments from the
    /// root. This is the most common form for transitive follows.
    Path(Vec<String>),
}

/// The locked ref of a flake input — enough to build a source URL.
#[derive(Debug, Clone, Deserialize)]
pub struct LockedRef {
    #[serde(rename = "type")]
    pub ref_type: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub rev: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub url: Option<String>,
    /// GitLab / other hosts.
    #[serde(default)]
    pub host: Option<String>,
}

/// A parsed `flake.lock`, mapping dotted input names → locked refs.
#[derive(Debug, Clone)]
pub struct LockGraph {
    /// dotted input name (e.g. `catppuccin`, `catppuccin.nixpkgs`) → locked ref.
    pub inputs: HashMap<String, LockedRef>,
}

/// Parse `flake.lock` at `<flake_dir>/flake.lock`.
pub fn parse(flake_dir: &Path) -> anyhow::Result<LockGraph> {
    let lock_path = flake_dir.join("flake.lock");
    let contents = std::fs::read_to_string(&lock_path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", lock_path.display()))?;
    parse_str(&contents)
}

/// Parse `flake.lock` from its JSON text.
pub fn parse_str(contents: &str) -> anyhow::Result<LockGraph> {
    #[derive(Deserialize)]
    struct LockFile {
        root: String,
        nodes: HashMap<String, LockNode>,
    }
    let lock: LockFile = serde_json::from_str(contents)
        .map_err(|e| anyhow::anyhow!("failed to parse flake.lock: {e}"))?;

    let mut inputs: HashMap<String, LockedRef> = HashMap::new();
    let root_id = lock.root.clone();
    let nodes = &lock.nodes;
    if let Some(root) = nodes.get(&root_id) {
        walk(nodes, root, root, String::new(), &mut inputs);
    }
    Ok(LockGraph { inputs })
}

/// Recursively walk the node graph from `node`, recording every node that has
/// a `locked` ref under its dotted path.
fn walk(
    nodes: &HashMap<String, LockNode>,
    root: &LockNode,
    node: &LockNode,
    prefix: String,
    out: &mut HashMap<String, LockedRef>,
) {
    for (name, input_ref) in &node.inputs {
        let dotted = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        let target = match resolve_ref(nodes, root, input_ref) {
            Some(t) => t,
            None => continue,
        };
        if let Some(locked) = &target.locked {
            out.insert(dotted.clone(), locked.clone());
        }
        // Recurse into the target's own inputs.
        walk(nodes, root, target, dotted, out);
    }
}

/// Resolve a dotted path (from the root) to a node by walking
/// `root.inputs.<seg0> → that node's inputs.<seg1> → ...`.
/// Each segment's ref is resolved (following Id/Follows/Path hops).
fn resolve_path<'a>(
    nodes: &'a HashMap<String, LockNode>,
    root: &'a LockNode,
    segs: &[String],
) -> Option<&'a LockNode> {
    if segs.is_empty() {
        return None;
    }
    // First segment: look up in the root's inputs and resolve the ref.
    let mut node = resolve_ref(nodes, root, root.inputs.get(&segs[0])?)?;
    // Subsequent segments: look up in the current node's inputs.
    for seg in &segs[1..] {
        let r = node.inputs.get(seg)?;
        // A ref inside a non-root node is either an Id (node id) or a
        // Follows/Path (root-relative). Resolve accordingly.
        node = match r {
            InputRef::Id(id) => nodes.get(id)?,
            InputRef::Follows { follows } => {
                let p: Vec<String> = follows.split('.').map(str::to_string).collect();
                resolve_path(nodes, root, &p)?
            }
            InputRef::Path(p) => resolve_path(nodes, root, p)?,
        };
    }
    Some(node)
}

/// Resolve a single `InputRef` (one hop) to a node.
fn resolve_ref<'a>(
    nodes: &'a HashMap<String, LockNode>,
    root: &'a LockNode,
    input_ref: &'a InputRef,
) -> Option<&'a LockNode> {
    match input_ref {
        InputRef::Id(id) => nodes.get(id),
        InputRef::Follows { follows } => {
            let p: Vec<String> = follows.split('.').map(str::to_string).collect();
            resolve_path(nodes, root, &p)
        }
        InputRef::Path(p) => resolve_path(nodes, root, p),
    }
}

impl LockedRef {
    /// Build a web URL to `rel_path` at line `line` (1-indexed) for this ref,
    /// if the host is supported (GitHub, GitLab, SourceHut, Codeberg).
    /// Returns None for tarball/path/file inputs without a known web UI.
    pub fn url(&self, rel_path: &str, line: Option<u64>) -> Option<String> {
        let base = match self.ref_type.as_str() {
            "github" => format!(
                "https://github.com/{}/{}/blob/{}/{}",
                self.owner.as_deref()?,
                self.repo.as_deref()?,
                self.rev.as_deref()?,
                rel_path,
            ),
            "gitlab" => format!(
                "https://{}/{}/{}/-/blob/{}/{}",
                self.host.as_deref().unwrap_or("gitlab.com"),
                self.owner.as_deref()?,
                self.repo.as_deref()?,
                self.rev.as_deref()?,
                rel_path,
            ),
            "sourcehut" => format!(
                "https://git.sr.ht/~{}/{}/tree/{}/item/{}",
                self.owner.as_deref()?,
                self.repo.as_deref()?,
                self.rev.as_deref()?,
                rel_path,
            ),
            "codeberg" => format!(
                "https://codeberg.org/{}/{}/src/commit/{}/{}",
                self.owner.as_deref()?,
                self.repo.as_deref()?,
                self.rev.as_deref()?,
                rel_path,
            ),
            // tarball / path / file / indirect / mercurial — no web UI.
            _ => return None,
        };
        match line {
            Some(l) => Some(format!("{base}#L{l}")),
            None => Some(base),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github(owner: &str, repo: &str, rev: &str) -> LockedRef {
        LockedRef {
            ref_type: "github".into(),
            owner: Some(owner.into()),
            repo: Some(repo.into()),
            rev: Some(rev.into()),
            url: None,
            host: None,
        }
    }

    #[test]
    fn github_url_with_line() {
        let r = github("catppuccin", "nix", "3f3b351");
        assert_eq!(
            r.url("pkgs/vscode/package.nix", Some(18)).as_deref(),
            Some("https://github.com/catppuccin/nix/blob/3f3b351/pkgs/vscode/package.nix#L18"),
        );
    }

    #[test]
    fn github_url_without_line() {
        let r = github("o", "r", "rev");
        assert_eq!(
            r.url("a.nix", None).as_deref(),
            Some("https://github.com/o/r/blob/rev/a.nix"),
        );
    }

    #[test]
    fn tarball_has_no_url() {
        let r = LockedRef {
            ref_type: "tarball".into(),
            owner: None,
            repo: None,
            rev: None,
            url: Some("https://x/tarball".into()),
            host: None,
        };
        assert!(r.url("a.nix", Some(1)).is_none());
    }

    #[test]
    fn gitlab_url() {
        let r = LockedRef {
            ref_type: "gitlab".into(),
            owner: Some("o".into()),
            repo: Some("r".into()),
            rev: Some("rev".into()),
            url: None,
            host: None,
        };
        assert_eq!(
            r.url("a.nix", Some(5)).as_deref(),
            Some("https://gitlab.com/o/r/-/blob/rev/a.nix#L5"),
        );
    }

    #[test]
    fn parses_simple_lock() {
        let json = r#"{
            "root": "root",
            "nodes": {
                "root": {
                    "inputs": {
                        "catppuccin": "catppuccin",
                        "nixpkgs": "nixpkgs"
                    }
                },
                "catppuccin": {
                    "inputs": { "nixpkgs": "nixpkgs_2" },
                    "locked": {
                        "type": "github",
                        "owner": "catppuccin",
                        "repo": "nix",
                        "rev": "abc",
                        "lastModified": 1,
                        "narHash": "sha256-"
                    }
                },
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "owner": "nixos",
                        "repo": "nixpkgs",
                        "rev": "def",
                        "lastModified": 2,
                        "narHash": "sha256-"
                    }
                },
                "nixpkgs_2": {
                    "locked": {
                        "type": "tarball",
                        "url": "https://x/tarball",
                        "rev": "ghi",
                        "narHash": "sha256-"
                    }
                }
            },
            "version": 7
        }"#;
        let g = parse_str(json).unwrap();
        assert!(g.inputs.contains_key("catppuccin"));
        assert!(g.inputs.contains_key("nixpkgs"));
        assert!(g.inputs.contains_key("catppuccin.nixpkgs"));
        let c = g.inputs.get("catppuccin").unwrap();
        assert_eq!(
            c.url("pkgs/vscode/package.nix", Some(18)).as_deref(),
            Some("https://github.com/catppuccin/nix/blob/abc/pkgs/vscode/package.nix#L18")
        );
        assert!(g
            .inputs
            .get("catppuccin.nixpkgs")
            .unwrap()
            .url("a.nix", Some(1))
            .is_none());
    }

    #[test]
    fn follows_alias_resolves() {
        let json = r#"{
            "root": "root",
            "nodes": {
                "root": {
                    "inputs": {
                        "nixpkgs": "nixpkgs",
                        "foo": "foo"
                    }
                },
                "nixpkgs": {
                    "locked": { "type": "github", "owner": "nixos", "repo": "nixpkgs", "rev": "r1" }
                },
                "foo": {
                    "inputs": { "nixpkgs": { "follows": "nixpkgs" } },
                    "locked": { "type": "github", "owner": "me", "repo": "foo", "rev": "r2" }
                }
            },
            "version": 7
        }"#;
        let g = parse_str(json).unwrap();
        let f = g.inputs.get("foo.nixpkgs").unwrap();
        assert_eq!(f.owner.as_deref(), Some("nixos"));
    }

    #[test]
    fn path_array_resolves() {
        let json = r#"{
            "root": "root",
            "nodes": {
                "root": {
                    "inputs": {
                        "nixpkgs": "nixpkgs",
                        "foo": "foo"
                    }
                },
                "nixpkgs": {
                    "locked": { "type": "github", "owner": "nixos", "repo": "nixpkgs", "rev": "r1" }
                },
                "foo": {
                    "inputs": { "nixpkgs": ["nixpkgs"] },
                    "locked": { "type": "github", "owner": "me", "repo": "foo", "rev": "r2" }
                }
            },
            "version": 7
        }"#;
        let g = parse_str(json).unwrap();
        let f = g.inputs.get("foo.nixpkgs").unwrap();
        assert_eq!(f.owner.as_deref(), Some("nixos"));
    }
}
