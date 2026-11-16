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

#[test]
fn hidden_named_root_is_still_searched() {
    // Regression test: the hidden-file filter must not prune the search
    // root itself. A root like `.` (the `--flake` default) or an
    // explicitly-passed hidden dir looks "hidden" by file name but must
    // still be searched; only hidden *children* are skipped.
    let tmp = std::env::temp_dir().join(format!(
        ".nixgrep-hidden-root-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&tmp).unwrap();
    fs::write(tmp.join("mod.nix"), "lib.warn \"hidden root probe\" x\n").unwrap();

    let tree = ArchiveTree {
        inputs: vec![],
        root_path: Some(tmp.clone()),
    };
    let needle = Normalized {
        message: "hidden root probe".into(),
        needle: "hidden root probe".into(),
        partial: false,
    };
    let hits = search(Some(&tmp), &tree, &needle, &SearchOpts::default()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].attribution, "your config");
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

fn norm(s: &str) -> Normalized {
    Normalized {
        message: s.into(),
        needle: s.into(),
        partial: false,
    }
}

#[test]
fn search_all_splits_hits_per_needle() {
    let tmp = tempfile_dir();
    fs::write(tmp.join("a.nix"), "throw \"alpha broke\";\n").unwrap();
    fs::write(tmp.join("b.nix"), "throw \"beta broke\";\n").unwrap();

    let tree = ArchiveTree {
        inputs: vec![],
        root_path: Some(tmp.clone()),
    };
    let out = search_all(
        Some(&tmp),
        &tree,
        &[norm("alpha broke"), norm("beta broke")],
        &SearchOpts::default(),
    )
    .unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].len(), 1);
    assert_eq!(out[1].len(), 1);
    assert!(out[0][0].file.ends_with("a.nix"));
    assert!(out[1][0].file.ends_with("b.nix"));
}

#[test]
fn search_all_matches_single_search() {
    // The batched walk must report the same hits as one walk per needle.
    let tmp = tempfile_dir();
    fs::write(
        tmp.join("a.nix"),
        "x = 1; # alpha\ny = 2; # alpha again\nz = 3; # beta\n",
    )
    .unwrap();

    let tree = ArchiveTree {
        inputs: vec![],
        root_path: Some(tmp.clone()),
    };
    let needles = vec![norm("alpha"), norm("beta")];
    let opts = SearchOpts::default();
    let batched = search_all(Some(&tmp), &tree, &needles, &opts).unwrap();
    for (i, n) in needles.iter().enumerate() {
        let single = search(Some(&tmp), &tree, n, &opts).unwrap();
        let (mut b, mut s): (Vec<_>, Vec<_>) = (
            batched[i].iter().map(|h| &h.file).collect(),
            single.iter().map(|h| &h.file).collect(),
        );
        b.sort();
        s.sort();
        assert_eq!(b, s, "needle {i} diverged");
        assert_eq!(batched[i].len(), single.len());
    }
}

#[test]
fn search_all_max_applies_per_needle() {
    let tmp = tempfile_dir();
    for i in 0..5 {
        fs::write(tmp.join(format!("f{i}.nix")), "needle here\n").unwrap();
    }

    let tree = ArchiveTree {
        inputs: vec![],
        root_path: Some(tmp.clone()),
    };
    let opts = SearchOpts {
        max_results: Some(2),
        ..Default::default()
    };
    let out = search_all(Some(&tmp), &tree, &[norm("needle")], &opts).unwrap();
    assert_eq!(out[0].len(), 2);
}

#[test]
fn search_all_rejects_empty_needle() {
    let tmp = tempfile_dir();
    let tree = ArchiveTree {
        inputs: vec![],
        root_path: Some(tmp.clone()),
    };
    let empty = Normalized {
        message: String::new(),
        needle: String::new(),
        partial: false,
    };
    assert!(
        search_all(
            Some(&tmp),
            &tree,
            &[norm("x"), empty],
            &SearchOpts::default()
        )
        .is_err()
    );
}
