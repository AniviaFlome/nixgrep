# AGENTS.md

Compact repo-specific guidance for OpenCode sessions. Read this before editing.

## What this is

`nixgrep` is a single-crate Rust CLI (edition 2021) that attributes Nix evaluation warnings to the flake input that emits them. It shells out at runtime (`nix flake archive --json --dry-run`, `nix build --dry-run`, `nix eval`, and `git ls-remote` / `nix flake lock` for the `update` command) — so integration runs require `nix`/`git` on PATH, but unit tests don't.

## Commands

```sh
nix develop --command cargo test --bin nixgrep <filter> # focused tests, e.g. `update::`
cargo test --bin nixgrep # all unit tests (in-module #[cfg(test)] across src/); needs dev shell for cargo
nix build .#default      # build via flake (slow: release LTO; iterate with cargo test instead)
nix run .# -- scan .     # run locally
nix fmt                  # treefmt (nixfmt + rustfmt)
nix flake check          # builds package + runs treefmt check
```

No separate lint command — `nix flake check` and `cargo test` are the verification steps. `cargo` is not on PATH outside `nix develop`.

## Gotchas

- **Nix builds only see git-tracked files.** After creating a new file (`src/*.rs`, workflows, etc.), `git add` it before `nix build`/`nix run`/`nix flake check`, or the sandbox fails with `file not found` while `cargo test` passes.
- **This machine runs Lix, CI runs CppNix.** `nix flake lock --update-input X` does not exist on Lix (it's `nix flake update X`); the `update` subcommand avoids per-input flags entirely for this reason. Prefer flavor-agnostic invocations (`nix flake lock`, `nix flake update`) in code and docs.
- **`nix flake update` is a no-op for rev-pinned inputs.** The pin lives in `flake.nix`, so `update --update` rewrites the SHA there and re-locks — never suggest bare `nix flake update` for a pinned input.

## Build system

- **`rustPlatform.buildRustPackage`** with `cargoLock.lockFile = ./Cargo.lock`
  (vendored via the `static.crates.io` CDN). This replaced naersk in 2026-09
  after naersk's `crates.io/api/.../download` fetcher started 403ing and its
  fenix toolchain doubled the `nix run` download. Do not reintroduce naersk,
  crane, or manual `cargoHash` maintenance.
- **nixpkgs toolchain for builds** (`cargo`, `rustc` from nixpkgs, already on
  `cache.nixos.org`). The devShell adds `clippy`, `rustfmt`, `rust-analyzer`.
  Do not reintroduce fenix — it pulled a full stable toolchain plus two copies
  of `rust-analyzer-src` into every `nix run` fetch.
- **treefmt-nix**: one shared `treefmtEval` feeds both `formatter` and
  `checks.formatting` — add programs there, not in the outputs.
- Inputs are pinned via FlakeHub (`https://flakehub.com/f/...`) and all
  `follows = "nixpkgs"`. Keep that pattern for new inputs.

## Critical: flake.lock must be committed

If you change `flake.nix` inputs and push without regenerating `flake.lock`, `nix run github:aniviaflome/nixgrep` fails with `cannot write modified lock file of flake 'github:...'`. Always run `nix flake lock` and commit `flake.lock` together with any `flake.nix` input change.

## Dependencies

No native deps — `Cargo.toml` has no `openssl-sys`/`pkg-config`/system-lib crates. Do not add `openssl` or `pkg-config` to the flake's `buildInputs` or devShell.

## Source map

All code is in `src/`, single binary, no sub-crates:

- `main.rs` — CLI (clap), subcommands `locate` / `trigger` / `why` / `scan` / `update`, output formatting, unit tests
- `scan.rs` — runs `nix build --dry-run`, captures warnings, auto-detects capture/trigger targets
- `archive.rs` — runs `nix flake archive --json --dry-run`, builds the input → store-path tree
- `search.rs` — std+regex substring search over the input closure (Mode A / emitter); root exempt from hidden-dir pruning
- `meta.rs` — reads `meta.position` from derivations on an eval target (Mode B / trigger)
- `update.rs` — checks rev-pinned top-level inputs against upstream HEAD via `git ls-remote`; `--update` bumps the SHA in `flake.nix` and re-locks
- `lock.rs` — parses `flake.lock` to build source URLs (GitHub/GitLab/Codeberg/SourceHut); also exposes `original` refs + top-level names for `update`
- `message.rs` — normalizes warning text (strips `evaluation warning:`, ANSI, etc.)
- `map.rs` — shared attribution helpers

The two attribution modes (A: grep for emitter, B: `meta.position` for trigger) are the core abstraction; `why` and `scan` just compose them.

## Conventions

- `AGENTS.md`, `README.md`, and `flake.nix` description should stay coherent — the README documents the user-facing subcommands.
- `.envrc` is `use flake`; `.direnv/` is gitignored.
