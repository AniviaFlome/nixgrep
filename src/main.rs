//! nixgrep — pinpoint which flake input causes a given nixpkgs evaluation
//! warning/trace.
//!
//! Two attribution modes:
//!   * `locate`  (Mode A): grep the flake-input closure for the literal warning
//!     string → finds the `.nix` file that *emits* the warning (e.g. inside
//!     nixpkgs).
//!   * `trigger` (Mode B): read `meta.position` of the derivations on an eval
//!     target → finds what in *your config* caused the warning to fire.
//!   * `why` runs both: locate the emitter, then locate the trigger.
//!   * `scan` builds the flake (dry-run), captures every warning nix prints,
//!     and attributes each one — no message argument required.

use std::io::{self, IsTerminal, Read};

use anyhow::Result;
use clap::{Parser, Subcommand};
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};

mod archive;
mod lock;
mod map;
mod message;
mod meta;
mod scan;
mod search;

#[derive(Parser, Debug)]
#[command(
    name = "nixgrep",
    version,
    about = "Pinpoint which flake input causes a given nixpkgs evaluation warning/trace.",
    long_about = "Given an evaluation warning/trace message (e.g. \
        'evaluation warning: pnpm: Override nodejs-slim instead of nodejs'), \
        nixgrep finds where it comes from — either a flake input's source \
        (locate) or the exact site in your own config that triggered it \
        (trigger). Use `scan` to build the flake and auto-attribute every \
        warning nix prints, with no message argument required."
)]
struct Cli {
    /// Flake ref to inspect (default: `.` — the current directory).
    #[arg(long, global = true, default_value = ".")]
    flake: String,

    /// Verbose output (prints the nix commands being run, archive tree size,
    /// etc.).
    #[arg(long, short = 'v', global = true)]
    verbose: bool,

    /// Color output: auto | always | never.
    #[arg(long, global = true, default_value = "auto")]
    color: String,

    /// Emit results as JSON (machine-readable).
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Mode A: locate the *emitter* of the warning by grepping the flake-input
    /// closure for the literal warning text.
    Locate(LocateArgs),
    /// Mode B: locate the *trigger* of the warning by reading `meta.position`
    /// of the derivations on an eval target.
    Trigger(TriggerArgs),
    /// Run both `locate` and `trigger`.
    Why(WhyArgs),
    /// Build the flake (dry-run), capture every evaluation warning/trace from
    /// the output, and attribute each one — to the emitting input and (if a
    /// target derivation is given) to the triggering input. No message arg
    /// needed; nixgrep reads the warnings straight off the build.
    Scan(ScanArgs),
}

#[derive(Parser, Debug)]
struct LocateArgs {
    /// The warning/trace message to search for. Reads from stdin if omitted
    /// and stdin is piped. Nix prefixes (`evaluation warning:`, `warning:`,
    /// `trace:`) and ANSI escapes are stripped automatically.
    message: Option<String>,

    /// Treat the search needle as a regex instead of a literal substring.
    #[arg(long)]
    regex: bool,

    /// Report *all* matches per file (default: first match per file).
    #[arg(long = "all")]
    all_matches: bool,

    /// Search all file types, not just `.nix` files.
    #[arg(long = "no-nix-only")]
    no_nix_only: bool,

    /// Stop after this many hits total.
    #[arg(long)]
    max: Option<usize>,
}

#[derive(Parser, Debug)]
struct TriggerArgs {
    /// The warning/trace message to search for. Reads from stdin if omitted.
    message: Option<String>,

    /// The `nix` subcommand + args to re-run (e.g. `eval .#foo --raw` or
    /// `build .#myPackage --no-link`). Pass after `--`.
    #[arg(last = true)]
    nix_args: Vec<String>,
}

#[derive(Parser, Debug)]
struct WhyArgs {
    /// The warning/trace message to search for. Reads from stdin if omitted.
    message: Option<String>,

    /// The `nix` subcommand + args to re-run for Mode B (trigger). Pass after
    /// `--`. If omitted, only `locate` is run.
    #[arg(last = true)]
    nix_args: Vec<String>,

    /// Inherit locate flags.
    #[arg(long = "regex")]
    regex: bool,
    #[arg(long = "all")]
    all_matches: bool,
    #[arg(long = "no-nix-only")]
    no_nix_only: bool,
    #[arg(long)]
    max: Option<usize>,
}

#[derive(Parser, Debug)]
struct ScanArgs {
    /// Flake ref to inspect (overrides the global `--flake`). A path, `.#`,
    /// `github:owner/repo`, etc. Defaults to the current directory.
    flake: Option<String>,

    /// The flake output attr to dry-run-build in order to *capture* evaluation
    /// warnings (default: the whole flake). Without a `#`, it's prefixed with
    /// the flake ref. Usually `nixosConfigurations.<host>.config.system.build.toplevel`.
    #[arg(long = "capture-target")]
    capture_target: Option<String>,

    /// Also run Mode B (trigger attribution) via `meta.position`. Pass the
    /// attr path that contains the warned derivations, e.g.
    /// `nixosConfigurations.nixos.config.home-manager.users.<user>.programs.vscode.profiles.default.extensions`.
    /// nixgrep will `nix eval` it and read each derivation's `meta.position`.
    #[arg(long = "trigger-target")]
    trigger_target: Option<String>,

