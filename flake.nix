{
  description = "A Nix-flake-based Rust development environment";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1";
    naersk = {
      url = "https://flakehub.com/f/nix-community/naersk/0.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    treefmt-nix = {
      url = "https://flakehub.com/f/numtide/treefmt-nix/0.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self, ... }@inputs:

    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forEachSupportedSystem =
        f:
        inputs.nixpkgs.lib.genAttrs supportedSystems (
          system:
          f {
            inherit system;
            pkgs = import inputs.nixpkgs {
              inherit system;
              overlays = [
                inputs.self.overlays.default
              ];
            };
          }
        );
      treefmtEval = forEachSupportedSystem (
        { pkgs, ... }:
        inputs.treefmt-nix.lib.evalModule pkgs {
          projectRootFile = "flake.nix";
          programs = {
            nixfmt.enable = true;
            rustfmt.enable = true;
          };
        }
      );
    in
    {
      devShells = forEachSupportedSystem (
        { pkgs, ... }:
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustToolchain
              rust-analyzer
            ];

            env = {
              # Required by rust-analyzer
              RUST_SRC_PATH = "${pkgs.rustToolchain}/lib/rustlib/src/rust/library";
            };
          };
        }
      );

      packages = forEachSupportedSystem (
        { system, ... }:
        let
          naerskLib = inputs.naersk.lib.${system};
        in
        {
          default = naerskLib.buildPackage { src = ./.; };
        }
      );

      apps = forEachSupportedSystem (
        { system, ... }:
        {
          default = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/nixgrep";
          };
        }
      );

      formatter = forEachSupportedSystem (
        { system, ... }:
        treefmtEval.${system}.config.build.wrapper
      );

      checks = forEachSupportedSystem (
        { system, ... }: {
          formatting = treefmtEval.${system}.config.build.check self;
          default = self.packages.${system}.default;
        }
      );
    };
}
