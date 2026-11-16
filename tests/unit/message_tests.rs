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
    let n = normalize("trace: evaluation warning: pnpm: Override nodejs-slim instead of nodejs");
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
fn strips_error_prefix_for_throw() {
    let n = normalize("error: Go 1.25 is end-of-life, and 'buildGo125Module' has been removed.");
    assert_eq!(
        n.message,
        "Go 1.25 is end-of-life, and 'buildGo125Module' has been removed."
    );
    assert_eq!(
        n.needle,
        "Go 1.25 is end-of-life, and 'buildGo125Module' has been removed."
    );
}

#[test]
fn strips_nh_box_prefix() {
    let n = normalize(
        "┃        error: Go 1.25 is end-of-life, and 'buildGo125Module' has been removed.",
    );
    assert_eq!(
        n.message,
        "Go 1.25 is end-of-life, and 'buildGo125Module' has been removed."
    );
}

#[test]
fn strips_abort_wrapper_and_quotes() {
    let n =
        normalize("error: evaluation aborted with the following error message: 'my abort message'");
    assert_eq!(n.message, "my abort message");
}

#[test]
fn handles_empty() {
    let n = normalize("evaluation warning:");
    assert!(n.is_empty());
}

#[test]
fn preserves_leading_hash_in_message() {
    let n = normalize("# foo is deprecated");
    assert_eq!(n.message, "# foo is deprecated");
    assert_eq!(n.needle, "# foo is deprecated");
}
