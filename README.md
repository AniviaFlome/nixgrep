# nixgrep

Find which flake input causes a Nix evaluation warning or trace.

## Install

Try it without installing:

```sh
nix run github:AniviaFlome/nixgrep -- scan /path/to/your/nix-config
```

Install, as a flake input:

```nix
inputs.nixgrep.url = "github:AniviaFlome/nixgrep";
```

Via the overlay (exposes it as `pkgs.nixgrep`):

```nix
nixpkgs.overlays = [ inputs.nixgrep.overlays.default ];
environment.systemPackages = [ pkgs.nixgrep ];
```

Without the overlay, reference the package directly:

```nix
environment.systemPackages = [ inputs.nixgrep.packages.${stdenv.hostPlatform.system}.default ];
```

## Commands

| Command | What it does |
|---|---|
| `scan` | Capture every warning from `nix build --dry-run`. |
| `locate <msg>` | Find the `.nix` file + line that emits a known warning. |
| `trigger <msg> -- <nix args>` | Find which input's code triggers it via `meta.position`. |
| `why <msg>` | `locate` + find which input's code triggers it. |
| `update` | Check top-level inputs pinned to a commit for upstream updates (`--update` to apply). |

## Example

```
1.  [warning] pnpm: Override nodejs-slim instead of nodejs
   [trig] [input catppuccin] source/pkgs/vscode/package.nix:18
          https://github.com/catppuccin/nix/blob/3f3b351.../pkgs/vscode/package.nix#L18
          emitted by [input catppuccin.nixpkgs] source/pkgs/development/tools/pnpm/generic.nix:28
          lib.warn "pnpm: Override nodejs-slim instead of nodejs" nodejs;
```

## Development

```sh
nix develop
cargo test --bin nixgrep
nix fmt
```
