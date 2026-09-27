//! `mzn doctor`: report RTK, hook, filter and Lerim state without changing
//! anything.

use crate::paths;
use crate::rtk::db::{RtkDb, db_path};
use crate::rtk::filters::{FilterFile, Trust};
use crate::rtk::{self, Mode, RtkInfo};
use crate::settings::{self, HookKind, SettingsScan};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Ok,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub level: Level,
    pub area: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LerimInfo {
    pub binary: Option<PathBuf>,
    pub store: Option<PathBuf>,
    /// Files that register an MCP server named `lerim`.
    pub mcp_entries: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub project: PathBuf,
    pub rtk: RtkInfo,
    pub rtk_db: Option<PathBuf>,
    pub rtk_db_has_hook_decisions: bool,
    pub hooks: SettingsScan,
    pub project_filters: FilterFile,
    pub global_filters: FilterFile,
    pub lerim: LerimInfo,
    pub mzn_active: bool,
    pub findings: Vec<Finding>,
}

fn push(f: &mut Vec<Finding>, level: Level, area: &'static str, msg: String, fix: Option<&str>) {
    f.push(Finding {
        level,
        area,
        message: msg,
        fix: fix.map(str::to_string),
    });
}

pub fn run(project: &Path) -> Report {
    let info = rtk::detect();
    let hooks = settings::scan(project);
    let project_filters = FilterFile::load(&paths::rtk_project_filters(project));
    let global_filters = FilterFile::load(&paths::rtk_global_filters());
    let lerim = detect_lerim(project);
    let db = RtkDb::open();
    let has_decisions = db.as_ref().is_some_and(|d| d.has_table("hook_decisions"));
    let mzn_active = settings::load_state().ok().flatten().is_some();

    let mut f = Vec::new();

    // RTK binary
    match info.mode {
        Mode::Full => push(&mut f, Level::Ok, "rtk", info.describe(), None),
        Mode::Degraded | Mode::Untested => push(
            &mut f,
            Level::Warn,
            "rtk",
            info.describe(),
            Some("upgrade RTK: rtk upgrade, brew upgrade rtk, or re-run the installer"),
        ),
        Mode::TooOld | Mode::NotRtk | Mode::Missing => push(
            &mut f,
            Level::Error,
            "rtk",
            info.describe(),
            Some(rtk::INSTALL_HINT),
        ),
    }

    // Hooks
    let rtk_hooks = hooks.rtk_hooks();
    for (file, err) in &hooks.unreadable {
        push(
            &mut f,
            Level::Error,
            "hooks",
            format!("cannot parse {}: {err}", file.display()),
            None,
        );
    }
    match rtk_hooks.len() {
        0 if matches!(info.mode, Mode::Full | Mode::Degraded | Mode::Untested) => push(
            &mut f,
            Level::Warn,
            "hooks",
            "RTK is installed but no Claude Code hook sends Bash commands through it".into(),
            Some("rtk init -g"),
        ),
        0 => {}
        1 => {
            let h = rtk_hooks[0];
            push(
                &mut f,
                Level::Ok,
                "hooks",
                format!(
                    "one RTK hook: `{}` in {} settings ({})",
                    h.command,
                    h.scope.label(),
                    h.file.display()
                ),
                None,
            );
            if h.kind == HookKind::RtkLegacy {
                push(
                    &mut f,
                    Level::Info,
                    "hooks",
                    "legacy rtk-rewrite.sh hook: RTK does not log hook decisions for it, so coverage is approximate".into(),
                    Some("rtk init -g   (migrates to `rtk hook claude`)"),
                );
            }
        }
        n => push(
            &mut f,
            Level::Warn,
            "hooks",
            format!(
                "{n} RTK hooks on Bash ({}). Claude Code runs them in parallel and which rewrite wins is undefined",
                rtk_hooks
                    .iter()
                    .map(|h| format!("{}: `{}`", h.scope.label(), h.command))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Some("mzn activate   (keeps exactly one; reversible with mzn deactivate)"),
        ),
    }
    let others: Vec<_> = hooks
        .bash_pre_tool_use()
        .filter(|h| h.kind == HookKind::Other)
        .collect();
    if !others.is_empty() && !rtk_hooks.is_empty() {
        push(
            &mut f,
            Level::Info,
            "hooks",
            format!(
                "{} other PreToolUse hook(s) also match Bash ({}). If any of them rewrites commands (updatedInput), it races with RTK",
                others.len(),
                others
                    .iter()
                    .map(|h| format!("`{}`", h.command))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None,
        );
    }
    if !rtk_hooks.is_empty() && matches!(info.mode, Mode::Missing | Mode::NotRtk) {
        push(
            &mut f,
            Level::Error,
            "hooks",
            "an RTK hook is configured but a working rtk binary is not on PATH".into(),
            Some(rtk::INSTALL_HINT),
        );
    }
    for file in &hooks.disable_all_hooks {
        push(
            &mut f,
            Level::Warn,
            "hooks",
            format!("disableAllHooks is set in {}: no hook runs", file.display()),
            None,
        );
    }
    if std::env::var("CLAUDE_CODE_REMOTE").is_ok_and(|v| v == "true") {
        push(
            &mut f,
            Level::Info,
            "hooks",
            "cloud session: Claude Code on the web ignores ~/.claude/settings.json; only project hooks apply here".into(),
            None,
        );
    }

    // Stats DB
    match &db {
        Some(_) if has_decisions => push(
            &mut f,
            Level::Ok,
            "stats",
            format!(
                "RTK stats: {} (hook decisions available)",
                db_path().display()
            ),
            None,
        ),
        Some(_) => push(
            &mut f,
            Level::Info,
            "stats",
            format!(
                "RTK stats: {} (no hook_decisions table; needs RTK >= {})",
                db_path().display(),
                rtk::MIN_FULL
            ),
            None,
        ),
        None if info.mode != Mode::Missing => push(
            &mut f,
            Level::Info,
            "stats",
            format!("no RTK stats database yet at {}", db_path().display()),
            None,
        ),
        None => {}
    }

    // Filters
    for (label, ff) in [("project", &project_filters), ("global", &global_filters)] {
        if !ff.exists {
            continue;
        }
        if let Some(e) = &ff.parse_error {
            push(
                &mut f,
                Level::Error,
                "filters",
                format!("{label} filters {} do not parse: {e}", ff.path.display()),
                None,
            );
            continue;
        }
        let summary = format!(
            "{label} filters {}: {} yours, {} managed by mzn",
            ff.path.display(),
            ff.user_filters.len(),
            ff.managed_filters.len()
        );
        match ff.trust {
            Trust::Trusted => push(
                &mut f,
                Level::Ok,
                "filters",
                format!("{summary}, trusted"),
                None,
            ),
            Trust::Untrusted | Trust::Missing => push(
                &mut f,
                Level::Warn,
                "filters",
                format!("{summary}, NOT trusted: RTK silently ignores this file"),
                Some("rtk trust"),
            ),
            Trust::Changed => push(
                &mut f,
                Level::Warn,
                "filters",
                format!("{summary}, changed since `rtk trust`: RTK ignores it until re-trusted"),
                Some("rtk trust"),
            ),
        }
    }

    // Lerim
    if lerim.binary.is_some() || lerim.store.is_some() || !lerim.mcp_entries.is_empty() {
        let mut parts = Vec::new();
        if let Some(b) = &lerim.binary {
            parts.push(format!("binary {}", b.display()));
        }
        if let Some(s) = &lerim.store {
            parts.push(format!("store {}", s.display()));
        }
        push(
            &mut f,
            Level::Info,
            "lerim",
            format!(
                "Lerim found ({}); mzn reuses it read-only",
                parts.join(", ")
            ),
            None,
        );
        if lerim.mcp_entries.len() > 1 {
            push(
                &mut f,
                Level::Warn,
                "lerim",
                format!(
                    "Lerim MCP server registered {} times ({}): Claude Code may start it twice",
                    lerim.mcp_entries.len(),
                    lerim.mcp_entries.join(", ")
                ),
                Some("keep one entry; `lerim connect` manages ~/.claude.json"),
            );
        }
    }

    if mzn_active {
        push(
            &mut f,
            Level::Info,
            "mzn",
            format!(
                "mzn activate is in effect (state: {}); undo with `mzn deactivate`",
                paths::mzn_state_file().display()
            ),
            None,
        );
    }

    Report {
        project: project.to_path_buf(),
        rtk_db: db.is_some().then(db_path),
        rtk_db_has_hook_decisions: has_decisions,
        rtk: info,
        hooks,
        project_filters,
        global_filters,
        lerim,
        mzn_active,
        findings: f,
    }
}

fn detect_lerim(project: &Path) -> LerimInfo {
    let store = paths::lerim_dir().join("context.sqlite3");
    let mut mcp = Vec::new();
    let has_lerim = |v: &Value| v.get("mcpServers").and_then(|s| s.get("lerim")).is_some();
    let claude_json = paths::claude_json();
    if let Ok(Some(v)) = settings::read_json(&claude_json) {
        if has_lerim(&v) {
            mcp.push(format!("{} (user)", claude_json.display()));
        }
        let key = project.to_string_lossy();
        if let Some(p) = v.get("projects").and_then(|p| p.get(key.as_ref()))
            && has_lerim(p)
        {
            mcp.push(format!("{} (project {})", claude_json.display(), key));
        }
    }
    let mcp_json = project.join(".mcp.json");
    if let Ok(Some(v)) = settings::read_json(&mcp_json)
        && has_lerim(&v)
    {
        mcp.push(mcp_json.display().to_string());
    }
    LerimInfo {
        binary: paths::which("lerim"),
        store: store.is_file().then_some(store),
        mcp_entries: mcp,
    }
}

pub fn render_text(r: &Report) -> String {
    let mut s = format!("mzn doctor — {}\n\n", r.project.display());
    for f in &r.findings {
        let tag = match f.level {
            Level::Ok => "[ok]  ",
            Level::Info => "[info]",
            Level::Warn => "[warn]",
            Level::Error => "[err] ",
        };
        s.push_str(&format!("{tag} {:<7} {}\n", f.area, f.message));
        if let Some(fix) = &f.fix {
            s.push_str(&format!("              fix: {fix}\n"));
        }
    }
    let worst = r
        .findings
        .iter()
        .map(|f| f.level)
        .max()
        .unwrap_or(Level::Ok);
    s.push_str(match worst {
        Level::Ok | Level::Info => "\nAll good. Nothing was changed.\n",
        Level::Warn => "\nSome warnings. Nothing was changed.\n",
        Level::Error => "\nProblems found. Nothing was changed.\n",
    });
    s
}
