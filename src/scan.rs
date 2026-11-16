//! `scan` command: run a dry-run build of the flake, capture every evaluation
//! warning/trace line from the build output, and hand each to Mode A (locate)
//! — and optionally Mode B (trigger) — for attribution.
//!
//! This is the "just build my config and tell me which input causes each
//! warning" entry point: no message arg needed.

use std::process::Command;

use regex::Regex;

/// The kind of Nix evaluation diagnostic captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticKind {
    /// `trace: evaluation warning: <msg>` / `evaluation warning: <msg>`
    /// (from `lib.warn` / `builtins.warn`).
    Warning,
    /// `trace: <msg>` (from `builtins.trace` / `lib.trace`).
    Trace,
    /// `error: evaluation aborted with the following error message: '<msg>'`
    /// (from `abort`).
    Abort,
    /// `error: assertion failed` (from a failed `assert`).
    Assert,
    /// `error: <msg>` alongside a `caused by explicit throw` marker
    /// (from `builtins.throw` / `throw` in nixpkgs, e.g. removed-builder
    /// aliases like `buildGo125Module`).
    Throw,
}

impl DiagnosticKind {
    pub fn label(self) -> &'static str {
        match self {
            DiagnosticKind::Warning => "warning",
            DiagnosticKind::Trace => "trace",
            DiagnosticKind::Abort => "abort",
            DiagnosticKind::Assert => "assert",
            DiagnosticKind::Throw => "throw",
        }
    }
}

/// A captured diagnostic line from `nix build --dry-run` output.
#[derive(Debug, Clone)]
pub struct CapturedWarning {
    /// The raw line as it appeared on stderr (with Nix prefix + ANSI).
    #[allow(dead_code)]
    pub raw: String,
    /// The message with Nix prefixes / ANSI stripped.
    pub message: String,
    /// Which diagnostic form produced this line.
    pub kind: DiagnosticKind,
}

/// Run `nix build <flake>#<target> --dry-run --keep-going --no-eval-cache`
/// and return every evaluation-warning/trace line from stderr.
///
/// `--no-eval-cache` forces re-evaluation so warnings are re-emitted even on
/// repeat runs (Nix's eval-cache otherwise skips re-eval and swallows the
/// `lib.warn`/`builtins.trace` output).
///
/// If `target` is None, nixgrep auto-detects a capture target by inspecting the
/// flake's `nixosConfigurations` / `homeConfigurations` and picking the entry
/// matching the current hostname (preferring a full system toplevel, which
/// forces the widest evaluation and surfaces the most warnings). If the flake
/// ref itself carries a `#<host>` (e.g. `/path#nixos`), that host is used as a
/// hint. If nothing matches, it falls back to the bare flake.
pub fn capture_warnings(flake: &str, target: Option<&str>) -> anyhow::Result<Vec<CapturedWarning>> {
    let (base_flake, host_hint) = split_flake_ref(flake);

    let attr = match target {
        Some(t) => {
            if t == "." {
                t.to_string()
            } else if let Some(rest) = t.strip_prefix(".#") {
                format!("{base_flake}#{rest}")
            } else if t.contains('#') {
                t.to_string()
            } else {
                format!("{base_flake}#{t}")
            }
        }
        None => match default_capture_target(&base_flake, host_hint.as_deref()) {
            Some(t) => format!("{base_flake}#{t}"),
            None => base_flake.clone(),
        },
    };

    let mut cmd = Command::new("nix");
    cmd.args([
        "build",
        &attr,
        "--dry-run",
        "--keep-going",
        "--no-eval-cache",
    ]);
    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run `nix build --dry-run`: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    Ok(extract_eval_warnings(&stderr))
}

/// Split a flake ref into its base (no `#...`) and an optional host hint.
/// `/path#nixos`       → (`/path`, Some("nixos"))
/// `github:me/c#foo`    → (`github:me/c`, Some("foo"))
/// `.#nixos`           → (`.`, Some("nixos"))
/// `/path`             → (`/path`, None)
pub fn split_flake_ref(flake: &str) -> (String, Option<String>) {
    match flake.split_once('#') {
        Some((base, attr)) => (base.to_string(), Some(attr.to_string())),
        None => (flake.to_string(), None),
    }
}

