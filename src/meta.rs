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
    let extra = passthrough_flags(nix_args);
    let json = run_eval(&attr, flake, &probe, &extra)?;
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

/// Find the installable attr path in the nix argv, skipping flags and their
/// values (notably `--apply <expr>`, whose Nix expression must not be mistaken
/// for an attr). Prefers candidates that look like installables (contain
/// `#`, or start with `.`/`/`, or are bare attr paths without whitespace).
fn find_attr(nix_args: &[String]) -> Option<String> {
    // Flags taking a value: skip the value as well. `--apply` is handled by
    // run_eval itself, so its expression must never be treated as the target.
    const VALUE_FLAGS: &[&str] = &[
        "--apply",
        "--override-input",
        "--update-input",
        "--reference-lock-file",
        "--output-lock-file",
        "--log-format",
    ];
    let mut skip_next = 0usize;
    let mut fallback: Option<String> = None;
    for a in nix_args.iter().skip(1) {
        if skip_next > 0 {
            skip_next -= 1;
            continue;
        }
        if a == "--" {
            break;
        }
        if a.starts_with('-') {
            if a == "--override-input" {
                skip_next = 2;
            } else if VALUE_FLAGS.contains(&a.as_str()) {
                skip_next = 1;
            }
            continue;
        }
        // Skip Nix expressions: they contain whitespace/newlines or start
        // with `{`/`(` — installables never do.
        if a.contains('\n') || a.contains(' ') || a.starts_with('{') || a.starts_with('(') {
            continue;
        }
        // Prefer installable-looking candidates; keep the first plausible
        // bare attr as fallback.
        if a.contains('#') || a.starts_with('.') || a.starts_with('/') || a.contains(':') {
            return Some(a.clone());
        }
        if fallback.is_none() {
            fallback = Some(a.clone());
        }
    }
    fallback
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

fn run_eval(attr: &str, flake: &str, probe: &str, extra_args: &[String]) -> anyhow::Result<String> {
    let target = build_target(attr, flake);
    let mut args = vec![
        "eval".to_string(),
        target.clone(),
        "--apply".to_string(),
        probe.to_string(),
        "--json".to_string(),
    ];
    // Forward user flags (e.g. `--impure`) so impure flakes evaluate.
    // `--apply`/`--json` are managed above and never forwarded.
    for a in extra_args {
        if a == "--apply" || a == "--json" || a == "--raw" {
            continue;
        }
        args.push(a.clone());
    }

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

/// Flags to forward from the user's `nix` invocation to our internal
/// `nix eval` (e.g. `--impure`). The attr path, `--apply` expr, and output
/// format flags are managed by `run_eval` and excluded here.
fn passthrough_flags(nix_args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 1usize;
    while i < nix_args.len() {
        let a = &nix_args[i];
        if a == "--" {
            break;
        }
        if a == "--apply" {
            i += 2;
            continue;
        }
        if a == "--json" || a == "--raw" {
            i += 1;
            continue;
        }
        if a.starts_with('-') {
            out.push(a.clone());
            let take = if a == "--override-input" {
                2
            } else if matches!(
                a.as_str(),
                "--update-input"
                    | "--reference-lock-file"
                    | "--output-lock-file"
                    | "--log-format"
                    | "--option"
            ) {
                1
            } else {
                0
            };
            for _ in 0..take {
                if i + 1 < nix_args.len() && !nix_args[i + 1].starts_with('-') {
                    i += 1;
                    out.push(nix_args[i].clone());
                }
            }
        }
        i += 1;
    }
    out
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

/// Split `meta.position` ("path/to/file.nix:line" or "path:line:col") into
/// (file, Some(line)). A trailing `:col` is dropped; a non-numeric suffix
/// leaves the whole string as the file with no line.
fn split_position(pos: &str) -> Option<(PathBuf, Option<u64>)> {
    let parts: Vec<&str> = pos.rsplit(':').collect();
    // parts[0] is after the last ':'.
    if parts.len() >= 2 {
        if let Ok(line) = parts[1].parse::<u64>() {
            if let Ok(_col) = parts[0].parse::<u64>() {
                // file:line:col → drop col, keep line.
                let file: Vec<&str> = pos.split(':').collect();
                let file = file[..file.len() - 2].join(":");
                return Some((PathBuf::from(file), Some(line)));
            }
        }
    }
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
#[path = "../tests/unit/meta_tests.rs"]
mod tests;
