use super::*;

#[test]
fn splits_position_with_line() {
    let (f, l) = split_position("/a/b.nix:42").unwrap();
    assert_eq!(f, PathBuf::from("/a/b.nix"));
    assert_eq!(l, Some(42));
}

#[test]
fn splits_position_without_line() {
    let (f, l) = split_position("/a/b.nix").unwrap();
    assert_eq!(f, PathBuf::from("/a/b.nix"));
    assert_eq!(l, None);
}

#[test]
fn finds_attr_dot_hash() {
    let args = vec!["eval".into(), ".#foo.bar".into(), "--json".into()];
    assert_eq!(find_attr(&args), Some(".#foo.bar".into()));
}

#[test]
fn finds_attr_explicit() {
    let args = vec!["eval".into(), "github:me/c#foo".into()];
    assert_eq!(find_attr(&args), Some("github:me/c#foo".into()));
}

#[test]
fn rewrites_bare_attr_to_flake_prefix() {
    assert_eq!(
        build_target(
            "nixosConfigurations.nixos.config.systemPackages",
            "/home/me/cfg"
        ),
        "/home/me/cfg#nixosConfigurations.nixos.config.systemPackages",
    );
}

#[test]
fn rewrites_dot_hash_attr() {
    assert_eq!(
        build_target(".#foo.bar", "/home/me/cfg"),
        "/home/me/cfg#foo.bar",
    );
}

#[test]
fn leaves_explicit_flake_ref_alone() {
    assert_eq!(
        build_target("github:me/c#foo", "/home/me/cfg"),
        "github:me/c#foo",
    );
}