/// Auto-detect a capture target for the flake by listing `nixosConfigurations`
/// and `homeConfigurations` and picking the entry matching the current
/// hostname (or `host_hint` if given). Returns an attr path relative to the
/// flake (no `#` prefix), e.g.
/// `nixosConfigurations.nixos.config.system.build.toplevel`.
pub fn default_capture_target(flake: &str, host_hint: Option<&str>) -> Option<String> {
    let hostname = current_hostname().or_else(|| host_hint.map(str::to_string));
    let target_attr =
        |host: &str| format!("nixosConfigurations.{host}.config.system.build.toplevel");

    let nixos_hosts = list_attr(flake, "nixosConfigurations").ok();
    if let Some(hosts) = &nixos_hosts {
        if let Some(h) = pick_host(hosts, hostname.as_deref()) {
            return Some(target_attr(&h));
        }
    }
    if let Ok(hosts) = list_attr(flake, "homeConfigurations") {
        if let Some(h) = pick_host(&hosts, hostname.as_deref()) {
            return Some(format!("homeConfigurations.{h}.activationPackage"));
        }
    }
    if let Some(hosts) = nixos_hosts {
        if hosts.len() == 1 {
            return Some(target_attr(&hosts[0]));
        }
    }
    None
}

/// Auto-detect a trigger target for Mode B (meta.position attribution).
///
/// Given a capture host (e.g. `nixos`), probe common attribute paths that
/// evaluate to lists/attrsets of derivations, returning the first that exists:
///   1. `nixosConfigurations.<host>.config.home-manager.users.<user>.programs.<editor>.profiles.default.extensions`
///      (for each home-manager user × editor) — editor extension lists are
///      where warnings like pnpm's commonly originate.
///   2. `nixosConfigurations.<host>.config.home-manager.users.<user>.home.packages`
///   3. `nixosConfigurations.<host>.config.environment.systemPackages`
///
/// Returns an attr path relative to the flake (no `#`).
///
/// Prefer [`default_trigger_targets`] (plural) for `scan`, which returns *all*
/// existing candidate paths so every consumer of a warned package can be
/// attributed, not just the first one found.
#[allow(dead_code)]
pub fn default_trigger_target(flake: &str, host: &str) -> Option<String> {
    default_trigger_targets(flake, host).into_iter().next()
}

/// Auto-detect *all* candidate trigger targets for Mode B.
///
/// Returns every attribute path from the probe list that exists in the flake,
/// in priority order (editor extension lists first, then `home.packages`,
/// then `environment.systemPackages`). Each path is relative to the flake
/// (no `#`). See [`default_trigger_target`] for the single-target variant.
///
/// This runs a *single* `nix eval` whose `--apply` expression enumerates the
/// home-manager users, builds every candidate path, and filters by existence
/// with a pure total `hasPath` helper — instead of one `nix eval` per
/// candidate (each of which pays full evaluation + flake-fetch overhead).
pub fn default_trigger_targets(flake: &str, host: &str) -> Vec<String> {
    let cfg = format!("nixosConfigurations.{host}.config");
    match probe_trigger_targets(flake, &cfg) {
        Ok(paths) => paths.into_iter().map(|p| format!("{cfg}.{p}")).collect(),
        Err(_) => {
            // The batched probe failed (e.g. the config attr itself doesn't
            // evaluate) — fall back to just systemPackages so a broken
            // home-manager subtree doesn't wipe out every trigger.
            let sys = format!("{cfg}.environment.systemPackages");
            if attr_exists(flake, &sys) {
                vec![sys]
            } else {
                Vec::new()
            }
        }
    }
}

/// Run one `nix eval` returning the existing trigger-candidate sub-paths
/// (relative to `cfg_attr`, e.g. `nixosConfigurations.nixos.config`) as a
/// JSON string array, in priority order.
fn probe_trigger_targets(flake: &str, cfg_attr: &str) -> anyhow::Result<Vec<String>> {
    // NOTE: keep in sync with the candidate list the old per-path probing
    // used: per user × per editor × 2 shapes, then home.packages, then
    // environment.systemPackages last.
    const PROBE: &str = r#"
cfg:
let
  hasPath = obj: path:
    if path == [] then true
    else builtins.isAttrs obj
      && builtins.hasAttr (builtins.head path) obj
      && hasPath (builtins.getAttr (builtins.head path) obj) (builtins.tail path);
  users = builtins.attrNames (cfg.home-manager.users or {});
  editors = ["vscode" "vscodium" "vscode-fhs" "cursor"];
  perUser = u: builtins.concatLists (builtins.map (ed: [
    ["home-manager" "users" u "programs" ed "profiles" "default" "extensions"]
    ["home-manager" "users" u "programs" ed "extensions"]
  ]) editors) ++ [[ "home-manager" "users" u "home" "packages" ]];
  cands = builtins.concatLists (builtins.map perUser users) ++ [[ "environment" "systemPackages" ]];
in builtins.map (p: builtins.concatStringsSep "." p) (builtins.filter (p: hasPath cfg p) cands)
"#;
    let target = format!("{flake}#{cfg_attr}");
    let output = Command::new("nix")
        .args(["eval", &target, "--apply", PROBE, "--json"])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run `nix eval {target}`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`nix eval {target}` failed: {}", stderr.trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<Vec<String>>(stdout.trim())
        .map_err(|e| anyhow::anyhow!("failed to parse `{target}` JSON: {e}"))
}

