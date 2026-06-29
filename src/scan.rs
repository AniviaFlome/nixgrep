//! `scan` command: run a dry-run build of the flake, capture every evaluation
//! warning/trace line from the build output, and hand each to Mode A (locate)
//! — and optionally Mode B (trigger) — for attribution.
//!
//! This is the "just build my config and tell me which input causes each
//! warning" entry point: no message arg needed.

use std::process::Command;

use regex::Regex;

/// The kind of Nix evaluation diagnostic captured.
///
/// Note: `builtins.throw` produces `error: <msg>` with *no fixed prefix* — the
/// message is arbitrary user text, indistinguishable from a generic Nix-tool
/// `error:` line. We therefore don't capture `throw` from build output; users
/// should pass the message explicitly to `nixgrep locate` instead.
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
}

impl DiagnosticKind {
    pub fn label(self) -> &'static str {
        match self {
            DiagnosticKind::Warning => "warning",
            DiagnosticKind::Trace => "trace",
            DiagnosticKind::Abort => "abort",
            DiagnosticKind::Assert => "assert",
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
            if t.contains('#') || t == "." {
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
pub fn default_trigger_target(flake: &str, host: &str) -> Option<String> {
    let cfg = format!("nixosConfigurations.{host}.config");
    let editors = ["vscode", "vscodium", "vscode-fhs", "cursor"];

    let hm_users = list_attr(flake, &format!("{cfg}.home-manager.users")).ok();

    if let Some(users) = &hm_users {
        for u in users {
            let hm = format!("{cfg}.home-manager.users.{u}");
            for ed in editors {
                let path = format!("{hm}.programs.{ed}.profiles.default.extensions");
                if attr_exists(flake, &path) {
                    return Some(path);
                }
                let path = format!("{hm}.programs.{ed}.extensions");
                if attr_exists(flake, &path) {
                    return Some(path);
                }
            }
        }
    }

    if let Some(users) = &hm_users {
        for u in users {
            let path = format!("{cfg}.home-manager.users.{u}.home.packages");
            if attr_exists(flake, &path) {
                return Some(path);
            }
        }
    }

    let sys = format!("{cfg}.environment.systemPackages");
    if attr_exists(flake, &sys) {
        return Some(sys);
    }

    None
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
/// | `lib.warn`/`builtins.warn` | `trace: evaluation warning: <msg>` (or `evaluation warning: <msg>` on newer Nix) | Warning  |
/// | `builtins.trace`/`lib.trace` | `trace: <msg>`                                  | Trace   |
/// | `abort`           | `error: evaluation aborted with the following error message: '<msg>'` | Abort |
/// | `assert` (failed) | `error: assertion failed`                             | Assert  |
///
/// `builtins.throw` is *not* captured: it prints `error: <msg>` with no fixed
/// prefix, so its message is indistinguishable from a generic Nix-tool `error:`
/// line. Pass a throw message explicitly to `nixgrep locate` to attribute it.
///
/// We deliberately drop plain `warning: ...` lines from Nix-the-tool (e.g.
/// substitute signature warnings) and generic `error:` lines that aren't one
/// of the fixed-prefix forms above.
fn extract_eval_warnings(stderr: &str) -> Vec<CapturedWarning> {
    static RE_WARN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_TRACE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_ABORT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_ASSERT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

    let re_warn =
        RE_WARN.get_or_init(|| Regex::new(r"^(?:trace:\s*)?evaluation warning:\s*(.+)$").unwrap());
    let re_trace = RE_TRACE.get_or_init(|| Regex::new(r"^trace:\s+(.+)$").unwrap());
    let re_abort = RE_ABORT.get_or_init(|| {
        Regex::new(r"^\s*error:\s+evaluation aborted with the following error message:\s*'(.*)'$")
            .unwrap()
    });
    let re_assert =
        RE_ASSERT.get_or_init(|| Regex::new(r"^\s*error:\s+assertion failed\s*$").unwrap());

    let mut out = Vec::new();
    for line in stderr.lines() {
        let stripped = strip_ansi(line);
        if let Some(caps) = re_warn.captures(&stripped) {
            out.push(make(&stripped, caps.get(1), DiagnosticKind::Warning));
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
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|w| seen.insert((w.kind, w.message.clone())));
    out
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
mod tests {
    use super::*;

    #[test]
    fn warning_with_trace_prefix() {
        let s = "trace: evaluation warning: pnpm: Override nodejs-slim instead of nodejs\n";
        let w = extract_eval_warnings(s);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].kind, DiagnosticKind::Warning);
        assert_eq!(w[0].message, "pnpm: Override nodejs-slim instead of nodejs");
    }

    #[test]
    fn warning_no_trace_prefix() {
        let s = "evaluation warning: foo is deprecated\n";
        let w = extract_eval_warnings(s);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].kind, DiagnosticKind::Warning);
        assert_eq!(w[0].message, "foo is deprecated");
    }

    #[test]
    fn warning_with_ansi() {
        let s = "\x1b[1;35mevaluation warning:\x1b[0m colorful\n";
        let w = extract_eval_warnings(s);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].kind, DiagnosticKind::Warning);
        assert_eq!(w[0].message, "colorful");
    }

