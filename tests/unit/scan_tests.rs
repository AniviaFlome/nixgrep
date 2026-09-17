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
fn throw_message_not_captured_without_marker() {
    let s = "error: thrown error: package XYZ is broken\n";
    assert!(extract_eval_warnings(s).is_empty());
}

#[test]
fn throw_multiline_not_captured_without_marker() {
    let s = "error:\n       … while calling the 'import' builtin\n       at foo.nix:1:1:\n\n       error: my thrown message\n";
    assert!(extract_eval_warnings(s).is_empty());
}

#[test]
fn throw_captured_with_explicit_throw_marker() {
    let s = "error:\n       … while calling 'checkInPkgs'\n       … caused by explicit throw\n         at /nix/store/abc-source/pkgs/top-level/aliases.nix:456:22:\n         error: Go 1.25 is end-of-life, and 'buildGo125Module' has been removed.\n";
    let w = extract_eval_warnings(s);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].kind, DiagnosticKind::Throw);
    assert_eq!(
        w[0].message,
        "Go 1.25 is end-of-life, and 'buildGo125Module' has been removed."
    );
}

#[test]
fn throw_marker_does_not_capture_abort_or_assert_twice() {
    let s = "… caused by explicit throw\nerror: evaluation aborted with the following error message: 'nope'\nerror: assertion failed\n";
    let w = extract_eval_warnings(s);
    assert_eq!(w.len(), 2);
    assert_eq!(w[0].kind, DiagnosticKind::Abort);
    assert_eq!(w[1].kind, DiagnosticKind::Assert);
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
fn captures_plain_warning_for_builtins_warn() {
    let s = "warning: stdenv.isDarwin is deprecated, use stdenv.hostPlatform.isDarwin instead\n";
    let w = extract_eval_warnings(s);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].kind, DiagnosticKind::Warning);
    assert_eq!(
        w[0].message,
        "stdenv.isDarwin is deprecated, use stdenv.hostPlatform.isDarwin instead"
    );
}

#[test]
fn captures_plain_warning_is_linux() {
    let s = "warning: stdenv.isLinux is deprecated, use stdenv.hostPlatform.isLinux instead\n";
    let w = extract_eval_warnings(s);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].kind, DiagnosticKind::Warning);
}

#[test]
fn filters_nix_tool_download_warning() {
    let s = "warning: unable to download 'https://x.cachix.org/foo.narinfo'; retrying\n";
    assert!(extract_eval_warnings(s).is_empty());
}

#[test]
fn keeps_evaluation_warning_and_plain_warning_distinct() {
    let s = "evaluation warning: foo is deprecated\nwarning: bar is deprecated\n";
    let w = extract_eval_warnings(s);
    assert_eq!(w.len(), 2);
    assert_eq!(w[0].message, "foo is deprecated");
    assert_eq!(w[1].message, "bar is deprecated");
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
    assert_eq!(DiagnosticKind::Throw.label(), "throw");
}
