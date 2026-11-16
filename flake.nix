{
  description = "Pinpoint which flake input causes a given nixpkgs evaluation warning/trace";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1";
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
            pkgs = import inputs.nixpkgs { inherit system; };
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
      overlays.default = final: prev: {
        nixgrep = self.packages.${prev.stdenv.hostPlatform.system}.default;
      };

      devShells = forEachSupportedSystem (
        { pkgs, ... }:
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              clippy
              rustfmt
              rust-analyzer
            ];

            env = {
              # Required by rust-analyzer
              RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
            };
          };
        }
      );

      packages = forEachSupportedSystem (
        { pkgs, ... }:
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "nixgrep";
            version = "0.1.1";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            meta = {
              description = "Pinpoint which flake input causes a given nixpkgs evaluation warning/trace";
              license = pkgs.lib.licenses.mit;
              mainProgram = "nixgrep";
            };
          };
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
