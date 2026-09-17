use super::*;
use std::path::Path;

#[test]
fn display_path_multi_dash_name() {
    let p = Path::new("/nix/store/abc123-nixfmt-classic-source/pkgs/foo.nix");
    assert_eq!(display_path(p), "nixfmt-classic/pkgs/foo.nix");
}

#[test]
fn display_path_no_source_suffix() {
    let p = Path::new("/nix/store/abc123-some-package/lib/x.nix");
    assert_eq!(display_path(p), "some-package/lib/x.nix");
}

#[test]
fn display_path_non_store() {
    let p = Path::new("/home/me/cfg/flake.nix");
    assert_eq!(display_path(p), "/home/me/cfg/flake.nix");
}
