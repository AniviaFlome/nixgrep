//! `update` command: check top-level flake inputs pinned to a commit
//! (`rev = "..."` in `flake.nix`) for upstream updates.
//!
//! For each rev-pinned input we resolve its git remote, ask for the current
//! HEAD of the tracked branch with `git ls-remote` (no hosting API, no
//! tokens), and compare it with the locked rev. Branch-following and
//! non-git (tarball/path/…) inputs are reported as skipped.

use std::process::Command;

use crate::lock::{LockGraph, LockedRef};

/// Upstream HEAD as reported by `git ls-remote`.
#[derive(Debug, Clone)]
pub struct UpstreamHead {
    /// Full upstream SHA.
    pub sha: String,
    /// Branch the SHA belongs to (requested ref, or resolved default branch).
    pub branch: String,
}

/// Check result for one top-level input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputStatus {
    UpToDate,
    Outdated,
    /// Not checked: branch-following or non-git input.
    Skipped(String),
    /// Checked but inconclusive (network/auth/`git` failure).
    Unknown(String),
}

impl InputStatus {
    pub fn label(&self) -> &'static str {
        match self {
            InputStatus::UpToDate => "up-to-date",
            InputStatus::Outdated => "outdated",
            InputStatus::Skipped(_) => "skipped",
            InputStatus::Unknown(_) => "unknown",
        }
    }

    /// Sort rank for display: actionable first.
    fn rank(&self) -> u8 {
        match self {
            InputStatus::Outdated => 0,
            InputStatus::Unknown(_) => 1,
            InputStatus::UpToDate => 2,
            InputStatus::Skipped(_) => 3,
        }
    }
}

/// One row of the `update` report.
#[derive(Debug, Clone)]
pub struct InputUpdate {
    pub name: String,
    pub status: InputStatus,
    pub locked_rev: Option<String>,
    pub locked_date: Option<i64>,
    pub head: Option<UpstreamHead>,
    pub compare_url: Option<String>,
}

/// Check every top-level input of the lock graph. Only rev-pinned git inputs
/// hit the network; everything else is classified without I/O.
pub fn check_top_level(graph: &LockGraph) -> Vec<InputUpdate> {
    let mut rows: Vec<InputUpdate> = graph
        .top_level
        .iter()
        .map(|name| check_one(graph, name))
        .collect();
    rows.sort_by(|a, b| {
        a.status
            .rank()
            .cmp(&b.status.rank())
            .then_with(|| a.name.cmp(&b.name))
    });
    rows
}

fn check_one(graph: &LockGraph, name: &str) -> InputUpdate {
    let locked = graph.inputs.get(name);
    let original = graph.originals.get(name);
    let locked_rev = locked.and_then(|l| l.rev.clone());
    let locked_date = locked.and_then(|l| l.last_modified);

    let mut row = InputUpdate {
        name: name.to_string(),
        status: InputStatus::UpToDate,
        locked_rev,
        locked_date,
        head: None,
        compare_url: None,
    };

    let locked = match locked {
        Some(l) => l,
        None => {
            row.status = InputStatus::Unknown("no locked ref in flake.lock".into());
            return row;
        }
    };

    // Only rev-pinned inputs are in scope.
    let pinned = original.and_then(|o| o.rev.as_deref()).is_some();
    if !pinned {
        row.status = InputStatus::Skipped("not pinned to a commit (tracks a branch)".into());
        return row;
    }

    let Some(url) = remote_url(locked) else {
        row.status = InputStatus::Skipped(format!("not a git input ({})", locked.ref_type));
        return row;
    };

    // A pinned input may still name a branch (`rev` + `ref`); otherwise Nix
    // tracks the default branch, which we resolve via `--symref`.
    let branch = original.and_then(|o| o.ref_name.clone());
    let head = match ls_remote_head(&url, branch.as_deref()) {
        Ok(h) => h,
        Err(e) => {
            row.status = InputStatus::Unknown(format!("git ls-remote failed: {e}"));
            return row;
        }
    };

    row.head = Some(head.clone());
    let locked_sha = locked.rev.as_deref().unwrap_or_default();
    row.status = if heads_match(locked_sha, &head.sha) {
        InputStatus::UpToDate
    } else {
        // Only outdated rows get a compare link; a self-compare would be noise.
        row.compare_url = compare_url(locked, &head);
        InputStatus::Outdated
    };
    row
}

