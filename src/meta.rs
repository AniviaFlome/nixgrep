//! Mode B (reliable): attribute a warning's *trigger* by reading
//! `meta.position` of the derivations on the user's eval target.
//!
//! On Lix / Nix < 2.23, `NIX_ABORT_ON_WARN` is unreliable for stack-trace
//! attribution because nixpkgs' `lib.warn` emulation aborts lazily (the abort
//! only fires when the warned value is forced, which most eval targets don't
//! do). Instead, we use `meta.position`: the derivation that *consumes* the
//! warning-emitting package (and thus triggers the warn) records where it was
//! defined. Mapping that file to a flake input tells the user whether the
//! trigger is in their config or in a dependency input.
//!
//! For the pnpm example: the warn is emitted by nixpkgs' `generic.nix`, but
//! *triggered* by `catppuccin-vscode`'s `package.nix` calling pnpm with the
//! default `nodejs`. `catppuccin-vscode.meta.position` points at the
//! `catppuccin` input's source — so the trigger is "input catppuccin", not
//! "your config".

use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

use crate::archive::ArchiveTree;
use crate::map::{Attribution, Classifier};
use crate::message::Normalized;

/// A located trigger site.
#[derive(Debug, Clone)]
pub struct MetaTrigger {
    /// The package name (derivation `name`).
    pub name: String,
    /// `meta.position` as a file path (line stripped).
    pub file: PathBuf,
    /// Line number from `meta.position` (`path:line`).
    pub line: Option<u64>,
    /// Human-readable attribution: "your config", "input <name>", or "unknown".
    pub attribution: String,
}

/// Evaluate `<target>` (and, if it's a list, each element) and collect
/// `meta.position` for every derivation found.
///
/// `nix_args` is the argv after `nix` (e.g. `["eval", ".#foo"]`). We run
/// `nix eval <target> --apply <probe>` where `<probe>` extracts
/// `{ name, position }` per derivation, handling both single derivations and
/// lists/attrsets.
pub fn collect(
    nix_args: &[String],
    flake: &str,
    _needle: &Normalized,
    tree: &ArchiveTree,
    project_root: Option<&std::path::Path>,
) -> anyhow::Result<Vec<MetaTrigger>> {
    if nix_args.is_empty() {
        anyhow::bail!(
            "no nix command given — pass one with `-- <nix args...>`, e.g. \
             `nixgrep trigger '...' -- eval .#myPackage`"
        );
    }

    let attr = find_attr(nix_args).ok_or_else(|| {
        anyhow::anyhow!(
            "couldn't find an attr path in `{} -- {}`; pass one like `.#foo`",
            "nix",
            nix_args.join(" ")
        )
    })?;

    let probe = probe_expr();
    let json = run_eval(&attr, flake, &probe)?;
    let probed: Vec<ProbedItem> = serde_json::from_str(&json)
        .map_err(|e| anyhow::anyhow!("failed to parse `nix eval` JSON: {e}"))?;

    let classifier = Classifier::new(project_root, tree);
    let mut triggers = Vec::new();
    for item in probed {
        if let Some(pos) = item.position {
            if let Some((file, line)) = split_position(&pos) {
                let attr = classifier.classify(&file);
                triggers.push(MetaTrigger {
                    name: item.name.unwrap_or_default(),
                    file,
                    line,
                    attribution: attribution_label(attr),
                });
            }
        }
    }
    Ok(triggers)
}

#[derive(Debug, Deserialize)]
struct ProbedItem {
    name: Option<String>,
    position: Option<String>,
}

/// Find the first `.#...` or `foo#...` positional in the nix argv.
fn find_attr(nix_args: &[String]) -> Option<String> {
    for a in nix_args.iter().skip(1) {
        if a == "--" {
            break;
        }
        if a.starts_with('-') {
            continue;
        }
        return Some(a.clone());
    }
    None
}

/// The Nix expression applied to the user's value to extract name+position.
///
/// It handles:
///   - a derivation (`{ __attr = "drv"; ... }`-ish) → `[{name, position}]`
///   - a list of derivations
///   - an attrset of derivations
///
/// Non-derivations are filtered out (null position/name).
fn probe_expr() -> String {
    r#"
x:
  let
    isDrv = x: x ? drvPath && x ? name;
    one = d: {
      name = d.name or null;
      position = d.meta.position or null;
    };
  in
  if builtins.isList x then
    builtins.map (y: if isDrv y then one y else { name = null; position = null; }) x
  else if builtins.isAttrs x && isDrv x then
    [ (one x) ]
  else if builtins.isAttrs x then
    builtins.map (y: if isDrv y then one y else { name = null; position = null; }) (builtins.attrValues x)
  else
    [ { name = null; position = null; } ]
"#
    .to_string()
}

fn run_eval(attr: &str, flake: &str, probe: &str) -> anyhow::Result<String> {
    let target = build_target(attr, flake);
    let args = vec![
        "eval".to_string(),
        target.clone(),
        "--apply".to_string(),
        probe.to_string(),
        "--json".to_string(),
    ];

    let output = Command::new("nix")
        .args(&args)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run `nix eval`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "`nix eval {} --apply ...` failed ({}):\n{}",
            target,
            output.status,
            stderr.trim()
        );
    }
    let s = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(s.trim().to_string())
}

/// Build a flake-relative target from an attr path. Bare attr paths (no
/// `#`/`:`/`/`) get prefixed with `<flake>#`; `.#...` becomes `<flake>#...`;
/// an explicit flake ref (contains `#` or `:` or is a path) is left alone.
fn build_target(attr: &str, flake: &str) -> String {
    if let Some(rest) = attr.strip_prefix(".#") {
        format!("{flake}#{rest}")
    } else if attr == "." {
        flake.to_string()
    } else if attr.contains('#') || attr.contains(':') || attr.starts_with('/') {
        attr.to_string()
    } else {
        format!("{flake}#{attr}")
    }
}

/// Split `meta.position` ("path/to/file.nix:line") into (file, Some(line)).
fn split_position(pos: &str) -> Option<(PathBuf, Option<u64>)> {
    if let Some((file, line)) = pos.rsplit_once(':') {
        if let Ok(n) = line.parse::<u64>() {
            return Some((PathBuf::from(file), Some(n)));
        }
    }
    Some((PathBuf::from(pos), None))
}

/// Convert a borrowed `Attribution` to an owned label string.
fn attribution_label(a: Attribution<'_>) -> String {
    match a {
        Attribution::YourConfig => "your config".to_string(),
        Attribution::Input(n) => format!("input {n}"),
        Attribution::Unknown => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
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
}
