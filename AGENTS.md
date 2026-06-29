# AGENTS.md

Compact repo-specific guidance for OpenCode sessions. Read this before editing.

## What this is

`nixgrep` is a single-crate Rust CLI (edition 2021) that attributes Nix evaluation warnings to the flake input that emits them. It shells out to `nix` at runtime (`nix flake archive --json --dry-run`, `nix build --dry-run`, `nix eval`) — so integration runs require `nix` on PATH, but unit tests don't.

## Commands

```sh
nix develop              # dev shell (fenix toolchain, rust-analyzer, RUST_SRC_PATH set)
cargo test --bin nixgrep # unit tests live in src/main.rs #[cfg(test)]
cargo build              # plain cargo build works; nix build uses naersk
nix build .#default      # build via flake
nix run .# -- scan .     # run locally
nix fmt                  # treefmt (nixfmt + rustfmt) — formatter is in flake.nix
nix flake check          # builds package + runs treefmt check
```

No separate lint command — `nix flake check` and `cargo test` are the verification steps.

## Build system (do not regress)

- **naersk**, not crane. `packages.default = naerskLib.buildPackage { src = ./.; };` — no `cargoHash`, no `cargoArtifacts`. Do not reintroduce crane or manual hash maintenance.
- **fenix** provides the Rust toolchain via `overlays.default` as `rustToolchain`. Do not switch to `nixpkgs.rustPlatform` or pin a nixpkgs-frozen rustc.
- **treefmt-nix** drives both `formatter` and `checks.treefmt`. The `evalModule` config is duplicated in both outputs — keep them in sync when adding formatters.
- Inputs are pinned via FlakeHub (`https://flakehub.com/f/...`) and all `follows = "nixpkgs"`. Keep that pattern for new inputs.

## Critical: flake.lock must be committed

If you change `flake.nix` inputs and push without regenerating `flake.lock`, `nix run github:aniviaflome/nixgrep` fails with `cannot write modified lock file of flake 'github:...'`. Always run `nix flake lock` and commit `flake.lock` together with any `flake.nix` input change.

## Dependencies

No native deps — `Cargo.toml` has no `openssl-sys`/`pkg-config`/system-lib crates. Do not add `openssl` or `pkg-config` to the flake's `buildInputs` or devShell.

## Source map

All code is in `src/`, single binary, no sub-crates:

- `main.rs` — CLI (clap), subcommands `locate` / `trigger` / `why` / `scan`, output formatting, unit tests
- `scan.rs` — runs `nix build --dry-run`, captures warnings, auto-detects capture/trigger targets
- `archive.rs` — runs `nix flake archive --json --dry-run`, builds the input → store-path tree
- `search.rs` — greps the input closure for the warning text (Mode A / emitter)
- `meta.rs` — reads `meta.position` from derivations on an eval target (Mode B / trigger)
- `lock.rs` — parses `flake.lock` to build source URLs (GitHub/GitLab/Codeberg/SourceHut)
- `message.rs` — normalizes warning text (strips `evaluation warning:`, ANSI, etc.)
- `map.rs` — shared attribution helpers

The two attribution modes (A: grep for emitter, B: `meta.position` for trigger) are the core abstraction; `why` and `scan` just compose them.

## Conventions

- `AGENTS.md`, `README.md`, and `flake.nix` description should stay coherent — the README documents the user-facing subcommands.
- `.envrc` is `use flake`; `.direnv/` is gitignored.
- `supportedSystems` is `x86_64-linux`, `aarch64-linux`, `aarch64-darwin`. Adding `aarch64-darwin` is fine but requires all three inputs to build there.