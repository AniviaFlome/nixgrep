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
//!   * `update` checks top-level flake inputs pinned to a commit for upstream
//!     updates.

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
mod update;

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
    /// Check top-level flake inputs pinned to a commit (`rev = "..."`) for
    /// upstream updates via `git ls-remote`, and optionally update them.
    Update(UpdateArgs),
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

#[derive(Parser, Debug)]
struct UpdateArgs {
    /// Actually update outdated inputs (runs the suggested `nix flake`
    /// command). Without it, only report and print the suggested command.
    #[arg(long)]
    update: bool,
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
        Command::Update(ref args) => run_update(&cli, args, &mut out),
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
    // Trigger attribution reads `meta.position` and needs no message text;
    // still consume the arg/stdin so missing input errors the same way.
    let _ = read_message(args.message.as_deref())?;
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

    let triggers = meta::collect(&args.nix_args, &cli.flake, &tree, project_root.as_deref());

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
    let triggers = meta::collect(&args.nix_args, &cli.flake, &tree, project_root.as_deref());

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
    let t_capture_target = std::time::Instant::now();
    let resolved_target = args
        .capture_target
        .clone()
        .or_else(|| scan::default_capture_target(&base_flake, host_hint.as_deref()));
    writeln_timing(cli, out, "capture-target detection", t_capture_target)?;
    // Status/info lines go to stdout for human output but must not pollute
    // --json (which also writes to stdout). Skip them entirely in JSON mode.
    if !cli.json {
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
    }
    let t_capture = std::time::Instant::now();
    let warnings = scan::capture_warnings(flake, resolved_target.as_deref())?;
    writeln_timing(
        cli,
        out,
        "warning capture (`nix build --dry-run`)",
        t_capture,
    )?;
    if warnings.is_empty() {
        if !cli.json {
            writeln_header(out, "no evaluation warnings captured")?;
            writeln_dim(
                out,
                "the build produced no `evaluation warning:` / `trace:` lines from Nix expressions.",
            )?;
        } else {
            writeln_json(&serde_json::json!({"mode": "scan", "warnings": []}))?;
        }
        return Ok(());
    }

    let t_trig_detect = std::time::Instant::now();
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
    writeln_timing(cli, out, "trigger-target detection", t_trig_detect)?;
    if !cli.json && !trigger_targets.is_empty() && args.trigger_target.is_none() {
        writeln_info(
            out,
            &format!(
                "auto-detected trigger target(s): {}",
                trigger_targets.join(", ")
            ),
        )?;
    }

    let project_root = Some(std::path::Path::new(&base_flake).to_path_buf());
    let t_tree = std::time::Instant::now();
    let tree = collect_tree(cli, &base_flake, out)?;
    writeln_timing(cli, out, "input tree (`nix flake archive`)", t_tree)?;

