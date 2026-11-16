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
/// (`evaluation warning:`, `warning:`, `trace:`, `error:`) from a raw stderr line.
///
/// Nix itself adds these prefixes in its C++ logger; the `.nix` source only
/// contains the bare message, so we must remove the prefix before grepping.
/// Also strips `nh`/terminal box-drawing wrappers (`┃`, `┏`, `┗`, `┣`, `━`)
/// so pasted `nh` output works, and the `evaluation aborted …: 'msg'`
/// wrapper plus surrounding quotes from `abort`/`throw` lines.
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
    let mut trimmed = strip_decorative_prefix(s);
    loop {
        let mut stripped = false;
        for p in [
            "evaluation warning:",
            "evaluation aborted with the following error message:",
            "warning:",
            "trace:",
            "error:",
        ] {
            if let Some(rest) = trimmed.strip_prefix(p) {
                trimmed = strip_decorative_prefix(rest);
                stripped = true;
                break;
            }
        }
        if !stripped {
            return strip_surrounding_quotes(trimmed);
        }
    }
}

/// Strip leading `nh`/box-drawing wrappers and whitespace so pasted
/// `nh os switch` output (`┃  error: …`, `┏━ …`) normalizes to the bare Nix
/// message.
fn strip_decorative_prefix(s: &str) -> &str {
    let mut t = s.trim_start();
    loop {
        // Only box-drawing / `nh` wrapper glyphs — never strip `-`, `#`, `*`
        // or `·`, which can legitimately start a warning message.
        let next = t.trim_start_matches([
            '┃', '┏', '┗', '┣', '┻', '┳', '━', '─', '│', '⚠', '∑', '⋮', '…', '›', '»',
        ]);
        let next = next.trim_start();
        if next.len() == t.len() {
            return t;
        }
        t = next;
    }
}

/// Strip one layer of surrounding single or double quotes, e.g. the `'msg'`
/// wrapper Nix adds around `abort` messages.
fn strip_surrounding_quotes(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2
        && ((t.starts_with('\'') && t.ends_with('\'')) || (t.starts_with('"') && t.ends_with('"')))
    {
        return t[1..t.len() - 1].trim().to_string();
    }
    t.to_string()
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
#[path = "../tests/unit/message_tests.rs"]
mod tests;
