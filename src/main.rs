use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use mzn::analyze::{self, Analysis, Inputs};
use mzn::rtk::db::RtkDb;
use mzn::rtk::filters::{FilterFile, managed_defs, merge_defs, with_managed_block};
use mzn::rtk::{self, Classifier, Mode};
use mzn::session::{self, BashCall};
use mzn::settings::{self, Restore};
use mzn::suggest::{self, Options};
use mzn::util::{confirm, human};
use mzn::{doctor, paths, report};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "mzn",
    version,
    about = "RTK saves tokens. mzn makes RTK fit your project.",
    long_about = "mzn reads your local Claude Code session logs, measures which commands cost the most tokens and whether RTK filters them, and generates project-specific RTK filters for the rest.\n\nEverything is local: no network, no LLM calls, no telemetry."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(clap::Args, Clone)]
struct Scope {
    /// Project directory (default: current directory)
    #[arg(long, short = 'p', value_name = "PATH")]
    project: Option<PathBuf>,
    /// Only sessions from the last N days
    #[arg(long, short = 'd')]
    days: Option<u32>,
    /// Do not call the rtk binary (classify from command text only)
    #[arg(long)]
    no_rtk: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Token cost per command, RTK coverage, waste and over-compression
    Analyze {
        #[command(flatten)]
        scope: Scope,
        /// Rows per table
        #[arg(long, short = 'n', default_value_t = 10)]
        limit: usize,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Check RTK, hooks, filters and Lerim. Changes nothing.
    Doctor {
        #[arg(long, short = 'p', value_name = "PATH")]
        project: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        /// Offer to run RTK's official installer if RTK is missing
        #[arg(long)]
        install_rtk: bool,
        /// Skip the confirmation prompt for --install-rtk
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Generate RTK filters for costly commands RTK does not cover
    Suggest {
        #[command(flatten)]
        scope: Scope,
        /// Write the filters into .rtk/filters.toml (mzn's managed block only)
        #[arg(long)]
        apply: bool,
        /// Remove mzn's managed block from .rtk/filters.toml
        #[arg(long, conflicts_with = "apply")]
        clear: bool,
        /// Minimum runs of a command to consider it
        #[arg(long, default_value_t = 2)]
        min_runs: u64,
        /// Minimum estimated tokens for a command to consider it
        #[arg(long, default_value_t = 500)]
        min_tokens: u64,
        #[arg(long)]
        json: bool,
    },
    /// Shareable summary for the last N days (default 7)
    Report {
        #[command(flatten)]
        scope: Scope,
        /// Output markdown instead of plain text
        #[arg(long, alias = "md")]
        markdown: bool,
        #[arg(long)]
        json: bool,
    },
    /// Make sure exactly one RTK hook handles Bash, and add mzn's SessionEnd hook
    Activate {
        #[arg(long, short = 'p', value_name = "PATH")]
        project: Option<PathBuf>,
        /// Apply without asking
        #[arg(long, short = 'y')]
        yes: bool,
        /// Do not add the SessionEnd stats hook
        #[arg(long)]
        no_session_hook: bool,
    },
    /// Undo everything `mzn activate` changed
    Deactivate {
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Hook entry points called by Claude Code
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        hook: HookCmd,
    },
}

#[derive(Subcommand)]
enum HookCmd {
    /// SessionEnd: record a one-line summary of the finished session
    SessionEnd,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("mzn: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Cmd::Analyze { scope, limit, json } => {
            let (a, _) = load(&scope, 30)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&a)?);
            } else {
                print!("{}", analyze::render_text(&a, limit));
            }
        }
        Cmd::Doctor {
            project,
            json,
            install_rtk,
            yes,
        } => {
            let project = paths::resolve_project(project.as_deref());
            let r = doctor::run(&project);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print!("{}", doctor::render_text(&r));
            }
            if install_rtk {
                return install_rtk_flow(r.rtk.mode, yes);
            }
        }
        Cmd::Suggest {
            scope,
            apply,
            clear,
            min_runs,
            min_tokens,
            json,
        } => return suggest_cmd(&scope, apply, clear, min_runs, min_tokens, json),
        Cmd::Report {
            scope,
            markdown,
            json,
        } => {
            let (a, calls) = load(&scope, 7)?;
            let since = Utc::now() - Duration::days(i64::from(a.days));
            let savings = RtkDb::open().map(|db| db.savings_since(since));
            let project = paths::resolve_project(scope.project.as_deref());
            let pf = FilterFile::load(&paths::rtk_project_filters(&project));
            let gf = FilterFile::load(&paths::rtk_global_filters());
            let (sugg, _) = suggest::suggest(
                &a,
                &calls,
                &|k| user_filter_for(&pf, &gf, k),
                &Options::default(),
            );
            let r = report::build(&a, savings, &sugg);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print!("{}", report::render(&r, markdown));
            }
        }
        Cmd::Activate {
            project,
            yes,
            no_session_hook,
        } => return activate_cmd(project.as_deref(), yes, !no_session_hook),
        Cmd::Deactivate { yes } => return deactivate_cmd(yes),
        Cmd::Hook {
            hook: HookCmd::SessionEnd,
        } => {
            // Hooks must never block or fail the session.
            let _ = session_end_hook();
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Parse sessions and analyze them for the given scope.
fn load(scope: &Scope, default_days: u32) -> Result<(Analysis, Vec<BashCall>)> {
    let project = paths::resolve_project(scope.project.as_deref());
    let days = scope.days.unwrap_or(default_days);
    let since = Utc::now() - Duration::days(i64::from(days));
    let (calls, parse) =
        session::load_project(&paths::claude_projects_dir(), &project, Some(since));

    let info = if scope.no_rtk {
        None
    } else {
        Some(rtk::detect())
    };
    let mut classifier = match &info {
        Some(i) => Classifier::new(i),
        None => Classifier::disabled(),
    };
    let decisions = match (&info, RtkDb::open()) {
        (Some(_), Some(db)) => {
            let sessions: HashSet<String> = calls.iter().map(|c| c.session_id.clone()).collect();
            db.hook_decisions(&sessions)
        }
        _ => Default::default(),
    };
    if let Some(i) = &info
        && !matches!(i.mode, Mode::Full)
    {
        eprintln!("mzn: {}", i.describe());
    }
    let a = analyze::analyze(Inputs {
        project: project.display().to_string(),
        days,
        calls: &calls,
        parse,
        decisions: &decisions,
        classifier: &mut classifier,
    });
    Ok((a, calls))
}

fn user_filter_for(pf: &FilterFile, gf: &FilterFile, key: &str) -> Option<String> {
    pf.user_filter_matching(key)
        .or_else(|| gf.user_filter_matching(key))
        .map(str::to_string)
}

fn unified_diff(path: &Path, old: &str, new: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(
            &format!("{} (current)", path.display()),
            &format!("{} (proposed)", path.display()),
        )
        .to_string()
}

fn suggest_cmd(
    scope: &Scope,
    apply: bool,
    clear: bool,
    min_runs: u64,
    min_tokens: u64,
    json: bool,
) -> Result<ExitCode> {
    let project = paths::resolve_project(scope.project.as_deref());
    let path = paths::rtk_project_filters(&project);
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };

    if clear {
        let Some(text) = existing else {
            println!("Nothing to clear: {} does not exist.", path.display());
            return Ok(ExitCode::SUCCESS);
        };
        let new = with_managed_block(Some(&text), &[])?;
        if new == text {
            println!("No mzn-managed block in {}.", path.display());
        } else {
            mzn::util::atomic_write(&path, new.as_bytes())?;
            println!("Removed mzn's managed block from {}.", path.display());
            println!("Re-run `rtk trust` so RTK keeps loading your remaining filters.");
        }
        return Ok(ExitCode::SUCCESS);
    }

    if apply {
        require_full_mode("suggest --apply")?;
    }
    let (a, calls) = load(scope, 30)?;
    let pf = FilterFile::load(&path);
    let gf = FilterFile::load(&paths::rtk_global_filters());
    let opts = Options {
        min_runs,
        min_tokens,
        ..Options::default()
    };
    let (sugg, skipped) = suggest::suggest(&a, &calls, &|k| user_filter_for(&pf, &gf, k), &opts);
    let fresh: Vec<_> = sugg.iter().map(|s| s.filter.clone()).collect();
    let new = if fresh.is_empty() {
        None
    } else {
        let kept = existing.as_deref().map(managed_defs).unwrap_or_default();
        Some(with_managed_block(
            existing.as_deref(),
            &merge_defs(kept, &fresh),
        )?)
    };

    if json {
        let out = serde_json::json!({
            "file": path,
            "suggestions": sugg,
            "skipped": skipped,
            "applied": apply && new.is_some(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        if sugg.is_empty() {
            println!("No filter suggestions for {}.", project.display());
            if a.totals.bash_calls == 0 {
                println!("(no Claude Code Bash activity found for this project)");
            } else if a.coverage_source == analyze::CoverageSource::None {
                println!(
                    "RTK is not available, so mzn cannot tell which commands RTK already filters. See `mzn doctor`."
                );
            } else if !a
                .waste
                .iter()
                .any(|r| r.coverage == analyze::Coverage::Uncovered)
            {
                println!("Every costly command is covered by RTK or your filters.");
            }
        } else {
            println!("Suggested RTK filters for {}:\n", project.display());
            for s in &sugg {
                println!(
                    "  {}  ({} runs, ~{} → ~{} tokens, -{:.0}% estimated)",
                    s.filter.name,
                    s.runs,
                    human(s.est_tokens),
                    human(s.est_tokens_after),
                    s.est_saved_pct
                );
                for r in &s.reasons {
                    println!("      {r}");
                }
            }
        }
        for s in &skipped {
            println!("  skipped `{}`: {}", s.key, s.reason);
        }
        if let Some(new) = &new {
            let old = existing.clone().unwrap_or_default();
            if new == &old {
                println!("\n{} is already up to date.", path.display());
            } else {
                println!("\n{}", unified_diff(&path, &old, new));
            }
        }
    }

    match (&new, apply) {
        (Some(new), true) if existing.as_deref() != Some(new.as_str()) => {
            mzn::util::atomic_write(&path, new.as_bytes())?;
            eprintln!(
                "Wrote {}. RTK ignores changed filter files until you approve them:\n  cd {} && rtk trust",
                path.display(),
                project.display()
            );
        }
        (Some(_), false) if !json => {
            eprintln!("Dry run. Re-run with --apply to write only mzn's managed block.");
        }
        _ => {}
    }
    Ok(ExitCode::SUCCESS)
}

/// Outside the supported RTK range mzn only analyzes; it changes nothing.
fn require_full_mode(what: &str) -> Result<()> {
    let info = rtk::detect();
    if info.mode == Mode::Full {
        return Ok(());
    }
    anyhow::bail!(
        "`mzn {what}` needs RTK {}–<{} (read-only mode otherwise): {}",
        rtk::MIN_FULL,
        rtk::MAX_TESTED_EXCLUSIVE,
        info.describe()
    )
}

fn session_hook_command() -> String {
    if paths::which("mzn").is_some() {
        settings::SESSION_END_COMMAND.to_string()
    } else {
        match std::env::current_exe() {
            Ok(p) => format!("\"{}\" hook session-end", p.display()),
            Err(_) => settings::SESSION_END_COMMAND.to_string(),
        }
    }
}

fn activate_cmd(project: Option<&Path>, yes: bool, session_hook: bool) -> Result<ExitCode> {
    require_full_mode("activate")?;
    let project = paths::resolve_project(project);
    let report = doctor::run(&project);
    print!("{}", doctor::render_text(&report));
    println!();

    let changes = settings::plan_activate(&project, session_hook, &session_hook_command())?;
    if changes.is_empty() {
        println!("Already active: nothing to change.");
        return Ok(ExitCode::SUCCESS);
    }
    println!("mzn activate will make these changes (backed up first; undo with `mzn deactivate`):");
    for c in &changes {
        println!("\n  {}", c.file.display());
        for r in &c.reasons {
            println!("    - {r}");
        }
        let (before, after) = settings::preview(c)?;
        let before = before
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        print!("{}", unified_diff(&c.file, &before, &after));
    }
    println!();
    if !yes && !confirm("Apply these changes?") {
        println!("Aborted. Nothing was changed.");
        return Ok(ExitCode::FAILURE);
    }
    settings::apply_activate(&changes)?;
    println!(
        "Done. State recorded in {}.",
        paths::mzn_state_file().display()
    );
    Ok(ExitCode::SUCCESS)
}

fn deactivate_cmd(yes: bool) -> Result<ExitCode> {
    let Some(state) = settings::load_state()? else {
        println!("mzn is not active: nothing to restore.");
        return Ok(ExitCode::SUCCESS);
    };
    let plan = settings::plan_deactivate(&state)?;
    let mut needs_confirm = false;
    for (path, r) in &plan {
        match r {
            Restore::Exact(_) => println!("restore {} from backup (exact)", path.display()),
            Restore::Delete => println!("remove {} (did not exist before)", path.display()),
            Restore::Skip(why) => println!("skip {}: {why}", path.display()),
            Restore::Merge { current, merged } => {
                needs_confirm = true;
                println!(
                    "{} changed since activation; merging the reversal into your current content:",
                    path.display()
                );
                print!("{}", unified_diff(path, current, merged));
            }
        }
    }
    if needs_confirm && !yes && !confirm("Apply the merged changes above?") {
        println!("Aborted. Nothing was changed.");
        return Ok(ExitCode::FAILURE);
    }
    settings::apply_deactivate(&plan)?;
    println!("Deactivated.");
    Ok(ExitCode::SUCCESS)
}

fn install_rtk_flow(mode: Mode, yes: bool) -> Result<ExitCode> {
    if !matches!(mode, Mode::Missing) {
        println!("\nRTK is already installed; not running the installer.");
        return Ok(ExitCode::SUCCESS);
    }
    let script =
        "curl -fsSL https://raw.githubusercontent.com/rtk-ai/rtk/refs/heads/master/install.sh | sh";
    println!("\nRTK's official installer:\n  {script}");
    if !yes && !confirm("Download and run it now?") {
        println!("Not installed. You can run the command above yourself.");
        return Ok(ExitCode::FAILURE);
    }
    let status = std::process::Command::new("sh")
        .args(["-c", script])
        .status()
        .context("running the RTK installer")?;
    if status.success() {
        println!("RTK installed. Next: `rtk init -g` to add its Claude Code hook.");
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

fn session_end_hook() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().take(1 << 20).read_to_string(&mut input)?;
    let v: serde_json::Value = serde_json::from_str(&input)?;
    let transcript = v
        .get("transcript_path")
        .and_then(|p| p.as_str())
        .context("no transcript_path")?;
    let (calls, parse) = session::parse_file(Path::new(transcript));
    let tokens: u64 = calls
        .iter()
        .map(|c| mzn::util::est_tokens(c.seen_chars))
        .sum();
    let rtk_calls = calls
        .iter()
        .filter(|c| mzn::cmdkey::classify(&c.command).rtk_prefixed)
        .count();
    let line = serde_json::json!({
        "ended_at": Utc::now().to_rfc3339(),
        "session_id": v.get("session_id"),
        "cwd": v.get("cwd"),
        "reason": v.get("reason"),
        "bash_calls": calls.len(),
        "rtk_prefixed_calls": rtk_calls,
        "est_tokens": tokens,
        "malformed_lines": parse.malformed_lines,
    });
    let path = paths::mzn_data_dir().join("sessions.jsonl");
    std::fs::create_dir_all(paths::mzn_data_dir())?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    use std::io::Write;
    writeln!(f, "{line}")?;
    Ok(())
}