    /// Treat locate needles as regex.
    #[arg(long = "regex")]
    regex: bool,
    /// Report all matches per file in locate.
    #[arg(long = "all")]
    all_matches: bool,
    /// Search all file types, not just `.nix`.
    #[arg(long = "no-nix-only")]
    no_nix_only: bool,
    /// Max locate hits per warning.
    #[arg(long)]
    max: Option<usize>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let color = match cli.color.as_str() {
        "always" => ColorChoice::Always,
        "never" => ColorChoice::Never,
        _ => ColorChoice::Auto,
    };
    let out = StandardStream::stdout(color);
    let mut out = out.lock();

    match cli.command {
        Command::Locate(ref args) => run_locate(&cli, args, &mut out),
        Command::Trigger(ref args) => run_trigger(&cli, args, &mut out),
        Command::Why(ref args) => run_why(&cli, args, &mut out),
        Command::Scan(ref args) => run_scan(&cli, args, &mut out),
    }
}

fn run_locate<W: WriteColor>(cli: &Cli, args: &LocateArgs, out: &mut W) -> Result<()> {
    let msg = read_message(args.message.as_deref())?;
    let norm = message::normalize(&msg);
    if norm.is_empty() {
        anyhow::bail!("empty message after normalization");
    }
    if cli.verbose {
        writeln_header(out, "nixgrep — locate (Mode A: grep closure for emitter)")?;
        writeln_dim(out, &format!("needle: {:?}", norm.needle))?;
        if norm.partial {
            writeln_dim(
                out,
                "note: message looks interpolated; searching for a literal fragment only",
            )?;
        }
    }

    let project_root = Some(std::path::Path::new(&cli.flake).to_path_buf());
    let tree = collect_tree(cli, &cli.flake, out)?;

    let opts = search::SearchOpts {
        all_matches: args.all_matches,
        regex: args.regex,
        nix_only: !args.no_nix_only,
        max_results: args.max,
    };
    let hits = search::search(project_root.as_deref(), &tree, &norm, &opts)?;

    if cli.json {
        let json = serde_json::json!({
            "mode": "locate",
            "needle": norm.needle,
            "partial": norm.partial,
            "hits": hits.iter().map(|h| serde_json::json!({
                "attribution": h.attribution,
                "file": h.file,
                "line": h.line_no,
                "match": h.line,
            })).collect::<Vec<_>>(),
        });
        writeln_json(&json)?;
        return Ok(());
    }

    if hits.is_empty() {
        writeln_header(out, "no matches found")?;
        writeln_dim(
            out,
            "the warning text may be constructed dynamically (try `--regex`),",
        )?;
        writeln_dim(out, "or it may come from code outside the flake closure.")?;
        return Ok(());
    }

    writeln_header(out, &format!("{} match(es) found:", hits.len()))?;
    for h in &hits {
        writeln_bold(
            out,
            &format!(
                "  [{}] {}:{}",
                h.attribution,
                display_path(&h.file),
                h.line_no
            ),
        )?;
        writeln_dim(out, &format!("    {}", h.line.trim()))?;
    }
    Ok(())
}

fn run_trigger<W: WriteColor>(cli: &Cli, args: &TriggerArgs, out: &mut W) -> Result<()> {
    let msg = read_message(args.message.as_deref())?;
    let norm = message::normalize(&msg);
    if cli.verbose {
        writeln_header(out, "nixgrep — trigger (Mode B: meta.position attribution)")?;
        if !args.nix_args.is_empty() {
            writeln_dim(out, &format!("nix {}", args.nix_args.join(" ")))?;
        }
    }

    if args.nix_args.is_empty() {
        writeln_dim(
            out,
            "warning: no nix command passed; pass one with `-- <nix args...>`,",
        )?;
        writeln_dim(out, "e.g.  nixgrep trigger '...' -- eval .#myPackage")?;
        anyhow::bail!("no nix command given for Mode B");
    }

    let project_root = Some(std::path::Path::new(&cli.flake).to_path_buf());
    let tree = collect_tree(cli, &cli.flake, out)?;

    let triggers = meta::collect(
        &args.nix_args,
        &cli.flake,
        &norm,
        &tree,
        project_root.as_deref(),
    );

    if cli.json {
        let json = match &triggers {
            Ok(ts) => serde_json::json!({
                "mode": "trigger",
                "method": "meta-position",
                "triggers": ts.iter().map(|t| serde_json::json!({
                    "name": t.name,
                    "file": t.file,
                    "line": t.line,
                    "attribution": t.attribution,
                })).collect::<Vec<_>>(),
            }),
            Err(e) => serde_json::json!({
                "mode": "trigger",
                "method": "meta-position",
                "error": e.to_string(),
            }),
        };
        writeln_json(&json)?;
        return Ok(());
    }

    match triggers {
        Ok(ts) if !ts.is_empty() => {
            writeln_header(out, "trigger(s) — where the warned package is consumed:")?;
            for t in &ts {
                writeln_bold(
                    out,
                    &format!(
                        "  [{}] {}{}  ({})",
                        t.attribution,
                        display_path(&t.file),
                        line_suffix(t.line),
                        t.name,
                    ),
                )?;
            }
            let pick = pick_trigger(&ts).unwrap();
            writeln_header(out, "trigger:")?;
            writeln_bold(
                out,
                &format!(
                    "  [{}] {}{}",
                    pick.attribution,
                    display_path(&pick.file),
                    line_suffix(pick.line),
                ),
            )?;
            if pick.attribution == "your config" {
                writeln_dim(out, "  the warning was triggered from your own config.")?;
            } else if pick.attribution.starts_with("input ") {
                writeln_dim(
                    out,
                    &format!(
                        "  the warning was triggered from {} (not your config).",
                        pick.attribution
                    ),
                )?;
            } else {
                writeln_dim(out, "  the trigger file is outside the flake closure.")?;
            }
        }
        Ok(_) => {
            writeln_header(out, "no derivations found on the eval target")?;
            writeln_dim(
                out,
                "pass an attr path that evaluates to a derivation (or a list/attrset of them),",
            )?;
            writeln_dim(
                out,
                "e.g.  -- eval .#myPackage  or  -- eval .#config.environment.systemPackages",
            )?;
        }
        Err(e) => {
            writeln_header(out, "trigger attribution failed")?;
            writeln_dim(out, &format!("  {e}"))?;
            writeln_dim(
                out,
                "you can still use `nixgrep locate` to find the emitter.",
            )?;
        }
    }
    Ok(())
}