/// Two SHAs match when they share a full-SHA prefix (a lock rev is always a
/// full SHA; be lenient towards short upstream output anyway).
fn heads_match(locked_rev: &str, head_sha: &str) -> bool {
    !locked_rev.is_empty()
        && !head_sha.is_empty()
        && (head_sha.starts_with(locked_rev) || locked_rev.starts_with(head_sha))
}

/// HTTPS git remote for `git ls-remote`, or None for non-git input types.
pub fn remote_url(locked: &LockedRef) -> Option<String> {
    match locked.ref_type.as_str() {
        "github" => Some(format!(
            "https://github.com/{}/{}",
            locked.owner.as_deref()?,
            locked.repo.as_deref()?,
        )),
        "gitlab" => Some(format!(
            "https://{}/{}/{}",
            locked.host.as_deref().unwrap_or("gitlab.com"),
            locked.owner.as_deref()?,
            locked.repo.as_deref()?,
        )),
        "sourcehut" => Some(format!(
            "https://git.sr.ht/~{}/{}",
            locked.owner.as_deref()?,
            locked.repo.as_deref()?,
        )),
        "codeberg" => Some(format!(
            "https://codeberg.org/{}/{}",
            locked.owner.as_deref()?,
            locked.repo.as_deref()?,
        )),
        "git" => locked.url.as_deref().map(|u| {
            // Locked `git` URLs may carry a `git+` scheme prefix and/or a
            // `?ref=…&rev=…` query suffix; neither is valid for `git ls-remote`.
            let u = u.strip_prefix("git+").unwrap_or(u);
            u.split('?').next().unwrap_or(u).to_string()
        }),
        _ => None,
    }
}

/// Ask the remote for the HEAD SHA. With `branch = None` the default branch
/// is resolved via `--symref`. A `--` separator guards against URLs that
/// begin with `-`, and the call is bounded by a 60s timeout so a hung
/// network can't wedge `update` forever.
pub fn ls_remote_head(url: &str, branch: Option<&str>) -> anyhow::Result<UpstreamHead> {
    let mut cmd = Command::new("git");
    cmd.args(["ls-remote"]);
    match branch {
        Some(b) => {
            cmd.arg("--");
            cmd.arg(url);
            cmd.arg(format!("refs/heads/{b}"));
        }
        None => {
            cmd.args(["--symref", "--", url, "HEAD"]);
        }
    }
    let output = run_with_timeout(cmd, std::time::Duration::from_secs(60))
        .map_err(|e| anyhow::anyhow!("failed to run `git ls-remote`: {e} (is git installed?)"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{}", stderr.trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_ls_remote(&stdout, branch)
}

/// Run `cmd` to completion, killing the child if it exceeds `timeout`.
fn run_with_timeout(
    mut cmd: Command,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(_) => return child.wait_with_output(),
            None if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("command timed out after {}s", timeout.as_secs()),
                ));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
}

