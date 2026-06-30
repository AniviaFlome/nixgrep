//! Normalizes a raw warning/trace line from Nix's stderr into the bare message
//! that actually appears in `.nix` source, and picks a good grep needle.

use regex::Regex;

/// A Nix-prefixed message with the prefix stripped and ANSI escapes removed,
/// plus the needle we'll search for in source trees.
#[derive(Debug, Clone)]
pub struct Normalized {
    /// The full message with Nix prefixes / ANSI removed (what the user typed,
    /// cleaned).
    #[allow(dead_code)]
    pub message: String,
    /// The substring we'll actually grep for. Usually the whole cleaned
    /// message unless it looks like it contains interpolation, in which case
    /// we pick the longest literal run.
    pub needle: String,
    /// True when the needle is only a fragment of the message (i.e. we
    /// heuristically think the message is dynamically constructed).
    pub partial: bool,
}

impl Normalized {
    pub fn is_empty(&self) -> bool {
        self.needle.trim().is_empty()
    }
}

/// Strip ANSI CSI sequences and the Nix-added prefixes
/// (`evaluation warning:`, `warning:`, `trace:`) from a raw stderr line.
///
/// Nix itself adds these prefixes in its C++ logger; the `.nix` source only
/// contains the bare message, so we must remove the prefix before grepping.
pub fn normalize(raw: &str) -> Normalized {
    let cleaned = strip_ansi(raw);
    let stripped = strip_nix_prefix(&cleaned);
    let (needle, partial) = pick_needle(&stripped);
    Normalized {
        message: stripped,
        needle,
        partial,
    }
}

/// Remove ANSI/CSI escape sequences.
fn strip_ansi(s: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").unwrap());
    re.replace_all(s, "").into_owned()
}

fn strip_nix_prefix(s: &str) -> String {
    let mut trimmed = s.trim_start();
    loop {
        let mut stripped = false;
        for p in ["evaluation warning:", "warning:", "trace:"] {
            if let Some(rest) = trimmed.strip_prefix(p) {
                trimmed = rest.trim_start();
                stripped = true;
                break;
            }
        }
        if !stripped {
            return trimmed.to_string();
        }
    }
}

/// Heuristically pick the best literal needle for grepping source.
///
/// Many `lib.warn`/`builtins.warn` messages are plain string literals and the
/// whole message appears verbatim in `.nix` source. Some are built by
/// concatenation/interpolation (e.g. `lib.warn "${name}: is deprecated" x`),
/// where only the literal fragments outside the interpolation are
/// grep-friendly. We strip `${...}` interpolations and keep the longest
/// remaining literal run.
fn pick_needle(msg: &str) -> (String, bool) {
    static INTERP: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = INTERP.get_or_init(|| Regex::new(r"\$\{[^}]*\}").unwrap());

    let msg = msg.trim();
    if msg.is_empty() {
        return (String::new(), false);
    }

    if !re.is_match(msg) {
        return (msg.to_string(), false);
    }

    let stripped = re.replace_all(msg, "\x00");
    let longest = stripped
        .split('\x00')
        .map(|p| p.trim_start_matches('$').trim())
        .max_by_key(|p| p.len())
        .unwrap_or("")
        .to_string();

    let partial = !longest.is_empty();
    (longest, partial)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_evaluation_warning_prefix() {
        let n = normalize("evaluation warning: pnpm: Override nodejs-slim instead of nodejs");
        assert_eq!(n.message, "pnpm: Override nodejs-slim instead of nodejs");
        assert_eq!(n.needle, "pnpm: Override nodejs-slim instead of nodejs");
        assert!(!n.partial);
    }

    #[test]
    fn strips_trace_and_evaluation_warning_prefix() {
        let n =
            normalize("trace: evaluation warning: pnpm: Override nodejs-slim instead of nodejs");
        assert_eq!(n.message, "pnpm: Override nodejs-slim instead of nodejs");
        assert_eq!(n.needle, "pnpm: Override nodejs-slim instead of nodejs");
        assert!(!n.partial);
    }

    #[test]
    fn strips_warning_prefix() {
        let n = normalize("warning: foo is deprecated");
        assert_eq!(n.message, "foo is deprecated");
    }

    #[test]
    fn strips_trace_prefix() {
        let n = normalize("trace: hello world");
        assert_eq!(n.message, "hello world");
    }

    #[test]
    fn strips_ansi() {
        let n = normalize("\x1b[1;35mevaluation warning:\x1b[0m colorful");
        assert_eq!(n.message, "colorful");
    }

    #[test]
    fn picks_fragment_for_interpolated() {
        let n = normalize("evaluation warning: ${name}: is deprecated");
        assert_eq!(n.needle, ": is deprecated");
        assert!(n.partial);
    }

    #[test]
    fn handles_empty() {
        let n = normalize("evaluation warning:");
        assert!(n.is_empty());
    }
}
