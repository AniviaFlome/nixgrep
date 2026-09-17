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
