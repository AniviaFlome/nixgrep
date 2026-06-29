# nixgrep

Find which flake input causes a Nix evaluation warning or trace.

```sh
nix run github:AniviaFlome/nixgrep -- scan /path/to/your/nix-config
```

## Commands

| Command | What it does |
|---|---|
| `scan` | Capture every warning from `nix build --dry-run`. |
| `locate <msg>` | Find the `.nix` file + line that emits a known warning. |
| `why <msg>` | `locate` + find which input's code triggers it. |

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
