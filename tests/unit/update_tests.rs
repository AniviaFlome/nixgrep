use super::*;
use crate::lock::parse_str;

fn locked(ref_type: &str, owner: Option<&str>, repo: Option<&str>, rev: Option<&str>) -> LockedRef {
    LockedRef {
        ref_type: ref_type.into(),
        owner: owner.map(str::to_string),
        repo: repo.map(str::to_string),
        rev: rev.map(str::to_string),
        url: None,
        host: None,
        last_modified: None,
    }
}

#[test]
fn remote_urls_per_host() {
    let gh = locked("github", Some("o"), Some("r"), None);
    assert_eq!(remote_url(&gh).as_deref(), Some("https://github.com/o/r"));
    let gl = locked("gitlab", Some("o"), Some("r"), None);
    assert_eq!(remote_url(&gl).as_deref(), Some("https://gitlab.com/o/r"));
    let mut gl_host = gl.clone();
    gl_host.host = Some("git.example.com".into());
    assert_eq!(
        remote_url(&gl_host).as_deref(),
        Some("https://git.example.com/o/r")
    );
    let sh = locked("sourcehut", Some("o"), Some("r"), None);
    assert_eq!(remote_url(&sh).as_deref(), Some("https://git.sr.ht/~o/r"));
    let cb = locked("codeberg", Some("o"), Some("r"), None);
    assert_eq!(remote_url(&cb).as_deref(), Some("https://codeberg.org/o/r"));
    let tb = locked("tarball", None, None, None);
    assert!(remote_url(&tb).is_none());
    let mut git = locked("git", None, None, None);
    git.url = Some("https://example.com/repo.git?ref=main".into());
    assert_eq!(
        remote_url(&git).as_deref(),
        Some("https://example.com/repo.git")
    );
}

#[test]
fn parses_branch_ls_remote() {
    let out = "abc123def456abc123def456abc123def456abc1\trefs/heads/main\n";
    let h = parse_ls_remote(out, Some("main")).unwrap();
    assert_eq!(h.sha, "abc123def456abc123def456abc123def456abc1");
    assert_eq!(h.branch, "main");
}

#[test]
fn parses_symref_ls_remote() {
    let out = "ref: refs/heads/main\tHEAD\n\
                   abc123def456abc123def456abc123def456abc1\tHEAD\n";
    let h = parse_ls_remote(out, None).unwrap();
    assert_eq!(h.sha, "abc123def456abc123def456abc123def456abc1");
    assert_eq!(h.branch, "main");
}

#[test]
fn skips_peeled_tag_lines() {
    let out = "aaa\trefs/tags/v1.0.0\nbbb\trefs/tags/v1.0.0^{}\n";
    let heads = ls_remote_heads(out);
    assert_eq!(
        heads,
        vec![("aaa".to_string(), "refs/tags/v1.0.0".to_string())]
    );
}

#[test]
fn missing_branch_errors() {
    let out = "abc\trefs/heads/main\n";
    assert!(parse_ls_remote(out, Some("dev")).is_err());
    assert!(parse_ls_remote("", None).is_err());
}

#[test]
fn heads_match_prefix_lenient() {
    assert!(heads_match("abc123def456", "abc123def456"));
    assert!(heads_match("abc123def456abc", "abc123d"));
    assert!(!heads_match("abc123", "def456"));
    assert!(!heads_match("", "abc"));
}

#[test]
fn compare_urls() {
    let gh = locked(
        "github",
        Some("o"),
        Some("r"),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
    );
    let head = UpstreamHead {
        sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        branch: "main".into(),
    };
    assert_eq!(
        compare_url(&gh, &head).as_deref(),
        Some("https://github.com/o/r/compare/aaaaaaa...bbbbbbb")
    );
    let tb = locked("tarball", None, None, None);
    assert!(compare_url(&tb, &head).is_none());
}

#[test]
fn age_formatting() {
    assert_eq!(format_age(100, 90), "just now");
    assert_eq!(format_age(1000, 1000 - 90), "1 minute ago");
    assert_eq!(format_age(10000, 10000 - 5 * 3600), "5 hours ago");
    assert_eq!(format_age(1_000_000, 1_000_000 - 45 * 86400), "1 month ago");
}