fn run_why<W: WriteColor>(cli: &Cli, args: &WhyArgs, out: &mut W) -> Result<()> {
    let msg = read_message(args.message.as_deref())?;
    let norm = message::normalize(&msg);
    if cli.verbose {
        writeln_header(out, "nixgrep — why (Mode A + Mode B)")?;
    }

    // Mode A first.
    let project_root = Some(std::path::Path::new(&cli.flake).to_path_buf());
    let tree = collect_tree(cli, &cli.flake, out)?;
    let opts = search::SearchOpts {
        all_matches: args.all_matches,
        regex: args.regex,
        nix_only: !args.no_nix_only,
        max_results: args.max,
    };
    let hits = search::search(project_root.as_deref(), &tree, &norm, &opts)?;

    writeln_header(out, "[locate] emitter of the warning:")?;
    if hits.is_empty() {
        writeln_dim(out, "  no matches in flake closure")?;
    } else {
        for h in &hits {
            writeln_bold(
                out,
                &format!(
                    "  [{}] {}:{}",
                    h.attribution,
                    display_path(&h.file),
                    h.line_no
                ),
            )?;
            writeln_dim(out, &format!("    {}", h.line.trim()))?;
        }
    }

    // Mode B if nix args given.
    if args.nix_args.is_empty() {
        writeln_dim(
            out,
            "\n[trigger] skipped (no nix command passed; use `-- eval .#foo`)",
        )?;
        return Ok(());
    }
    let triggers = meta::collect(
        &args.nix_args,
        &cli.flake,
        &norm,
        &tree,
        project_root.as_deref(),
    );

    writeln_header(out, "\n[trigger] what caused it:")?;
    match triggers {
        Ok(ts) if !ts.is_empty() => {
            let pick = pick_trigger(&ts).unwrap();
            let pick_idx = ts.iter().position(|t| std::ptr::eq(t, pick)).unwrap();
            writeln_bold(
                out,
                &format!(
                    "  [{}] {}{}",
                    pick.attribution,
                    display_path(&pick.file),
                    line_suffix(pick.line),
                ),
            )?;
            for (i, t) in ts.iter().enumerate() {
                if i != pick_idx {
                    writeln_dim(
                        out,
                        &format!(
                            "  also: [{}] {}{} ({})",
                            t.attribution,
                            display_path(&t.file),
                            line_suffix(t.line),
                            t.name,
                        ),
                    )?;
                }
            }
        }
        Ok(_) => {
            writeln_dim(out, "  no derivations found on the eval target")?;
        }
        Err(e) => {
            writeln_dim(out, &format!("  {e}"))?;
        }
    }
    Ok(())
}