/// Return true if `flake#<attr>` evaluates without error.
fn attr_exists(flake: &str, attr: &str) -> bool {
    let target = format!("{flake}#{attr}");
    Command::new("nix")
        .args(["eval", &target, "--apply", "x: builtins.length x"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Best-effort current hostname: `$HOSTNAME` env var (if exported), else
/// `/etc/hostname`, else the `hostname` command.
fn current_hostname() -> Option<String> {
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
        let h = s.trim();
        if !h.is_empty() {
            return Some(h.to_string());
        }
    }
    if let Ok(out) = Command::new("hostname").output() {
        if out.status.success() {
            let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !h.is_empty() {
                return Some(h);
            }
        }
    }
    None
}

/// List the attribute names of a top-level flake output (e.g.
/// `nixosConfigurations`) as a `Vec<String>`.
fn list_attr(flake: &str, attr: &str) -> anyhow::Result<Vec<String>> {
    let target = format!("{flake}#{attr}");
    let output = Command::new("nix")
        .args([
            "eval",
            &target,
            "--apply",
            "x: builtins.attrNames x",
            "--json",
        ])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run `nix eval {target}`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`nix eval {target}` failed: {}", stderr.trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<Vec<String>>(stdout.trim())
        .map_err(|e| anyhow::anyhow!("failed to parse `{target}` JSON: {e}"))
}

/// Pick a host name from `hosts` matching `hostname`, else the first.
fn pick_host(hosts: &[String], hostname: Option<&str>) -> Option<String> {
    if let Some(h) = hostname {
        if hosts.iter().any(|x| x == h) {
            return Some(h.to_string());
        }
    }
    hosts.first().cloned()
}

/// Extract evaluation diagnostics from `nix` stderr.
///
/// Captures every diagnostic whose message text lives in `.nix` source (so it
/// can be grepped), across the Nix evaluation forms that have a *fixed prefix*:
///
/// | form              | stderr line                                            | kind    |
/// |-------------------|--------------------------------------------------------|---------|
/// | `lib.warn` (via `builtins.trace`) | `trace: evaluation warning: <msg>` (or `evaluation warning: <msg>`) | Warning  |
/// | `builtins.warn` (newer Nix)       | `warning: <msg>`                          | Warning  |
/// | `builtins.trace`/`lib.trace` | `trace: <msg>`                                  | Trace   |
/// | `abort`           | `error: evaluation aborted with the following error message: '<msg>'` | Abort |
/// | `assert` (failed) | `error: assertion failed`                             | Assert  |
/// | `throw`           | `error: <msg>` *with* a `caused by explicit throw` marker elsewhere in the output | Throw |
///
/// `builtins.throw` prints `error: <msg>` with no fixed prefix, so its message
/// is indistinguishable from a generic Nix-tool `error:` line on its own. We
/// only capture it when the output also contains an explicit-throw marker
/// (`caused by explicit throw` / `explicit throw`), which Nix prints for
/// evaluation throws but not for tool errors. Bare `error:` lines without
/// that marker are still dropped. When in doubt, pass a throw message
/// explicitly to `nixgrep locate` to attribute it.
///
/// Plain `warning:` lines are captured too (newer Nix emits `builtins.warn` as
/// `warning: <msg>` without the `evaluation warning:` prefix), but we filter
/// out known Nix-the-tool warnings (substitute/download/git/cache operations)
/// via [`is_nix_tool_warning`] to avoid noise. Generic `error:` lines that
/// aren't one of the fixed-prefix forms above are also dropped.
fn extract_eval_warnings(stderr: &str) -> Vec<CapturedWarning> {
    static RE_WARN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_WARN_PLAIN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_TRACE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_ABORT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_ASSERT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_THROW: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

    let re_warn =
        RE_WARN.get_or_init(|| Regex::new(r"^(?:trace:\s*)?evaluation warning:\s*(.*)$").unwrap());
    let re_warn_plain = RE_WARN_PLAIN.get_or_init(|| Regex::new(r"^warning:\s*(.*)$").unwrap());
    let re_trace = RE_TRACE.get_or_init(|| Regex::new(r"^trace:\s+(.+)$").unwrap());
    let re_abort = RE_ABORT.get_or_init(|| {
        Regex::new(r"^\s*error:\s+evaluation aborted with the following error message:\s*'(.*)'$")
            .unwrap()
    });
    let re_assert =
        RE_ASSERT.get_or_init(|| Regex::new(r"^\s*error:\s+assertion failed\s*$").unwrap());
    let re_throw = RE_THROW.get_or_init(|| Regex::new(r"^\s*error:\s*(.+?)\s*$").unwrap());

    // `throw` has no fixed prefix of its own — gate generic `error:` capture
    // on Nix's explicit-throw marker so plain tool errors stay dropped.
    let has_throw = stderr.contains("explicit throw");

    let mut out = Vec::new();
    for line in stderr.lines() {
        let stripped = strip_ansi(line);
        if let Some(caps) = re_warn.captures(&stripped) {
            out.push(make(&stripped, caps.get(1), DiagnosticKind::Warning));
            continue;
        }
        if let Some(caps) = re_warn_plain.captures(&stripped) {
            let msg = caps.get(1).unwrap().as_str();
            if !is_nix_tool_warning(msg) {
                out.push(make(&stripped, caps.get(1), DiagnosticKind::Warning));
            }
            continue;
        }
        if let Some(caps) = re_trace.captures(&stripped) {
            out.push(make(&stripped, caps.get(1), DiagnosticKind::Trace));
            continue;
        }
        if let Some(caps) = re_abort.captures(&stripped) {
            out.push(make(&stripped, caps.get(1), DiagnosticKind::Abort));
            continue;
        }
        if re_assert.is_match(&stripped) {
            out.push(CapturedWarning {
                raw: stripped.clone(),
                message: "assertion failed".to_string(),
                kind: DiagnosticKind::Assert,
            });
            continue;
        }
        if has_throw {
            if let Some(caps) = re_throw.captures(&stripped) {
                let msg = caps.get(1).unwrap().as_str().trim();
                if !msg.is_empty()
                    && !msg.starts_with("evaluation aborted with the following error message:")
                    && msg != "assertion failed"
                {
                    out.push(make(&stripped, caps.get(1), DiagnosticKind::Throw));
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|w| seen.insert((w.kind, w.message.clone())));
    out
}

/// True for `warning:` lines that come from Nix-the-tool (the C++ client/
/// daemon) rather than from Nix evaluation (`builtins.warn` / `lib.warn`).
///
/// Newer Nix versions emit `builtins.warn` as plain `warning: <msg>` (without
/// the `evaluation warning:` prefix that `lib.warn` via `builtins.trace`
/// produces). We capture these plain `warning:` lines too, but filter out
/// known Nix-the-tool warnings (substitute/download/git/cache/file operations)
/// to avoid noise. This list is conservative — when in doubt we include the
/// warning so real evaluation warnings are never silently dropped.
fn is_nix_tool_warning(msg: &str) -> bool {
    let msg = msg.trim();
    const PREFIXES: &[&str] = &[
        "ignoring substitute",
        "unable to download",
        "found empty hash",
        "creating directory",
        "Git tree",
        "not committing",
        "you don't have permission",
        "writing manifest",
        "copying path",
        "downloading",
        "skipping",
        "updating",
        "renaming",
        "deleting",
        "experimental feature",
        "unknown setting",
        "deprecated setting",
        "flakes are deprecated",
    ];
    if PREFIXES.iter().any(|p| msg.starts_with(p)) {
        return true;
    }
    msg.contains("is not signed")
        || msg.contains("download speed")
        || (msg.starts_with("path ") && msg.contains("is not valid"))
        || (msg.starts_with("file ") && msg.contains("was modified"))
}

fn make(raw: &str, cap: Option<regex::Match<'_>>, kind: DiagnosticKind) -> CapturedWarning {
    let message = cap
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_default();
    CapturedWarning {
        raw: raw.to_string(),
        message,
        kind,
    }
}

fn strip_ansi(s: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").unwrap());
    re.replace_all(s, "").into_owned()
}

#[cfg(test)]
#[path = "../tests/unit/scan_tests.rs"]
mod tests;
