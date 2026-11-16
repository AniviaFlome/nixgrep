use super::*;

fn github(owner: &str, repo: &str, rev: &str) -> LockedRef {
    LockedRef {
        ref_type: "github".into(),
        owner: Some(owner.into()),
        repo: Some(repo.into()),
        rev: Some(rev.into()),
        url: None,
        host: None,
        last_modified: None,
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
        last_modified: None,
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
        last_modified: None,
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
    assert!(
        g.inputs
            .get("catppuccin.nixpkgs")
            .unwrap()
            .url("a.nix", Some(1))
            .is_none()
    );
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

#[test]
fn originals_and_top_level() {
    let json = r#"{
            "root": "root",
            "nodes": {
                "root": {
                    "inputs": {
                        "pinned": "pinned",
                        "floating": "floating",
                        "tb": "tb"
                    }
                },
                "pinned": {
                    "locked": {
                        "type": "github", "owner": "o", "repo": "r",
                        "rev": "abc123", "lastModified": 1700000000,
                        "narHash": "sha256-"
                    },
                    "original": {
                        "type": "github", "owner": "o", "repo": "r",
                        "rev": "abc123"
                    }
                },
                "floating": {
                    "locked": {
                        "type": "github", "owner": "nixos", "repo": "nixpkgs",
                        "rev": "def456", "lastModified": 1700000001,
                        "narHash": "sha256-"
                    },
                    "original": {
                        "type": "github", "owner": "nixos", "repo": "nixpkgs",
                        "ref": "nixos-unstable"
                    }
                },
                "tb": {
                    "locked": { "type": "tarball", "url": "https://x/t.tgz", "narHash": "sha256-" },
                    "original": { "type": "tarball", "url": "https://x/t.tgz" }
                }
            },
            "version": 7
        }"#;
    let g = parse_str(json).unwrap();
    assert_eq!(g.top_level, vec!["floating", "pinned", "tb"]);
    let p = g.originals.get("pinned").unwrap();
    assert_eq!(p.rev.as_deref(), Some("abc123"));
    assert!(p.ref_name.is_none());
    let f = g.originals.get("floating").unwrap();
    assert!(f.rev.is_none());
    assert_eq!(f.ref_name.as_deref(), Some("nixos-unstable"));
    let locked = g.inputs.get("pinned").unwrap();
    assert_eq!(locked.last_modified, Some(1700000000));
}

#[test]
fn follows_alias_not_duplicated() {
    let json = r#"{
            "root": "root",
            "nodes": {
                "root": {
                    "inputs": {
                        "nixpkgs": "nixpkgs",
                        "foo": { "follows": "nixpkgs" }
                    }
                },
                "nixpkgs": {
                    "locked": { "type": "github", "owner": "nixos", "repo": "nixpkgs", "rev": "r1" }
                }
            },
            "version": 7
        }"#;
    let g = parse_str(json).unwrap();
    assert!(g.inputs.contains_key("nixpkgs"));
    assert!(
        !g.inputs.contains_key("foo"),
        "follows alias must not duplicate"
    );
    assert!(!g.originals.contains_key("foo"));
}