fn run_scan<W: WriteColor>(cli: &Cli, args: &ScanArgs, out: &mut W) -> Result<()> {
    let flake = args.flake.as_deref().unwrap_or(&cli.flake);

    if cli.verbose {
        writeln_header(out, "nixgrep — scan (build + auto-attribute)")?;
    }

    let (base_flake, host_hint) = scan::split_flake_ref(flake);
    let resolved_target = args
        .capture_target
        .clone()
        .or_else(|| scan::default_capture_target(&base_flake, host_hint.as_deref()));
    if cli.verbose {
        let target_desc = resolved_target
            .as_deref()
            .unwrap_or("<whole flake — no auto-target found>");
        writeln_info(
            out,
            &format!(
                "running `nix build {} --dry-run --keep-going --no-eval-cache` (capture target: {})…",
                flake, target_desc,
            ),
        )?;
    } else if args.capture_target.is_none() {
        match &resolved_target {
            Some(t) => writeln_info(out, &format!("auto-detected capture target: {t}"))?,
            None => writeln_info(
                out,
                "no --capture-target given and no nixosConfigurations/homeConfigurations found; \
                 building the bare flake",
            )?,
        }
    }
    let warnings = scan::capture_warnings(flake, resolved_target.as_deref())?;
    if warnings.is_empty() {
        writeln_header(out, "no evaluation warnings captured")?;
        writeln_dim(
            out,
            "the build produced no `evaluation warning:` / `trace:` lines from Nix expressions.",
        )?;
        return Ok(());
    }

    let trigger_targets: Vec<String> = match &args.trigger_target {
        Some(t) => vec![t.clone()],
        None => {
            let host = resolved_target
                .as_deref()
                .and_then(extract_host_from_target)
                .or_else(|| host_hint.clone());
            match host {
                Some(h) => scan::default_trigger_targets(&base_flake, &h),
                None => Vec::new(),
            }
        }
    };
    if !trigger_targets.is_empty() && args.trigger_target.is_none() {
        writeln_info(
            out,
            &format!(
                "auto-detected trigger target(s): {}",
                trigger_targets.join(", ")
            ),
        )?;
    }

    let project_root = Some(std::path::Path::new(&base_flake).to_path_buf());
    let tree = collect_tree(cli, &base_flake, out)?;

    let lock_graph = lock::parse(std::path::Path::new(&base_flake)).ok();
    if cli.verbose {
        match &lock_graph {
            Some(g) => writeln_info(
                out,
                &format!("parsed flake.lock: {} input(s)", g.inputs.len()),
            )?,
            None => writeln_info(out, "no flake.lock found or parse failed — URLs disabled")?,
        }
    }

    let opts = search::SearchOpts {
        all_matches: args.all_matches,
        regex: args.regex,
        nix_only: !args.no_nix_only,
        max_results: args.max,
    };

    let mut results: Vec<ScanResult> = Vec::new();

    for w in &warnings {
        let norm = message::normalize(&w.message);
        if norm.is_empty() {
            continue;
        }
        let hits = search::search(project_root.as_deref(), &tree, &norm, &opts).unwrap_or_default();

        let triggers = if trigger_targets.is_empty() {
            None
        } else {
            let mut merged: Vec<meta::MetaTrigger> = Vec::new();
            let mut first_err: Option<anyhow::Error> = None;
            for target in &trigger_targets {
                let nix_args = vec!["eval".to_string(), target.clone()];
                match meta::collect(
                    &nix_args,
                    &base_flake,
                    &norm,
                    &tree,
                    project_root.as_deref(),
                ) {
                    Ok(mut ts) => merged.append(&mut ts),
                    Err(e) if first_err.is_none() => first_err = Some(e),
                    Err(_) => {}
                }
            }
            // Dedupe triggers that appear in multiple target paths (e.g. the
            // same vscode extension surfaced via `programs.vscode...extensions`
            // and `programs.vscodium...extensions`). Key on (file, line, name)
            // so distinct derivations at the same site are both kept.
            let mut seen: std::collections::HashSet<(std::path::PathBuf, Option<u64>, String)> =
                std::collections::HashSet::new();
            merged.retain(|t| seen.insert((t.file.clone(), t.line, t.name.clone())));
            if merged.is_empty() && first_err.is_some() {
                Some(Err(first_err.unwrap()))
            } else {
                Some(Ok(merged))
            }
        };

        results.push(ScanResult {
            kind: w.kind,
            message: norm.needle.clone(),
            partial: norm.partial,
            hits,
            triggers,
        });
    }

    if cli.json {
        let json = serde_json::json!({
            "mode": "scan",
            "warnings": results.iter().map(|r| {
                serde_json::json!({
                    "kind": r.kind.label(),
                    "message": r.message,
                    "partial": r.partial,
                    "emitters": r.hits.iter().map(|h| serde_json::json!({
                        "attribution": h.attribution,
                        "file": h.file,
                        "line": h.line_no,
                        "match": h.line,
                        "url": source_url(&h.attribution, &h.file, Some(h.line_no), &tree, lock_graph.as_ref()),
                    })).collect::<Vec<_>>(),
                    "triggers": match &r.triggers {
                        Some(Ok(ts)) => serde_json::Value::Array(ts.iter().map(|t| serde_json::json!({
                            "name": t.name,
                            "file": t.file,
                            "line": t.line,
                            "attribution": t.attribution,
                            "url": source_url(&t.attribution, &t.file, t.line, &tree, lock_graph.as_ref()),
                        })).collect::<Vec<_>>()),
                        Some(Err(e)) => serde_json::Value::String(format!("error: {e}")),
                        None => serde_json::Value::Array(vec![]),
                    },
                })
            }).collect::<Vec<_>>(),
        });
        writeln_json(&json)?;
        return Ok(());
    }

    writeln_header(
        out,
        &format!("{} evaluation warning(s) captured:", results.len()),
    )?;
    for (i, r) in results.iter().enumerate() {
        writeln_bold(
            out,
            &format!("\n{}.  [{}] {}", i + 1, r.kind.label(), r.message),
        )?;
        if r.partial {
            writeln_dim(
                out,
                "   (interpolated message — searching for a literal fragment only)",
            )?;
        }

        let triggers_ok: Vec<_> = match &r.triggers {
            Some(Ok(ts)) => ts
                .iter()
                .filter(|t| !is_emitter_internal(t, &r.hits))
                .filter(|t| is_relevant_trigger(t, &r.hits))
                .cloned()
                .collect(),
            _ => Vec::new(),
        };

        if triggers_ok.is_empty() {
            if r.hits.is_empty() {
                writeln_dim(out, "   emitter: not found in flake closure (try --regex)")?;
            } else {
                for h in &r.hits {
                    writeln_emitter(
                        out,
                        &format!(
                            "   [emit] [{}] {}:{}",
                            h.attribution,
                            display_path(&h.file),
                            h.line_no
                        ),
                    )?;
                    writeln_dim(out, &format!("          {}", h.line.trim()))?;
                }
            }
            if let Some(Err(e)) = &r.triggers {
                writeln_dim(out, &format!("   trigger: {e}"))?;
            }
            continue;
        }

        for t in &triggers_ok {
            let line = line_suffix(t.line);
            let is_yours = t.attribution == "your config";
            let label = if is_yours {
                writeln_yours(
                    out,
                    &format!(
                        "   [trig] [{}] {}{}  ({})  ← YOUR CONFIG",
                        t.attribution,
                        display_path(&t.file),
                        line,
                        t.name,
                    ),
                )?;
                "your config"
            } else {
                writeln_trigger(
                    out,
                    &format!(
                        "   [trig] [{}] {}{}  ({})",
                        t.attribution,
                        display_path(&t.file),
                        line,
                        t.name,
                    ),
                )?;
                t.attribution.as_str()
            };

            if let Some(url) =
                source_url(&t.attribution, &t.file, t.line, &tree, lock_graph.as_ref())
            {
                writeln_info(out, &format!("          {url}"))?;
            }

            let nixpkgs_attr = correlated_nixpkgs(label);
            let correlated: Vec<&search::Hit> = if is_yours || nixpkgs_attr.is_empty() {
                Vec::new()
            } else {
                r.hits
                    .iter()
                    .filter(|h| h.attribution == nixpkgs_attr)
                    .collect()
            };
            if !correlated.is_empty() {
                for h in correlated {
                    writeln_emitter(
                        out,
                        &format!(
                            "          emitted by [{}] {}:{}",
                            h.attribution,
                            display_path(&h.file),
                            h.line_no
                        ),
                    )?;
                    writeln_dim(out, &format!("          {}", h.line.trim()))?;
                    if let Some(url) = source_url(
                        &h.attribution,
                        &h.file,
                        Some(h.line_no),
                        &tree,
                        lock_graph.as_ref(),
                    ) {
                        writeln_info(out, &format!("          {url}"))?;
                    }
                }
            } else if !r.hits.is_empty() {
                for h in &r.hits {
                    writeln_dim(
                        out,
                        &format!(
                            "          (emit) [{}] {}:{}",
                            h.attribution,
                            display_path(&h.file),
                            h.line_no
                        ),
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Given a trigger attribution like `input catppuccin`, return the attribution
/// string of the nixpkgs copy that input builds against — `input catppuccin.nixpkgs`.
/// Returns empty string for non-input triggers (e.g. "your config", "unknown").
fn correlated_nixpkgs(trigger_attr: &str) -> String {
    if let Some(input) = trigger_attr.strip_prefix("input ") {
        format!("input {input}.nixpkgs")
    } else {
        String::new()
    }
}

/// Build a source URL for `file` (an absolute path inside a flake input's
/// store tree) if the owning input has a web-hosted locked ref (GitHub/GitLab/
/// SourceHut/Codeberg). Returns None for "your config", tarball/path inputs,
/// or inputs not found in flake.lock.
fn source_url(
    attribution: &str,
    file: &std::path::Path,
    line: Option<u64>,
    tree: &archive::ArchiveTree,
    lock_graph: Option<&lock::LockGraph>,
) -> Option<String> {
    let input_name = attribution.strip_prefix("input ")?;
    let lock_graph = lock_graph?;
    let locked = lock_graph.inputs.get(input_name)?;

    let store_path = tree
        .inputs
        .iter()
        .find(|(n, _)| n == input_name)
        .map(|(_, p)| p.as_path())?;
    let rel = file.strip_prefix(store_path).ok()?;
    let rel_str = rel.to_string_lossy();
    if rel_str.is_empty() {
        return None;
    }
    locked.url(&rel_str, line)
}

/// True if an attribution refers to a nixpkgs copy (a derivation defined
/// *inside* nixpkgs, not the consuming input). These are `input nixpkgs`,
/// `input foo.nixpkgs`, `input foo.nixpkgs-lib`, etc. Currently used only by
/// tests; the live trigger filter is [`is_relevant_trigger`] which subsumes
/// this check.
#[allow(dead_code)]
fn is_nixpkgs_internal(attr: &str) -> bool {
    let Some(input) = attr.strip_prefix("input ") else {
        return false;
    };
    input == "nixpkgs"
        || input.ends_with(".nixpkgs")
        || input.ends_with(".nixpkgs-lib")
        || input == "nixpkgs-stable"
        || input.ends_with(".nixpkgs-stable")
}

/// A trigger is "emitter-internal" (and should be dropped from the trigger
/// list) when it points at the *same file* as one of the Mode A emitter hits.
///
/// A nixpkgs-defined package like vesktop has `meta.position` inside nixpkgs
/// (`pkgs/by-name/ve/vesktop/package.nix`) and is a *real* consumer of pnpm,
/// so dropping every nixpkgs-located trigger (as [`is_nixpkgs_internal`] did)
/// throws away real consumers. Instead we only drop a trigger when its file
/// is the emitting file itself — i.e. the trigger is the `lib.warn` site,
/// not a caller of it.
fn is_emitter_internal(t: &meta::MetaTrigger, hits: &[search::Hit]) -> bool {
    hits.iter().any(|h| h.file == t.file)
}

/// A trigger is "relevant" when it plausibly consumes the warned package.
///
/// We derive the warned package name from the emitter hit path (e.g.
/// `.../pkgs/development/tools/pnpm/generic.nix` → `pnpm`) and keep only
/// triggers whose source file mentions that name. This drops noise like
/// `kate`, `glib`, `zed-editor`, `hm-session-vars` while keeping real
/// consumers like `vesktop` and `catppuccin-vscode` (whose package files
/// reference `pnpm`).
///
/// When no emitter hits are available (Mode A found nothing) or the package
/// name can't be derived, all triggers are kept — the filter only narrows
/// when there's a concrete emitter to match against.
fn is_relevant_trigger(t: &meta::MetaTrigger, hits: &[search::Hit]) -> bool {
    let Some(pkg) = warned_package_name(hits) else {
        return true;
    };
    if is_nixpkgs_registrar(&t.file) {
        return false;
    }
    file_mentions(&t.file, &pkg)
}

/// True for nixpkgs top-level registrar files (e.g. `pkgs/top-level/all-packages.nix`).
///
/// These files wire up every package in nixpkgs via `callPackage`, so they
/// mention every package name without *consuming* any of them. A trigger
/// whose `meta.position` lands here (e.g. `libressl`'s position is
/// `all-packages.nix:2605`) is a false positive — the file mentions `pnpm`
/// only because it registers the pnpm attribute aliases. Excluding these
/// keeps real consumers (vesktop, catppuccin) which reference pnpm in their
/// own `nativeBuildInputs`.
fn is_nixpkgs_registrar(file: &std::path::Path) -> bool {
    file.to_string_lossy()
        .contains("/pkgs/top-level/all-packages.nix")
}

/// Derive the warned package's name from the emitter hit file path.
///
/// Emitter files live under `<nixpkgs>/pkgs/.../<pkg>/generic.nix` or
/// `<nixpkgs>/pkgs/by-name/<ab>/<pkg>/package.nix`; the directory name
/// directly above the leaf file is the package name.
fn warned_package_name(hits: &[search::Hit]) -> Option<String> {
    for h in hits {
        if let Some(name) = h
            .file
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
        {
            return Some(name.to_string());
        }
    }
    None
}

/// True if `file` contains `needle` as a substring on any line (best-effort:
/// returns false on read errors).
fn file_mentions(file: &std::path::Path, needle: &str) -> bool {
    std::fs::read_to_string(file)
        .map(|s| s.contains(needle))
        .unwrap_or(false)
}

struct ScanResult {
    kind: scan::DiagnosticKind,
    message: String,
    partial: bool,
    hits: Vec<search::Hit>,
    triggers: Option<Result<Vec<meta::MetaTrigger>, anyhow::Error>>,
}

/// Extract the host name from a capture/trigger target attr path like
/// `nixosConfigurations.nixos.config.system.build.toplevel` → `nixos`.
/// Returns None if the path doesn't start with `nixosConfigurations.`.
fn extract_host_from_target(target: &str) -> Option<String> {
    let rest = target.strip_prefix("nixosConfigurations.")?;
    let host = rest.split('.').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Pick the most useful trigger from a list: prefer "your config", then a
/// non-nixpkgs input, then any. Shared by `run_trigger`, `run_why`, and
/// `run_scan`.
fn pick_trigger(ts: &[meta::MetaTrigger]) -> Option<&meta::MetaTrigger> {
    ts.iter()
        .find(|t| t.attribution == "your config")
        .or_else(|| {
            ts.iter()
                .find(|t| !t.attribution.starts_with("input nixpkgs"))
        })
        .or_else(|| ts.first())
}

/// Format an optional line number as `:N` (or empty string).
fn line_suffix(line: Option<u64>) -> String {
    line.map(|l| format!(":{l}")).unwrap_or_default()
}

/// Collect the flake-input archive tree, printing progress in verbose mode.
fn collect_tree<W: WriteColor>(
    cli: &Cli,
    flake: &str,
    out: &mut W,
) -> Result<archive::ArchiveTree> {
    if cli.verbose {
        writeln_dim(
            out,
            &format!("running `nix flake archive --json --dry-run {}`…", flake),
        )?;
    }
    let tree = archive::collect(flake).map_err(|e| {
        anyhow::anyhow!("{e}\nhint: ensure your flake inputs are fetched, e.g. `nix flake lock`.")
    })?;
    if cli.verbose {
        writeln_dim(out, &format!("  found {} input(s)", tree.inputs.len()))?;
    }
    Ok(tree)
}

/// Read the warning message from an explicit arg or stdin.
fn read_message(arg: Option<&str>) -> Result<String> {
    if let Some(s) = arg {
        return Ok(s.to_string());
    }
    if !io::stdin().is_terminal() {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        let first_line = buf.lines().next().unwrap_or("").to_string();
        return Ok(first_line);
    }
    anyhow::bail!("no message given — pass it as an argument or pipe it via stdin")
}

fn display_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("/nix/store/") {
        if let Some(idx) = rest.find('/') {
            let first = &rest[..idx];
            if let Some(name_part) = first.split_once('-').map(|(_, name)| name) {
                let name = name_part.strip_suffix("-source").unwrap_or(name_part);
                let tail = &rest[idx + 1..];
                return format!("{name}/{tail}");
            }
        }
    }
    s.into_owned()
}

fn writeln_header<W: WriteColor>(out: &mut W, s: &str) -> Result<()> {
    out.set_color(ColorSpec::new().set_bold(true).set_fg(Some(Color::Cyan)))?;
    writeln!(out, "{s}")?;
    out.reset()?;
    Ok(())
}

fn writeln_bold<W: WriteColor>(out: &mut W, s: &str) -> Result<()> {
    out.set_color(ColorSpec::new().set_bold(true))?;
    writeln!(out, "{s}")?;
    out.reset()?;
    Ok(())
}

fn writeln_dim<W: WriteColor>(out: &mut W, s: &str) -> Result<()> {
    out.set_color(ColorSpec::new().set_fg(Some(Color::White)))?;
    writeln!(out, "{s}")?;
    out.reset()?;
    Ok(())
}

/// Bright yellow — for the pinpointed trigger (the answer to "what caused it").
fn writeln_trigger<W: WriteColor>(out: &mut W, s: &str) -> Result<()> {
    out.set_color(ColorSpec::new().set_bold(true).set_fg(Some(Color::Yellow)))?;
    writeln!(out, "{s}")?;
    out.reset()?;
    Ok(())
}

/// Green — for the pinpointed, correlated emitter (the relevant nixpkgs copy).
fn writeln_emitter<W: WriteColor>(out: &mut W, s: &str) -> Result<()> {
    out.set_color(ColorSpec::new().set_bold(true).set_fg(Some(Color::Green)))?;
    writeln!(out, "{s}")?;
    out.reset()?;
    Ok(())
}

/// Bright red — for "your config" attributions, to flag user-owned triggers.
fn writeln_yours<W: WriteColor>(out: &mut W, s: &str) -> Result<()> {
    out.set_color(ColorSpec::new().set_bold(true).set_fg(Some(Color::Red)))?;
    writeln!(out, "{s}")?;
    out.reset()?;
    Ok(())
}

/// Blue — for informational dim lines (auto-detected targets, notes).
fn writeln_info<W: WriteColor>(out: &mut W, s: &str) -> Result<()> {
    out.set_color(ColorSpec::new().set_fg(Some(Color::Blue)))?;
    writeln!(out, "{s}")?;
    out.reset()?;
    Ok(())
}

fn writeln_json(v: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_host_from_nixos_target() {
        assert_eq!(
            extract_host_from_target("nixosConfigurations.nixos.config.system.build.toplevel",),
            Some("nixos".to_string()),
        );
    }

    #[test]
    fn extracts_host_from_other_host() {
        assert_eq!(
            extract_host_from_target("nixosConfigurations.vps.config.system.build.toplevel"),
            Some("vps".to_string()),
        );
    }

    #[test]
    fn returns_none_for_non_nixos_target() {
        assert_eq!(extract_host_from_target("homeConfigurations.foo"), None);
    }

    #[test]
    fn correlated_nixpkgs_for_input() {
        assert_eq!(
            correlated_nixpkgs("input catppuccin"),
            "input catppuccin.nixpkgs",
        );
    }

    #[test]
    fn correlated_nixpkgs_for_your_config() {
        assert_eq!(correlated_nixpkgs("your config"), "");
    }

    #[test]
    fn correlated_nixpkgs_for_unknown() {
        assert_eq!(correlated_nixpkgs("unknown"), "");
    }

    #[test]
    fn nixpkgs_internal_top_level() {
        assert!(is_nixpkgs_internal("input nixpkgs"));
    }

    #[test]
    fn nixpkgs_internal_nested() {
        assert!(is_nixpkgs_internal("input catppuccin.nixpkgs"));
    }

    #[test]
    fn nixpkgs_internal_stable() {
        assert!(is_nixpkgs_internal("input nixpkgs-stable"));
        assert!(is_nixpkgs_internal("input foo.nixpkgs-stable"));
    }

    #[test]
    fn not_nixpkgs_internal_for_consumer() {
        assert!(!is_nixpkgs_internal("input catppuccin"));
        assert!(!is_nixpkgs_internal("your config"));
        assert!(!is_nixpkgs_internal("unknown"));
    }

    #[test]
    fn emitter_internal_drops_coincident_file() {
        use std::path::Path;
        let hit = search::Hit {
            attribution: "input nixpkgs".into(),
            file: Path::new("/nix/store/abc-source/pkgs/development/tools/pnpm/generic.nix")
                .to_path_buf(),
            line_no: 28,
            line: r#"lib.warn "pnpm: Override nodejs-slim instead of nodejs" nodejs;"#.into(),
        };
        let t = meta::MetaTrigger {
            name: "pnpm-10.29.2".into(),
            file: Path::new("/nix/store/abc-source/pkgs/development/tools/pnpm/generic.nix")
                .to_path_buf(),
            line: Some(28),
            attribution: "input nixpkgs".into(),
        };
        assert!(is_emitter_internal(&t, std::slice::from_ref(&hit)));
    }

    #[test]
    fn emitter_internal_keeps_distinct_file() {
        use std::path::Path;
        let hit = search::Hit {
            attribution: "input nixpkgs".into(),
            file: Path::new("/nix/store/abc-source/pkgs/development/tools/pnpm/generic.nix")
                .to_path_buf(),
            line_no: 28,
            line: r#"lib.warn "pnpm: Override nodejs-slim instead of nodejs" nodejs;"#.into(),
        };
        let t = meta::MetaTrigger {
            name: "vesktop-1.6.5".into(),
            file: Path::new("/nix/store/abc-source/pkgs/by-name/ve/vesktop/package.nix")
                .to_path_buf(),
            line: Some(1),
            attribution: "input nixpkgs".into(),
        };
        assert!(!is_emitter_internal(&t, std::slice::from_ref(&hit)));
    }

    #[test]
    fn warned_package_name_from_emitter_path() {
        use std::path::Path;
        let hit = search::Hit {
            attribution: "input nixpkgs".into(),
            file: Path::new("/nix/store/abc-source/pkgs/development/tools/pnpm/generic.nix")
                .to_path_buf(),
            line_no: 28,
            line: String::new(),
        };
        assert_eq!(
            warned_package_name(std::slice::from_ref(&hit)),
            Some("pnpm".into())
        );
    }

    #[test]
    fn warned_package_name_from_by_name_path() {
        use std::path::Path;
        let hit = search::Hit {
            attribution: "input nixpkgs".into(),
            file: Path::new("/nix/store/abc-source/pkgs/by-name/ve/vesktop/package.nix")
                .to_path_buf(),
            line_no: 1,
            line: String::new(),
        };
        assert_eq!(
            warned_package_name(std::slice::from_ref(&hit)),
            Some("vesktop".into()),
        );
    }

    #[test]
    fn warned_package_name_none_when_no_hits() {
        assert_eq!(warned_package_name(&[]), None);
    }

    #[test]
    fn is_relevant_trigger_keeps_when_no_emitter_hits() {
        use std::path::Path;
        let t = meta::MetaTrigger {
            name: "foo".into(),
            file: Path::new("/tmp/nonexistent-xyz/foo.nix").to_path_buf(),
            line: None,
            attribution: "input nixpkgs".into(),
        };
        assert!(is_relevant_trigger(&t, &[]));
    }

    #[test]
    fn is_relevant_trigger_keeps_when_file_mentions_package() {
        use std::path::Path;
        let dir = std::env::temp_dir().join(format!(
            "nixgrep-rel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("package.nix");
        std::fs::write(&file, "buildInputs = [ pnpm ];\n").unwrap();
        let hit = search::Hit {
            attribution: "input nixpkgs".into(),
            file: Path::new("/nix/store/abc-source/pkgs/development/tools/pnpm/generic.nix")
                .to_path_buf(),
            line_no: 28,
            line: String::new(),
        };
        let t = meta::MetaTrigger {
            name: "my-pkg".into(),
            file: file.clone(),
            line: Some(1),
            attribution: "input nixpkgs".into(),
        };
        assert!(is_relevant_trigger(&t, std::slice::from_ref(&hit)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn is_relevant_trigger_drops_when_file_does_not_mention_package() {
        use std::path::Path;
        let dir = std::env::temp_dir().join(format!(
            "nixgrep-rel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("package.nix");
        std::fs::write(&file, "buildInputs = [ cmake ];\n").unwrap();
        let hit = search::Hit {
            attribution: "input nixpkgs".into(),
            file: Path::new("/nix/store/abc-source/pkgs/development/tools/pnpm/generic.nix")
                .to_path_buf(),
            line_no: 28,
            line: String::new(),
        };
        let t = meta::MetaTrigger {
            name: "my-pkg".into(),
            file: file.clone(),
            line: Some(1),
            attribution: "input nixpkgs".into(),
        };
        assert!(!is_relevant_trigger(&t, std::slice::from_ref(&hit)));
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod display_path_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn display_path_multi_dash_name() {
        let p = Path::new("/nix/store/abc123-nixfmt-classic-source/pkgs/foo.nix");
        assert_eq!(display_path(p), "nixfmt-classic/pkgs/foo.nix");
    }

    #[test]
    fn display_path_no_source_suffix() {
        let p = Path::new("/nix/store/abc123-some-package/lib/x.nix");
        assert_eq!(display_path(p), "some-package/lib/x.nix");
    }

    #[test]
    fn display_path_non_store() {
        let p = Path::new("/home/me/cfg/flake.nix");
        assert_eq!(display_path(p), "/home/me/cfg/flake.nix");
    }
}