#[test]
fn only_rev_pinned_inputs_are_checked() {
    // No network in tests: assert classification stops before I/O for
    // non-pinned / non-git inputs (status Skipped, no head).
    let json = r#"{
            "root": "root",
            "nodes": {
                "root": { "inputs": { "pinned": "pinned", "floating": "floating", "tb": "tb" } },
                "pinned": {
                    "locked": { "type": "tarball", "url": "https://x/t.tgz", "narHash": "sha256-" },
                    "original": { "type": "github", "owner": "o", "repo": "r", "rev": "abc" }
                },
                "floating": {
                    "locked": { "type": "github", "owner": "o", "repo": "r", "rev": "abc" },
                    "original": { "type": "github", "owner": "o", "repo": "r", "ref": "main" }
                },
                "tb": {
                    "locked": { "type": "tarball", "url": "https://x/t.tgz", "narHash": "sha256-" },
                    "original": { "type": "tarball", "url": "https://x/t.tgz" }
                }
            },
            "version": 7
        }"#;
    let g = parse_str(json).unwrap();
    let rows = check_top_level(&g);
    // Sorted: all Skipped (rank 3), alphabetical.
    assert_eq!(rows.len(), 3);
    let by_name: std::collections::HashMap<_, _> =
        rows.iter().map(|r| (r.name.as_str(), &r.status)).collect();
    assert!(matches!(by_name["floating"], InputStatus::Skipped(_)));
    assert!(matches!(by_name["tb"], InputStatus::Skipped(_)));
    // Rev-pinned but non-git remote → skipped without network.
    assert!(matches!(by_name["pinned"], InputStatus::Skipped(_)));
    assert!(rows.iter().all(|r| r.head.is_none()));
}

#[test]
fn bump_rewrites_pin_in_flake_nix() {
    let dir = std::env::temp_dir().join(format!(
        "nixgrep-bump-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let old = "9d4ed44d8b8cecdceb1d6fd76e74123d90ae6339";
    let new = "9aa07bb0256d300219b30622d2454e85f7f3667e";
    std::fs::write(
        dir.join("flake.nix"),
        format!("{{\n  inputs.naersk.url = \"github:nix-community/naersk/{old}\";\n}}\n"),
    )
    .unwrap();
    let count = bump_rev_in_flake_nix(&dir, "naersk", old, new).unwrap();
    assert_eq!(count, 1);
    let after = std::fs::read_to_string(dir.join("flake.nix")).unwrap();
    assert!(after.contains(new));
    assert!(!after.contains(old));
    // Unknown rev → clean error, file untouched.
    assert!(bump_rev_in_flake_nix(&dir, "naersk", old, new).is_err());
}

#[test]
fn bump_scoped_to_input_name() {
    let dir = std::env::temp_dir().join(format!(
        "nixgrep-bump-scope-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let shared = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let new_a = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let new_b = "cccccccccccccccccccccccccccccccccccccccc";
    std::fs::write(
        dir.join("flake.nix"),
        format!(
            "{{\n  inputs.a.url = \"github:o/a/{shared}\";\n  inputs.b.url = \"github:o/b/{shared}\";\n}}\n"
        ),
    )
    .unwrap();
    // Bumping `a` must not touch `b`'s identical pin.
    let n = bump_rev_in_flake_nix(&dir, "a", shared, new_a).unwrap();
    assert_eq!(n, 1);
    let after = std::fs::read_to_string(dir.join("flake.nix")).unwrap();
    assert!(after.contains(new_a));
    assert!(after.contains(shared), "b's pin must remain");
    // Now bumping `b` works independently.
    let n = bump_rev_in_flake_nix(&dir, "b", shared, new_b).unwrap();
    assert_eq!(n, 1);
    let after = std::fs::read_to_string(dir.join("flake.nix")).unwrap();
    assert!(!after.contains(shared));
}

#[test]
fn bump_refuses_ambiguous_global_replace() {
    let dir = std::env::temp_dir().join(format!(
        "nixgrep-bump-ambig-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let shared = "dddddddddddddddddddddddddddddddddddddddd";
    std::fs::write(
        dir.join("flake.nix"),
        format!("{{\n  x = \"{shared}\";\n  y = \"{shared}\";\n}}\n"),
    )
    .unwrap();
    // No input name matches → global replace with 2 hits must refuse.
    assert!(bump_rev_in_flake_nix(&dir, "nope", shared, "eeee").is_err());
}

#[test]
fn git_plus_prefix_stripped() {
    let mut g = locked("git", None, None, None);
    g.url = Some("git+https://example.com/repo.git?ref=main".into());
    assert_eq!(
        remote_url(&g).as_deref(),
        Some("https://example.com/repo.git")
    );
}
