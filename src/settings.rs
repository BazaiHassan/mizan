//! Claude Code settings files: finding hooks, and the reversible edits made
//! by `mzn activate` / `mzn deactivate`.
//!
//! `activate` guarantees exactly one RTK rewrite hook on Bash (Claude Code
//! runs matching hooks in parallel, and which `updatedInput` wins is
//! undefined) and optionally adds mzn's quiet `SessionEnd` hook. RTK's own
//! hook stays in charge of rewriting. Every touched file is backed up and
//! every change is recorded in `state.json`, so `deactivate` can restore the
//! exact previous bytes or, if the user edited the file since, merge the
//! reversal into the current content and show a diff.

use crate::paths;
use crate::util::{atomic_write, sha256_hex};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

pub const SESSION_END_COMMAND: &str = "mzn hook session-end";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    User,
    Project,
    Local,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
            Scope::Local => "project-local",
        }
    }
}

pub fn settings_files(project: &Path) -> Vec<(Scope, PathBuf)> {
    vec![
        (Scope::User, paths::user_settings()),
        (Scope::Project, paths::project_settings(project)),
        (Scope::Local, paths::project_local_settings(project)),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    /// `rtk hook claude` (RTK ≥ 0.3x native hook).
    RtkNative,
    /// `~/.claude/hooks/rtk-rewrite.sh` (legacy shell hook).
    RtkLegacy,
    Mzn,
    Other,
}

pub fn hook_kind(command: &str) -> HookKind {
    let c = command.trim();
    let words: Vec<&str> = c.split_whitespace().collect();
    let bin_is = |w: &str, name: &str| {
        let w = w.trim_matches(|ch| ch == '"' || ch == '\'');
        w == name
            || w.ends_with(&format!("/{name}"))
            || w.ends_with(&format!("\\{name}"))
            || w.ends_with(&format!("\\{name}.exe"))
    };
    if c.contains("rtk-rewrite.sh") {
        HookKind::RtkLegacy
    } else if words.len() >= 3
        && bin_is(words[0], "rtk")
        && words[1] == "hook"
        && words[2] == "claude"
    {
        HookKind::RtkNative
    } else if words.len() >= 2 && bin_is(words[0], "mzn") && words[1] == "hook" {
        HookKind::Mzn
    } else {
        HookKind::Other
    }
}

/// Does a hook group's matcher apply to the Bash tool?
fn matcher_covers_bash(group: &Value) -> bool {
    match group.get("matcher") {
        None | Some(Value::Null) => true,
        Some(Value::String(m)) => {
            let m = m.trim();
            if m.is_empty() || m == "*" {
                return true;
            }
            if m.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_- ,|".contains(c))
            {
                return m.split(['|', ',']).any(|n| n.trim() == "Bash");
            }
            regex::Regex::new(&format!("^(?:{m})$")).is_ok_and(|re| re.is_match("Bash"))
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FoundHook {
    pub scope: Scope,
    pub file: PathBuf,
    pub event: String,
    pub matcher: Option<String>,
    pub command: String,
    pub kind: HookKind,
    #[serde(skip)]
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsScan {
    pub hooks: Vec<FoundHook>,
    pub disable_all_hooks: Vec<PathBuf>,
    pub unreadable: Vec<(PathBuf, String)>,
}

impl SettingsScan {
    pub fn bash_pre_tool_use(&self) -> impl Iterator<Item = &FoundHook> {
        self.hooks.iter().filter(|h| h.event == "PreToolUse")
    }
    pub fn rtk_hooks(&self) -> Vec<&FoundHook> {
        self.bash_pre_tool_use()
            .filter(|h| matches!(h.kind, HookKind::RtkNative | HookKind::RtkLegacy))
            .collect()
    }
}

pub fn read_json(path: &Path) -> Result<Option<Value>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.iter().all(u8::is_ascii_whitespace) {
                return Ok(Some(json!({})));
            }
            let v: Value = serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not valid JSON", path.display()))?;
            Ok(Some(v))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn scan(project: &Path) -> SettingsScan {
    let mut out = SettingsScan {
        hooks: Vec::new(),
        disable_all_hooks: Vec::new(),
        unreadable: Vec::new(),
    };
    for (scope, file) in settings_files(project) {
        let root = match read_json(&file) {
            Ok(Some(v)) => v,
            Ok(None) => continue,
            Err(e) => {
                out.unreadable.push((file, format!("{e:#}")));
                continue;
            }
        };
        if root.get("disableAllHooks").and_then(Value::as_bool) == Some(true) {
            out.disable_all_hooks.push(file.clone());
        }
        let Some(events) = root.get("hooks").and_then(Value::as_object) else {
            continue;
        };
        for (event, groups) in events {
            let Some(groups) = groups.as_array() else {
                continue;
            };
            for group in groups {
                if event == "PreToolUse" && !matcher_covers_bash(group) {
                    continue;
                }
                let matcher = group
                    .get("matcher")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                for hook in group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let Some(command) = hook.get("command").and_then(Value::as_str) else {
                        continue;
                    };
                    out.hooks.push(FoundHook {
                        scope,
                        file: file.clone(),
                        event: event.clone(),
                        matcher: matcher.clone(),
                        command: command.to_string(),
                        kind: hook_kind(command),
                        raw: hook.clone(),
                    });
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// activate / deactivate
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookRef {
    pub event: String,
    pub matcher: Option<String>,
    pub hook: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: PathBuf,
    pub existed_before: bool,
    pub backup: Option<PathBuf>,
    pub sha256_after: String,
    /// False once the file changed between two activations; restoring the
    /// original backup would then lose user edits, so deactivate merges.
    pub exact_restore: bool,
    pub removed: Vec<HookRef>,
    pub added: Vec<HookRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub activated_at: String,
    pub files: Vec<FileEntry>,
}

pub fn load_state() -> Result<Option<State>> {
    let path = paths::mzn_state_file();
    match std::fs::read(&path) {
        Ok(b) => Ok(Some(
            serde_json::from_slice(&b).with_context(|| format!("parsing {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn save_state(state: &State) -> Result<()> {
    let mut s = serde_json::to_string_pretty(state)?;
    s.push('\n');
    atomic_write(&paths::mzn_state_file(), s.as_bytes())
}

#[derive(Debug, Clone)]
pub struct Change {
    pub file: PathBuf,
    pub remove: Vec<HookRef>,
    pub add: Vec<HookRef>,
    pub reasons: Vec<String>,
}

fn change_for(changes: &mut Vec<Change>, file: &Path) -> usize {
    if let Some(i) = changes.iter().position(|c| c.file == file) {
        return i;
    }
    changes.push(Change {
        file: file.to_path_buf(),
        remove: Vec::new(),
        add: Vec::new(),
        reasons: Vec::new(),
    });
    changes.len() - 1
}

/// Compute what `activate` would change. Empty when already active.
pub fn plan_activate(
    project: &Path,
    session_hook: bool,
    hook_command: &str,
) -> Result<Vec<Change>> {
    let scan = scan(project);
    if let Some((file, err)) = scan.unreadable.first() {
        bail!("cannot read {}: {err}", file.display());
    }
    let mut changes: Vec<Change> = Vec::new();

    // Keep one RTK hook: native over legacy, then user > project > local.
    let mut rtk = scan.rtk_hooks();
    rtk.sort_by_key(|h| (h.kind != HookKind::RtkNative, h.scope));
    if rtk.len() > 1 {
        let keep = rtk[0];
        for h in &rtk[1..] {
            let i = change_for(&mut changes, &h.file);
            changes[i].remove.push(HookRef {
                event: h.event.clone(),
                matcher: h.matcher.clone(),
                hook: h.raw.clone(),
            });
            changes[i].reasons.push(format!(
                "disable duplicate RTK hook `{}` ({} settings); keeping `{}` ({} settings)",
                h.command,
                h.scope.label(),
                keep.command,
                keep.scope.label()
            ));
        }
    }

    if session_hook
        && !scan
            .hooks
            .iter()
            .any(|h| h.event == "SessionEnd" && h.kind == HookKind::Mzn)
    {
        let user = paths::user_settings();
        let i = change_for(&mut changes, &user);
        changes[i].add.push(HookRef {
            event: "SessionEnd".into(),
            matcher: None,
            hook: json!({"type": "command", "command": hook_command}),
        });
        changes[i].reasons.push(format!(
            "add SessionEnd hook `{hook_command}` (quiet stats update)"
        ));
    }
    Ok(changes)
}

fn hooks_obj(root: &mut Value) -> &mut Map<String, Value> {
    if !root.is_object() {
        *root = json!({});
    }
    let obj = root.as_object_mut().expect("object");
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    hooks.as_object_mut().expect("object")
}

/// Remove one hook entry; drops groups/events that become empty.
pub fn remove_hook(root: &mut Value, r: &HookRef) -> bool {
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let Some(groups) = hooks.get_mut(&r.event).and_then(Value::as_array_mut) else {
        return false;
    };
    let mut removed = false;
    for group in groups.iter_mut() {
        if group
            .get("matcher")
            .and_then(Value::as_str)
            .map(str::to_string)
            != r.matcher
        {
            continue;
        }
        if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut)
            && let Some(pos) = list.iter().position(|h| h == &r.hook)
        {
            list.remove(pos);
            removed = true;
            break;
        }
    }
    groups.retain(|g| {
        g.get("hooks")
            .and_then(Value::as_array)
            .is_none_or(|l| !l.is_empty())
    });
    if groups.is_empty() {
        hooks.remove(&r.event);
    }
    if hooks.is_empty()
        && let Some(obj) = root.as_object_mut()
    {
        obj.remove("hooks");
    }
    removed
}

/// Add one hook entry (into a group with the same matcher when present).
/// No-op when an identical entry already exists.
pub fn add_hook(root: &mut Value, r: &HookRef) {
    let hooks = hooks_obj(root);
    let groups = hooks.entry(r.event.clone()).or_insert_with(|| json!([]));
    if !groups.is_array() {
        *groups = json!([]);
    }
    let groups = groups.as_array_mut().expect("array");
    for group in groups.iter_mut() {
        if group
            .get("matcher")
            .and_then(Value::as_str)
            .map(str::to_string)
            != r.matcher
        {
            continue;
        }
        if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
            if !list.contains(&r.hook) {
                list.push(r.hook.clone());
            }
            return;
        }
    }
    let mut group = Map::new();
    if let Some(m) = &r.matcher {
        group.insert("matcher".into(), json!(m));
    }
    group.insert("hooks".into(), json!([r.hook.clone()]));
    groups.push(Value::Object(group));
}

pub fn render_json(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).expect("serializable");
    s.push('\n');
    s
}

/// Current bytes and the bytes after applying `change`.
pub fn preview(change: &Change) -> Result<(Option<Vec<u8>>, String)> {
    let before = match std::fs::read(&change.file) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let mut root = match &before {
        Some(b) if !b.iter().all(u8::is_ascii_whitespace) => serde_json::from_slice(b)
            .with_context(|| format!("{} is not valid JSON", change.file.display()))?,
        _ => json!({}),
    };
    for r in &change.remove {
        remove_hook(&mut root, r);
    }
    for r in &change.add {
        add_hook(&mut root, r);
    }
    Ok((before, render_json(&root)))
}

fn backup_name(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn apply_activate(changes: &[Change]) -> Result<State> {
    let now = chrono::Utc::now();
    let mut state = load_state()?.unwrap_or(State {
        version: 1,
        activated_at: now.to_rfc3339(),
        files: Vec::new(),
    });
    let backup_dir = paths::mzn_config_dir()
        .join("backups")
        .join(now.format("%Y%m%dT%H%M%S%.3fZ").to_string());

    for change in changes {
        let (before, after) = preview(change)?;
        let existing = state.files.iter().position(|f| f.path == change.file);
        match existing {
            Some(i) => {
                // Second activation of an already-managed file: the original
                // backup stays; if the user edited in between, merge later.
                let entry = &mut state.files[i];
                let current = before.as_deref().map(sha256_hex).unwrap_or_default();
                if current != entry.sha256_after {
                    entry.exact_restore = false;
                }
                entry.removed.extend(change.remove.iter().cloned());
                entry.added.extend(change.add.iter().cloned());
                entry.sha256_after = sha256_hex(after.as_bytes());
            }
            None => {
                let backup = match &before {
                    Some(bytes) => {
                        std::fs::create_dir_all(&backup_dir)?;
                        let p = backup_dir.join(backup_name(&change.file));
                        std::fs::write(&p, bytes)
                            .with_context(|| format!("writing backup {}", p.display()))?;
                        Some(p)
                    }
                    None => None,
                };
                state.files.push(FileEntry {
                    path: change.file.clone(),
                    existed_before: before.is_some(),
                    backup,
                    sha256_after: sha256_hex(after.as_bytes()),
                    exact_restore: true,
                    removed: change.remove.clone(),
                    added: change.add.clone(),
                });
            }
        }
        // Record intent before touching the file, so a crash is recoverable.
        save_state(&state)?;
        atomic_write(&change.file, after.as_bytes())?;
    }
    Ok(state)
}

#[derive(Debug, Clone)]
pub enum Restore {
    /// Write these exact bytes back (from the backup).
    Exact(Vec<u8>),
    /// File did not exist before activation and is unchanged: delete it.
    Delete,
    /// File changed since activation: reverse our edits on current content.
    Merge { current: String, merged: String },
    /// Nothing to do (e.g. file deleted by the user and nothing to re-add).
    Skip(String),
}

pub fn plan_deactivate(state: &State) -> Result<Vec<(PathBuf, Restore)>> {
    let mut out = Vec::new();
    for entry in &state.files {
        let current = match std::fs::read(&entry.path) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let unchanged =
            current.as_deref().map(sha256_hex).as_deref() == Some(entry.sha256_after.as_str());
        let restore = if unchanged && entry.exact_restore {
            match &entry.backup {
                Some(b) => Restore::Exact(
                    std::fs::read(b).with_context(|| format!("reading backup {}", b.display()))?,
                ),
                None => Restore::Delete,
            }
        } else {
            let text = current
                .as_deref()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            let mut root: Value = if text.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&text).with_context(|| {
                    format!(
                        "{} is not valid JSON; fix it or restore manually",
                        entry.path.display()
                    )
                })?
            };
            for r in &entry.added {
                remove_hook(&mut root, r);
            }
            for r in &entry.removed {
                add_hook(&mut root, r);
            }
            let merged = render_json(&root);
            if current.is_none() && entry.removed.is_empty() {
                Restore::Skip("file was deleted since activation; nothing to restore".into())
            } else if merged == text {
                Restore::Skip("already matches the pre-activation hooks".into())
            } else {
                Restore::Merge {
                    current: text,
                    merged,
                }
            }
        };
        out.push((entry.path.clone(), restore));
    }
    Ok(out)
}

pub fn apply_deactivate(plan: &[(PathBuf, Restore)]) -> Result<()> {
    for (path, r) in plan {
        match r {
            Restore::Exact(bytes) => atomic_write(path, bytes)?,
            Restore::Delete => {
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
            }
            Restore::Merge { merged, .. } => atomic_write(path, merged.as_bytes())?,
            Restore::Skip(_) => {}
        }
    }
    let state_file = paths::mzn_state_file();
    if state_file.exists() {
        std::fs::remove_file(state_file)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_hook_commands() {
        assert_eq!(hook_kind("rtk hook claude"), HookKind::RtkNative);
        assert_eq!(
            hook_kind("/opt/homebrew/bin/rtk hook claude"),
            HookKind::RtkNative
        );
        assert_eq!(
            hook_kind("~/.claude/hooks/rtk-rewrite.sh"),
            HookKind::RtkLegacy
        );
        assert_eq!(hook_kind("mzn hook session-end"), HookKind::Mzn);
        assert_eq!(hook_kind("/usr/bin/other.sh"), HookKind::Other);
        assert_eq!(hook_kind("myrtk hook claude"), HookKind::Other);
    }

    #[test]
    fn matchers() {
        assert!(matcher_covers_bash(&json!({"matcher": "Bash"})));
        assert!(matcher_covers_bash(&json!({"matcher": "Edit|Bash"})));
        assert!(matcher_covers_bash(&json!({})));
        assert!(matcher_covers_bash(&json!({"matcher": "B.*"})));
        assert!(!matcher_covers_bash(&json!({"matcher": "Edit"})));
    }

    #[test]
    fn add_then_remove_round_trips() {
        let original = json!({"model": "x", "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "a"}]}]}});
        let mut v = original.clone();
        let r = HookRef {
            event: "PreToolUse".into(),
            matcher: Some("Bash".into()),
            hook: json!({"type": "command", "command": "rtk hook claude"}),
        };
        add_hook(&mut v, &r);
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        add_hook(&mut v, &r);
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(remove_hook(&mut v, &r));
        assert_eq!(v, original);
    }

    #[test]
    fn remove_last_hook_cleans_up() {
        let r = HookRef {
            event: "SessionEnd".into(),
            matcher: None,
            hook: json!({"type": "command", "command": "mzn hook session-end"}),
        };
        let mut v = json!({"a": 1});
        add_hook(&mut v, &r);
        remove_hook(&mut v, &r);
        assert_eq!(v, json!({"a": 1}));
    }
}
