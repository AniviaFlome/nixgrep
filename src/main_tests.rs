use super::*;

#[test]
fn extracts_host_from_nixos_target() {
    assert_eq!(
        extract_host_from_target("nixosConfigurations.nixos.config.system.build.toplevel",),
        Some("nixos".to_string()),
    );
}

#[test]
fn extracts_host_from_other_host() {
    assert_eq!(
        extract_host_from_target("nixosConfigurations.vps.config.system.build.toplevel"),
        Some("vps".to_string()),
    );
}

#[test]
fn returns_none_for_non_nixos_target() {
    assert_eq!(extract_host_from_target("homeConfigurations.foo"), None);
}

#[test]
fn correlated_nixpkgs_for_input() {
    assert_eq!(
        correlated_nixpkgs("input catppuccin"),
        "input catppuccin.nixpkgs",
    );
}

#[test]
fn correlated_nixpkgs_for_your_config() {
    assert_eq!(correlated_nixpkgs("your config"), "");
}

#[test]
fn correlated_nixpkgs_for_unknown() {
    assert_eq!(correlated_nixpkgs("unknown"), "");
}

#[test]
fn trigger_priority_orders_your_config_first() {
    assert_eq!(trigger_priority("your config"), 0);
    assert_eq!(trigger_priority("input catppuccin"), 1);
    assert_eq!(trigger_priority("input nixpkgs"), 2);
    assert_eq!(trigger_priority("input catppuccin.nixpkgs"), 2);
    assert_eq!(trigger_priority("unknown"), 3);
}

#[test]
fn nixpkgs_internal_top_level() {
    assert!(is_nixpkgs_internal("input nixpkgs"));
}

#[test]
fn nixpkgs_internal_nested() {
    assert!(is_nixpkgs_internal("input catppuccin.nixpkgs"));
}

#[test]
fn nixpkgs_internal_stable() {
    assert!(is_nixpkgs_internal("input nixpkgs-stable"));
    assert!(is_nixpkgs_internal("input foo.nixpkgs-stable"));
}

#[test]
fn not_nixpkgs_internal_for_consumer() {
    assert!(!is_nixpkgs_internal("input catppuccin"));
    assert!(!is_nixpkgs_internal("your config"));
    assert!(!is_nixpkgs_internal("unknown"));
}

#[test]
fn emitter_internal_drops_coincident_file() {
    use std::path::Path;
    let hit = search::Hit {
        attribution: "input nixpkgs".into(),
        file: Path::new("/nix/store/abc-source/pkgs/development/tools/pnpm/generic.nix")
            .to_path_buf(),
        line_no: 28,
        line: r#"lib.warn "pnpm: Override nodejs-slim instead of nodejs" nodejs;"#.into(),
    };
    let t = meta::MetaTrigger {
        name: "pnpm-10.29.2".into(),
        file: Path::new("/nix/store/abc-source/pkgs/development/tools/pnpm/generic.nix")
            .to_path_buf(),
        line: Some(28),
        attribution: "input nixpkgs".into(),
    };
    assert!(is_emitter_internal(&t, std::slice::from_ref(&hit)));
}

#[test]
fn emitter_internal_keeps_distinct_file() {
    use std::path::Path;
    let hit = search::Hit {
        attribution: "input nixpkgs".into(),
        file: Path::new("/nix/store/abc-source/pkgs/development/tools/pnpm/generic.nix")
            .to_path_buf(),
        line_no: 28,
        line: r#"lib.warn "pnpm: Override nodejs-slim instead of nodejs" nodejs;"#.into(),
    };
    let t = meta::MetaTrigger {
        name: "vesktop-1.6.5".into(),
        file: Path::new("/nix/store/abc-source/pkgs/by-name/ve/vesktop/package.nix").to_path_buf(),
        line: Some(1),
        attribution: "input nixpkgs".into(),
    };
    assert!(!is_emitter_internal(&t, std::slice::from_ref(&hit)));
}

#[test]
fn warned_package_name_from_emitter_path() {
    use std::path::Path;
    let hit = search::Hit {
        attribution: "input nixpkgs".into(),
        file: Path::new("/nix/store/abc-source/pkgs/development/tools/pnpm/generic.nix")
            .to_path_buf(),
        line_no: 28,
        line: String::new(),
    };
    assert_eq!(
        warned_package_name(std::slice::from_ref(&hit)),
        Some("pnpm".into())
    );
}

#[test]
fn warned_package_name_from_by_name_path() {
    use std::path::Path;
    let hit = search::Hit {
        attribution: "input nixpkgs".into(),
        file: Path::new("/nix/store/abc-source/pkgs/by-name/ve/vesktop/package.nix").to_path_buf(),
        line_no: 1,
        line: String::new(),
    };
    assert_eq!(
        warned_package_name(std::slice::from_ref(&hit)),
        Some("vesktop".into()),
    );
}

#[test]
fn warned_package_name_none_when_no_hits() {
    assert_eq!(warned_package_name(&[]), None);
}

#[test]
fn is_relevant_trigger_keeps_when_no_emitter_hits() {
    use std::path::Path;
    let t = meta::MetaTrigger {
        name: "foo".into(),
        file: Path::new("/tmp/nonexistent-xyz/foo.nix").to_path_buf(),
        line: None,
        attribution: "input nixpkgs".into(),
    };
    assert!(is_relevant_trigger(&t, &[]));
}

#[test]
fn is_relevant_trigger_keeps_when_file_mentions_package() {
    use std::path::Path;
    let dir = std::env::temp_dir().join(format!(
        "nixgrep-rel-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("package.nix");
    std::fs::write(&file, "buildInputs = [ pnpm ];\n").unwrap();
    let hit = search::Hit {
        attribution: "input nixpkgs".into(),
        file: Path::new("/nix/store/abc-source/pkgs/development/tools/pnpm/generic.nix")
            .to_path_buf(),
        line_no: 28,
        line: String::new(),
    };
    let t = meta::MetaTrigger {
        name: "my-pkg".into(),
        file: file.clone(),
        line: Some(1),
        attribution: "input nixpkgs".into(),
    };
    assert!(is_relevant_trigger(&t, std::slice::from_ref(&hit)));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn is_relevant_trigger_drops_when_file_does_not_mention_package() {
    use std::path::Path;
    let dir = std::env::temp_dir().join(format!(
        "nixgrep-rel-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("package.nix");
    std::fs::write(&file, "buildInputs = [ cmake ];\n").unwrap();
    let hit = search::Hit {
        attribution: "input nixpkgs".into(),
        file: Path::new("/nix/store/abc-source/pkgs/development/tools/pnpm/generic.nix")
            .to_path_buf(),
        line_no: 28,
        line: String::new(),
    };
    let t = meta::MetaTrigger {
        name: "my-pkg".into(),
        file: file.clone(),
        line: Some(1),
        attribution: "input nixpkgs".into(),
    };
    assert!(!is_relevant_trigger(&t, std::slice::from_ref(&hit)));
    std::fs::remove_dir_all(&dir).ok();
}