/// Parse `git ls-remote` output into an [`UpstreamHead`]. Skips peeled
/// (`^{}`) lines from annotated tags. `branch` is the requested ref, if any.
fn parse_ls_remote(output: &str, branch: Option<&str>) -> anyhow::Result<UpstreamHead> {
    let heads = ls_remote_heads(output);
    if let Some(want) = branch {
        return heads
            .into_iter()
            .find(|(_, r)| r == &format!("refs/heads/{want}"))
            .map(|(sha, _)| UpstreamHead {
                sha,
                branch: want.to_string(),
            })
            .ok_or_else(|| anyhow::anyhow!("branch `{want}` not found on remote"));
    }
    // Default-branch query: the symref target arrives as a pseudo-line
    // `ref: refs/heads/<branch>\tHEAD` ahead of the real `HEAD` SHA
    // (order-independent here, to be safe).
    let mut symref_branch: Option<String> = None;
    let mut head_sha: Option<String> = None;
    for (sha, r) in &heads {
        if r != "HEAD" {
            continue;
        }
        if let Some(target) = sha.strip_prefix("ref: ") {
            symref_branch = target
                .strip_prefix("refs/heads/")
                .map(str::to_string)
                .or_else(|| Some(target.to_string()));
        } else if !sha.is_empty() {
            head_sha = Some(sha.clone());
        }
    }
    match head_sha {
        Some(sha) => Ok(UpstreamHead {
            sha,
            branch: symref_branch.unwrap_or_else(|| "HEAD".to_string()),
        }),
        None => anyhow::bail!("no HEAD found in `git ls-remote` output"),
    }
}

/// Split `git ls-remote` output into `(sha, ref)` pairs, dropping peeled
/// `^{}` lines (annotated tags) so callers never mistake a peeled commit
/// for the ref itself.
fn ls_remote_heads(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(_, r)| !r.ends_with("^{}"))
        .map(|(sha, r)| (sha.to_string(), r.to_string()))
        .collect()
}

/// Web URL comparing the locked rev against upstream HEAD, if the host has
/// a compare view.
pub fn compare_url(locked: &LockedRef, head: &UpstreamHead) -> Option<String> {
    let locked_rev = locked.rev.as_deref()?;
    let (from, to) = (short_rev(locked_rev), short_rev(&head.sha));
    match locked.ref_type.as_str() {
        "github" => Some(format!(
            "https://github.com/{}/{}/compare/{from}...{to}",
            locked.owner.as_deref()?,
            locked.repo.as_deref()?,
        )),
        "gitlab" => Some(format!(
            "https://{}/{}/{}/-/compare/{from}...{to}",
            locked.host.as_deref().unwrap_or("gitlab.com"),
            locked.owner.as_deref()?,
            locked.repo.as_deref()?,
        )),
        "codeberg" => Some(format!(
            "https://codeberg.org/{}/{}/compare/{from}...{to}",
            locked.owner.as_deref()?,
            locked.repo.as_deref()?,
        )),
        "sourcehut" => Some(format!(
            "https://git.sr.ht/~{}/{}/log/{}",
            locked.owner.as_deref()?,
            locked.repo.as_deref()?,
            head.branch,
        )),
        _ => None,
    }
}

/// First 7 chars of a rev for display.
pub fn short_rev(rev: &str) -> &str {
    rev.get(..7).unwrap_or(rev)
}

/// Human age like "3 days ago" between two unix timestamps (pure for tests).
pub fn format_age(now: i64, then: i64) -> String {
    let secs = (now - then).max(0);
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    if secs < MIN {
        "just now".to_string()
    } else if secs < HOUR {
        let m = secs / MIN;
        format!("{m} minute{} ago", plural(m))
    } else if secs < DAY {
        let h = secs / HOUR;
        format!("{h} hour{} ago", plural(h))
    } else if secs < 30 * DAY {
        let d = secs / DAY;
        format!("{d} day{} ago", plural(d))
    } else if secs < 365 * DAY {
        let mo = secs / (30 * DAY);
        format!("{mo} month{} ago", plural(mo))
    } else {
        let y = secs / (365 * DAY);
        format!("{y} year{} ago", plural(y))
    }
}