    #[test]
    fn warning_empty_message_kept_as_empty() {
        let s = "trace: evaluation warning: \n";
        let w = extract_eval_warnings(s);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].kind, DiagnosticKind::Warning);
        assert_eq!(w[0].message, "");
    }

    #[test]
    fn trace_bare() {
        let s = "trace: building something\n";
        let w = extract_eval_warnings(s);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].kind, DiagnosticKind::Trace);
        assert_eq!(w[0].message, "building something");
    }

    #[test]
    fn trace_does_not_swallow_evaluation_warning() {
        let s = "trace: evaluation warning: real warn\ntrace: plain trace\n";
        let w = extract_eval_warnings(s);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].kind, DiagnosticKind::Warning);
        assert_eq!(w[0].message, "real warn");
        assert_eq!(w[1].kind, DiagnosticKind::Trace);
        assert_eq!(w[1].message, "plain trace");
    }

    #[test]
    fn throw_message_not_captured() {
        let s = "error: thrown error: package XYZ is broken\n";
        assert!(extract_eval_warnings(s).is_empty());
    }

    #[test]
    fn throw_multiline_not_captured() {
        let s = "error:\n       … while calling the 'import' builtin\n       at foo.nix:1:1:\n\n       error: my thrown message\n";
        assert!(extract_eval_warnings(s).is_empty());
    }

    #[test]
    fn abort_message() {
        let s = "error: evaluation aborted with the following error message: 'my abort message'\n";
        let w = extract_eval_warnings(s);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].kind, DiagnosticKind::Abort);
        assert_eq!(w[0].message, "my abort message");
    }

    #[test]
    fn abort_with_quotes_in_message() {
        let s = "error: evaluation aborted with the following error message: 'can't do \"this\"'\n";
        let w = extract_eval_warnings(s);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].kind, DiagnosticKind::Abort);
        assert_eq!(w[0].message, "can't do \"this\"");
    }

    #[test]
    fn assert_failure() {
        let s = "error: assertion failed\n";
        let w = extract_eval_warnings(s);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].kind, DiagnosticKind::Assert);
        assert_eq!(w[0].message, "assertion failed");
    }

    #[test]
    fn assert_failure_with_trailing_whitespace() {
        let s = "error: assertion failed   \n";
        let w = extract_eval_warnings(s);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].kind, DiagnosticKind::Assert);
    }

    #[test]
    fn ignores_nix_tool_substitute_warning() {
        let s = "warning: ignoring substitute for '/nix/store/abc' from 'https://x.cachix.org', as it's not signed\n";
        assert!(extract_eval_warnings(s).is_empty());
    }

    #[test]
    fn ignores_generic_error_lines() {
        let s = "error: unexpected fragment 'nixos' in flake reference\n";
        assert!(extract_eval_warnings(s).is_empty());
    }

    #[test]
    fn ignores_bare_error_word() {
        let s = "error:\n";
        assert!(extract_eval_warnings(s).is_empty());
    }

    #[test]
    fn ignores_trace_prefix_only() {
        let s = "trace:\n";
        assert!(extract_eval_warnings(s).is_empty());
    }

    #[test]
    fn mixed_output_classifies_all() {
        let s = "\
warning: ignoring substitute for '/nix/store/abc' from 'https://x', as it's not signed
trace: evaluation warning: pnpm: Override nodejs-slim instead of nodejs
trace: INFO: building foo
error: thrown error: pkg broken: missing dep
copying path '/nix/store/def' from 'https://cache.nixos.org'...
error: evaluation aborted with the following error message: 'nope'
error: assertion failed
error: some unrelated nix tool error
";
        let w = extract_eval_warnings(s);
        assert_eq!(w.len(), 4);
        assert_eq!(w[0].kind, DiagnosticKind::Warning);
        assert_eq!(w[0].message, "pnpm: Override nodejs-slim instead of nodejs");
        assert_eq!(w[1].kind, DiagnosticKind::Trace);
        assert_eq!(w[1].message, "INFO: building foo");
        assert_eq!(w[2].kind, DiagnosticKind::Abort);
        assert_eq!(w[2].message, "nope");
        assert_eq!(w[3].kind, DiagnosticKind::Assert);
    }

    #[test]
    fn dedupes_identical_kind_and_message() {
        let s = "trace: evaluation warning: same\ntrace: evaluation warning: same\ntrace: evaluation warning: other\n";
        let w = extract_eval_warnings(s);
        assert_eq!(w.len(), 2);
    }

    #[test]
    fn dedup_keeps_different_kinds_same_message() {
        let s = "trace: same text\ntrace: evaluation warning: same text\n";
        let w = extract_eval_warnings(s);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].kind, DiagnosticKind::Trace);
        assert_eq!(w[1].kind, DiagnosticKind::Warning);
    }

    #[test]
    fn splits_path_with_host() {
        let (base, host) = split_flake_ref("/home/me/cfg#nixos");
        assert_eq!(base, "/home/me/cfg");
        assert_eq!(host.as_deref(), Some("nixos"));
    }

    #[test]
    fn splits_github_ref() {
        let (base, host) = split_flake_ref("github:me/c#foo");
        assert_eq!(base, "github:me/c");
        assert_eq!(host.as_deref(), Some("foo"));
    }

    #[test]
    fn splits_dot_ref() {
        let (base, host) = split_flake_ref(".#nixos");
        assert_eq!(base, ".");
        assert_eq!(host.as_deref(), Some("nixos"));
    }

    #[test]
    fn splits_bare_path() {
        let (base, host) = split_flake_ref("/home/me/cfg");
        assert_eq!(base, "/home/me/cfg");
        assert!(host.is_none());
    }

    #[test]
    fn kind_labels() {
        assert_eq!(DiagnosticKind::Warning.label(), "warning");
        assert_eq!(DiagnosticKind::Trace.label(), "trace");
        assert_eq!(DiagnosticKind::Abort.label(), "abort");
        assert_eq!(DiagnosticKind::Assert.label(), "assert");
    }
}
