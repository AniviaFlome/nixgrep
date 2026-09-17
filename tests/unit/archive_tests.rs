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
    assert!(tree.inputs[0].1.as_os_str().len() >= tree.inputs.last().unwrap().1.as_os_str().len());
}