fn plural(n: i64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A rev-pinned input with a known upstream HEAD: everything needed to bump
/// the pin in `flake.nix`.
#[derive(Debug, Clone)]
pub struct Bump {
    pub name: String,
    pub old_rev: String,
    pub new_rev: String,
}

/// Bumps for every outdated row (rows carry their locked rev + head SHA).
pub fn bumps(rows: &[InputUpdate]) -> Vec<Bump> {
    rows.iter()
        .filter(|r| r.status == InputStatus::Outdated)
        .filter_map(|r| match (&r.locked_rev, &r.head) {
            (Some(old), Some(head)) => Some(Bump {
                name: r.name.clone(),
                old_rev: old.clone(),
                new_rev: head.sha.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Replace the pinned rev with the new HEAD SHA in `<flake_dir>/flake.nix`,
/// scoped to the given input so two inputs sharing one SHA can't corrupt
/// each other. Returns the number of replacements.
///
/// Scoping: prefer replacing `old_rev` only on lines that mention the input
/// name (the common `inputs.<name>.url = "...<rev>"` single-line form). If no
/// such line exists (e.g. multi-line input block), fall back to a global
/// replace only when the rev occurs exactly once in the whole file; otherwise
/// bail and ask the user to edit by hand. A zero count means the pin isn't a
/// literal in `flake.nix` (exotic URL construction).
pub fn bump_rev_in_flake_nix(
    flake_dir: &std::path::Path,
    input_name: &str,
    old_rev: &str,
    new_rev: &str,
) -> anyhow::Result<usize> {
    bump_rev_for_input(flake_dir, Some(input_name), old_rev, new_rev)
}

/// Unscoped variant kept for tests / callers without an input name.
/// Behaves like the old global replace but refuses to touch the file when
/// the rev occurs more than once (ambiguous — could belong to several inputs).
#[allow(dead_code)]
fn bump_rev_global(
    flake_dir: &std::path::Path,
    old_rev: &str,
    new_rev: &str,
) -> anyhow::Result<usize> {
    bump_rev_for_input(flake_dir, None, old_rev, new_rev)
}

fn bump_rev_for_input(
    flake_dir: &std::path::Path,
    input_name: Option<&str>,
    old_rev: &str,
    new_rev: &str,
) -> anyhow::Result<usize> {
    let path = flake_dir.join("flake.nix");
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    // Scoped pass: only lines mentioning the input stanza
    // (`inputs.<name>`). Matching the bare name over-matches
    // (`github` contains `a`); the stanza prefix is precise for the common
    // single-line `inputs.<name>.url = "...<rev>"` form.
    if let Some(name) = input_name {
        let stanza = format!("inputs.{name}");
        let mut scoped = 0usize;
        let mut out_lines: Vec<String> = Vec::new();
        for line in contents.lines() {
            if line.contains(&stanza) && line.contains(old_rev) {
                scoped += line.matches(old_rev).count();
                out_lines.push(line.replace(old_rev, new_rev));
            } else {
                out_lines.push(line.to_string());
            }
        }
        if scoped > 0 {
            let mut out = out_lines.join("\n");
            if contents.ends_with('\n') {
                out.push('\n');
            }
            std::fs::write(&path, out)
                .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
            return Ok(scoped);
        }
    }
    let total = contents.matches(old_rev).count();
    if total == 0 {
        anyhow::bail!(
            "pinned rev {old_rev} not found literally in {}",
            path.display()
        );
    }
    if total > 1 {
        anyhow::bail!(
            "pinned rev {old_rev} occurs {total} times in {} — refusing global replace (specify the input stanza by hand)",
            path.display()
        );
    }
    std::fs::write(&path, contents.replace(old_rev, new_rev))
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
    Ok(total)
}

/// Re-lock the flake after bumping pins. Plain `nix flake lock` only
/// re-resolves inputs whose declaration changed, so untouched inputs stay
/// put. The flake ref is omitted for the local `.` checkout.
pub fn relock(flake: &str) -> anyhow::Result<String> {
    let mut cmd = Command::new("nix");
    cmd.args(["flake", "lock"]);
    if !flake.is_empty() && flake != "." {
        cmd.arg(flake);
    }
    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run `nix flake lock`: {e}"))?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        anyhow::bail!("`nix flake lock` failed:\n{}", combined.trim());
    }
    Ok(combined.trim().to_string())
}

#[cfg(test)]
#[path = "../tests/unit/update_tests.rs"]
mod tests;