    let lock_graph = lock::parse(std::path::Path::new(&base_flake)).ok();
    if cli.verbose && !cli.json {
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
    let mut t_search = std::time::Duration::ZERO;
    let mut t_triggers = std::time::Duration::ZERO;

    // Trigger attribution doesn't depend on the warning text (it reads
    // `meta.position` off the trigger targets), so collect it ONCE up front
    // instead of re-evaluating per warning. The per-target evals run in
    // parallel; each result is cloned into the per-warning results below.
    let t0 = std::time::Instant::now();
    let shared_triggers: Option<Result<Vec<meta::MetaTrigger>, String>> =
        if trigger_targets.is_empty() {
            None
        } else {
            Some(collect_triggers_parallel(
                &base_flake,
                &trigger_targets,
                &tree,
                project_root.as_deref(),
            ))
        };
    t_triggers += t0.elapsed();

    // (kind, needle) per captured warning; empties are dropped everywhere.
    let mut indexed_norms: Vec<(scan::DiagnosticKind, message::Normalized)> = Vec::new();
    for w in &warnings {
        let norm = message::normalize(&w.message);
        if norm.is_empty() {
            continue;
        }
        indexed_norms.push((w.kind, norm));
    }
    let needles: Vec<message::Normalized> = indexed_norms.iter().map(|(_, n)| n.clone()).collect();

    // One walk of the closure for every needle at once (instead of one
    // walk per warning), spread across threads.
    let t0 = std::time::Instant::now();
    let all_hits = if needles.is_empty() {
        Vec::new()
    } else {
        match search::search_all(project_root.as_deref(), &tree, &needles, &opts) {
            Ok(h) => h,
            Err(_) => vec![Vec::new(); needles.len()],
        }
    };
    t_search += t0.elapsed();

    for ((kind, norm), hits) in indexed_norms
        .into_iter()
        .zip(all_hits.into_iter().chain(std::iter::repeat_with(Vec::new)))
    {
        // Rebuild the owned per-warning shape from the shared collection.
        // (Cloning a few hundred small structs is free next to a nix eval.)
        let triggers = shared_triggers
            .clone()
            .map(|r| r.map_err(|e| anyhow::anyhow!("{e}")));

        results.push(ScanResult {
            kind,
            message: norm.needle.clone(),
            partial: norm.partial,
            hits,
            triggers,
        });
    }
    writeln_timing_total(cli, out, "emitter search (all warnings)", t_search)?;
    writeln_timing_total(
        cli,
        out,
        "trigger collection (shared, parallel)",
        t_triggers,
    )?;

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

        // Sort triggers so the most actionable ones appear first:
        // "your config" → non-nixpkgs inputs → nixpkgs inputs. This keeps the
        // user's own config and third-party inputs (the ones they can fix) at
        // the top, and pushes the long tail of nixpkgs-internal consumers
        // (which the user can only fix by updating nixpkgs) to the bottom.
        let mut ordered: Vec<&meta::MetaTrigger> = triggers_ok.iter().collect();
        ordered.sort_by_key(|t| trigger_priority(&t.attribution));

        // Cap the displayed triggers so a generic warning (e.g. the
        // `stdenv.isDarwin` deprecation that fires for every stdenv consumer)
        // doesn't flood the terminal with hundreds of lines. Show the top N
        // and summarize the rest.
        const MAX_TRIGGERS_DISPLAY: usize = 10;
        let display: &[&meta::MetaTrigger] = if ordered.len() > MAX_TRIGGERS_DISPLAY {
            &ordered[..MAX_TRIGGERS_DISPLAY]
        } else {
            &ordered
        };
        let omitted = ordered.len() - display.len();

        for t in display {
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
        if omitted > 0 {
            writeln_dim(
                out,
                &format!("   …and {omitted} more trigger(s) omitted (pass --json to see all)"),
            )?;
        }
    }
    Ok(())
}

/// Evaluate every trigger target's `meta.position` probe, in parallel, and
/// merge/dedupe into one trigger list.
///
/// The per-target `nix eval`s are independent (each evaluates a different
/// attr path), so they run on scoped threads — wall time drops from the sum
/// to roughly the slowest single eval. The merge order follows `targets`
/// order (handles are joined in spawn order), keeping output deterministic.
///
/// Dedupe keys on (file, line, name): the same extension can surface via
/// several target paths (e.g. `programs.vscode...extensions` and
/// `programs.vscodium...extensions`), while distinct derivations defined at
/// the same site are all kept. When every target fails, the first error is
/// returned.
fn collect_triggers_parallel(
    flake: &str,
    targets: &[String],
    tree: &archive::ArchiveTree,
    project_root: Option<&std::path::Path>,
) -> Result<Vec<meta::MetaTrigger>, String> {
    let mut merged: Vec<meta::MetaTrigger> = Vec::new();
    let mut first_err: Option<String> = None;
    std::thread::scope(|s| {
        let handles: Vec<_> = targets
            .iter()
            .map(|t| {
                s.spawn(|| {
                    let nix_args = vec!["eval".to_string(), t.clone()];
                    meta::collect(&nix_args, flake, tree, project_root)
                })
            })
            .collect();
        for h in handles {
            match h.join() {
                Ok(Ok(mut ts)) => merged.append(&mut ts),
                Ok(Err(e)) => {
                    if first_err.is_none() {
                        first_err = Some(format!("{e:#}"));
                    }
                }
                Err(_) => {
                    if first_err.is_none() {
                        first_err = Some("trigger worker thread panicked".to_string());
                    }
                }
            }
        }
    });
    let mut seen: std::collections::HashSet<(std::path::PathBuf, Option<u64>, String)> =
        std::collections::HashSet::new();
    merged.retain(|t| seen.insert((t.file.clone(), t.line, t.name.clone())));
    if merged.is_empty() && first_err.is_some() {
        Err(first_err.unwrap())
    } else {
        Ok(merged)
    }
}

/// Sort priority for trigger attributions — lower comes first.
/// `your config` (0) < non-nixpkgs input (1) < nixpkgs input (2) < unknown (3).
/// Used to surface actionable triggers above the nixpkgs-internal long tail.
fn trigger_priority(attr: &str) -> u8 {
    if attr == "your config" {
        0
    } else if is_nixpkgs_internal(attr) {
        2
    } else if attr.starts_with("input ") {
        1
    } else {
        3
    }
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
/// `input foo.nixpkgs`, `input foo.nixpkgs-lib`, etc. Used by
/// [`trigger_priority`] to push nixpkgs-internal consumers below actionable
/// triggers in the scan output.
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
    if cli.verbose && !cli.json {
        writeln_dim(
            out,
            &format!("running `nix flake archive --json --dry-run {}`…", flake),
        )?;
    }
    let tree = archive::collect(flake).map_err(|e| {
        anyhow::anyhow!("{e}\nhint: ensure your flake inputs are fetched, e.g. `nix flake lock`.")
    })?;
    if cli.verbose && !cli.json {
        writeln_dim(out, &format!("  found {} input(s)", tree.inputs.len()))?;
    }
    Ok(tree)
}

fn run_update<W: WriteColor>(cli: &Cli, args: &UpdateArgs, out: &mut W) -> Result<()> {
    // `update` reads and rewrites the checkout's flake.lock, so it needs a
    // local flake directory — not a `github:…` ref.
    let flake_dir = std::path::Path::new(&cli.flake);
    if !flake_dir.join("flake.lock").is_file() {
        anyhow::bail!(
            "no flake.lock found at {} — `update` needs a local flake checkout (e.g. `--flake ~/nix-config`)",
            flake_dir.join("flake.lock").display()
        );
    }
    let graph = lock::parse(flake_dir)?;
    if cli.verbose && !cli.json {
        writeln_dim(
            out,
            &format!(
                "parsed flake.lock: {} top-level input(s)",
                graph.top_level.len()
            ),
        )?;
    }

    let rows = update::check_top_level(&graph);
    let planned: Vec<update::Bump> = update::bumps(&rows);

    if cli.json {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let bumped = if args.update && !planned.is_empty() {
            apply_bumps(out, &cli.flake, flake_dir, &planned)?
        } else {
            Vec::new()
        };
        let json = serde_json::json!({
            "mode": "update",
            "flake": cli.flake,
            "update_applied": !bumped.is_empty(),
            "bumped": bumped,
            "inputs": rows.iter().map(|r| {
                let mut v = serde_json::json!({
                    "name": r.name,
                    "status": r.status.label(),
                    "locked_rev": r.locked_rev,
                });
                if let Some(ts) = r.locked_date {
                    v["locked_age"] = update::format_age(now, ts).into();
                }
                match &r.status {
                    update::InputStatus::Skipped(reason)
                    | update::InputStatus::Unknown(reason) => {
                        v["reason"] = reason.as_str().into();
                    }
                    _ => {}
                }
                if let Some(h) = &r.head {
                    v["branch"] = h.branch.as_str().into();
                    v["head"] = h.sha.as_str().into();
                }
                if let Some(u) = &r.compare_url {
                    v["compare_url"] = u.as_str().into();
                }
                v
            }).collect::<Vec<_>>(),
        });
        writeln_json(&json)?;
        return Ok(());
    }

    if rows.is_empty() {
        writeln_header(out, "no flake inputs found")?;
        return Ok(());
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut counts = (0usize, 0usize, 0usize, 0usize); // outdated, current, skipped, unknown
    for r in &rows {
        let locked = r
            .locked_rev
            .as_deref()
            .map(update::short_rev)
            .unwrap_or("?");
        let age = r
            .locked_date
            .map(|ts| format!(" ({})", update::format_age(now, ts)))
            .unwrap_or_default();
        match &r.status {
            update::InputStatus::Outdated => {
                counts.0 += 1;
                let head = r
                    .head
                    .as_ref()
                    .map(|h| update::short_rev(&h.sha))
                    .unwrap_or("?");
                let branch = r.head.as_ref().map(|h| h.branch.as_str()).unwrap_or("?");
                writeln_trigger(
                    out,
                    &format!("  [outdated] {}: {locked}{age} → {head} ({branch})", r.name),
                )?;
                if let Some(u) = &r.compare_url {
                    writeln_dim(out, &format!("    {u}"))?;
                }
            }
            update::InputStatus::UpToDate => {
                counts.1 += 1;
                writeln_emitter(out, &format!("  [current] {}: {locked}{age}", r.name))?;
            }
            update::InputStatus::Skipped(reason) => {
                counts.2 += 1;
                writeln_dim(out, &format!("  [skipped] {}: {reason}", r.name))?;
            }
            update::InputStatus::Unknown(reason) => {
                counts.3 += 1;
                writeln_dim(out, &format!("  [unknown] {}: {reason}", r.name))?;
            }
        }
    }
    writeln_header(
        out,
        &format!(
            "{} outdated, {} current, {} skipped, {} unknown",
            counts.0, counts.1, counts.2, counts.3
        ),
    )?;

    if planned.is_empty() {
        return Ok(());
    }
    if args.update {
        let bumped = apply_bumps(out, &cli.flake, flake_dir, &planned)?;
        writeln_header(
            out,
            &format!(
                "updated {} input(s) — review `git diff flake.nix flake.lock`",
                bumped.len()
            ),
        )?;
    } else {
        writeln_bold(out, "to update (in flake.nix, then `nix flake lock`):")?;
        for b in &planned {
            writeln_dim(out, &format!("  {}: {} → {}", b.name, b.old_rev, b.new_rev))?;
        }
        writeln_dim(out, "  (or re-run with `--update` to apply)")?;
    }
    Ok(())
}

/// Bump each planned pin in `flake.nix` and re-lock once. Returns the names
/// that were bumped. A pin whose rev isn't literally in `flake.nix` is
/// reported and skipped instead of failing the whole run.
fn apply_bumps<W: WriteColor>(
    out: &mut W,
    flake: &str,
    flake_dir: &std::path::Path,
    planned: &[update::Bump],
) -> Result<Vec<String>> {
    let mut bumped = Vec::new();
    for b in planned {
        match update::bump_rev_in_flake_nix(flake_dir, &b.old_rev, &b.new_rev) {
            Ok(n) => {
                writeln_info(
                    out,
                    &format!(
                        "bumped {}: {} → {} ({n} occurrence(s))",
                        b.name, b.old_rev, b.new_rev
                    ),
                )?;
                bumped.push(b.name.clone());
            }
            Err(e) => {
                writeln_dim(out, &format!("  [skip] {}: {e}", b.name))?;
            }
        }
    }
    if bumped.is_empty() {
        return Ok(bumped);
    }
    writeln_info(out, "running `nix flake lock`…")?;
    let summary = update::relock(flake)?;
    if !summary.is_empty() {
        writeln_dim(out, &summary)?;
    }
    Ok(bumped)
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

/// Print `stage took Xs` in `--verbose` mode (never in `--json` mode, where
/// stdout must stay pure JSON). Used to make `scan` stage costs visible.
fn writeln_timing<W: WriteColor>(
    cli: &Cli,
    out: &mut W,
    stage: &str,
    since: std::time::Instant,
) -> Result<()> {
    writeln_timing_total(cli, out, stage, since.elapsed())
}

fn writeln_timing_total<W: WriteColor>(
    cli: &Cli,
    out: &mut W,
    stage: &str,
    elapsed: std::time::Duration,
) -> Result<()> {
    if cli.verbose && !cli.json {
        writeln_info(
            out,
            &format!("{} took {:.1}s", stage, elapsed.as_secs_f64()),
        )?;
    }
    Ok(())
}

fn writeln_json(v: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "main_display_path_tests.rs"]
mod display_path_tests;
