//! End-to-end tests of the `mzn` binary against synthetic fixtures.
//!
//! Each test gets its own fake HOME with XDG dirs, a Claude projects
//! directory seeded from `tests/fixtures/sessions`, and optionally a fake
//! `rtk` script. Nothing touches the real user environment.

#![cfg(unix)]

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Fake RTK: `rewrite` covers git/cargo/ls, like the real registry does.
const FAKE_RTK: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "rtk ${FAKE_RTK_VERSION:-0.49.0}" ;;
  gain) exit 0 ;;
  rewrite)
    shift
    cmd="$*"
    case "$cmd" in
      rtk\ *) echo "$cmd"; exit 0 ;;
      git*|cargo*|ls*) echo "rtk $cmd"; exit 3 ;;
      *) exit 1 ;;
    esac ;;
  *) exit 1 ;;
esac
"#;

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    rtk: Option<PathBuf>,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let home = root.join("home");
        let project = root.join("work").join("my-proj");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&project).unwrap();
        Env {
            _tmp: tmp,
            home,
            project,
            rtk: None,
        }
    }

    fn with_rtk(mut self) -> Env {
        let bin = self.home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let rtk = bin.join("rtk");
        fs::write(&rtk, FAKE_RTK).unwrap();
        fs::set_permissions(&rtk, fs::Permissions::from_mode(0o755)).unwrap();
        self.rtk = Some(rtk);
        self
    }

    fn projects_dir(&self) -> PathBuf {
        self.home.join(".claude").join("projects")
    }

    fn session_dir(&self) -> PathBuf {
        let enc: String = self
            .project
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        self.projects_dir().join(enc)
    }

    /// Copy a fixture session into the project's log dir, substituting the
    /// project path.
    fn add_session(&self, fixture: &str, dest: &str) {
        let text = fs::read_to_string(format!("{FIXTURES}/sessions/{fixture}")).unwrap();
        let text = text.replace("__PROJECT__", &self.project.to_string_lossy());
        let path = self.session_dir().join(dest);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn user_settings(&self) -> PathBuf {
        self.home.join(".claude").join("settings.json")
    }

    fn project_settings(&self) -> PathBuf {
        self.project.join(".claude").join("settings.json")
    }

    fn write(&self, path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn mzn(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mzn"));
        cmd.args(args)
            .current_dir(&self.project)
            .env_clear()
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("PATH", "/usr/bin:/bin")
            .env("RTK_DB_PATH", self.home.join("rtk-history.db"))
            .env(
                "MZN_RTK_BIN",
                self.rtk
                    .clone()
                    .unwrap_or_else(|| self.home.join("no-such-rtk")),
            );
        cmd.output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.mzn(args);
        assert!(
            out.status.success(),
            "mzn {args:?} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.ok(args)).unwrap()
    }
}

fn row<'a>(rows: &'a Value, key: &str) -> &'a Value {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == key)
        .unwrap_or_else(|| panic!("no row {key} in {rows:#}"))
}

// ---------------------------------------------------------------- analyze

#[test]
fn analyze_normal_run_with_rtk() {
    let env = Env::new().with_rtk();
    env.add_session("normal.jsonl", "s1.jsonl");
    let a = env.json(&["analyze", "--json", "--days", "36500"]);
    assert_eq!(a["totals"]["bash_calls"], 6);
    assert_eq!(a["coverage_source"], "command_prefix");
    let build = row(&a["commands"], "./scripts/build.sh");
    assert_eq!(build["runs"], 3);
    assert_eq!(build["coverage"], "uncovered");
    assert_eq!(row(&a["commands"], "git status")["coverage"], "missed");
    assert_eq!(a["waste"][0]["key"], "./scripts/build.sh");
}

#[test]
fn analyze_rtk_wrapped_and_rerun() {
    let env = Env::new().with_rtk();
    env.add_session("rerun_after_rtk.jsonl", "s2.jsonl");
    let a = env.json(&["analyze", "--json", "--days", "36500"]);
    assert_eq!(a["totals"]["rtk_calls"], 2);
    let reruns = a["reruns"].as_array().unwrap();
    assert_eq!(reruns.len(), 1);
    assert_eq!(reruns[0]["key"], "cargo test");
    assert_eq!(reruns[0]["gap"], 1);
    assert_eq!(row(&a["commands"], "cargo test")["rtk_runs"], 1);
}

#[test]
fn analyze_tolerates_malformed_lines() {
    let env = Env::new().with_rtk();
    env.add_session("malformed.jsonl", "s3.jsonl");
    let a = env.json(&["analyze", "--json", "--days", "36500"]);
    assert_eq!(a["totals"]["bash_calls"], 1);
    assert!(a["parse"]["malformed_lines"].as_u64().unwrap() >= 3);
    assert_eq!(a["parse"]["orphan_results"], 1);
    let text = env.ok(&["analyze", "--days", "36500"]);
    assert!(text.contains("malformed log lines"));
}

#[test]
fn analyze_counts_subagents() {
    let env = Env::new().with_rtk();
    env.add_session("normal.jsonl", "s1.jsonl");
    env.add_session("subagent.jsonl", "s1/subagents/agent-1.jsonl");
    let a = env.json(&["analyze", "--json", "--days", "36500"]);
    assert_eq!(a["totals"]["bash_calls"], 7);
    assert_eq!(a["totals"]["sidechain_calls"], 1);
    assert_eq!(
        row(&a["commands"], "./scripts/build.sh")["sidechain_runs"],
        1
    );
}

#[test]
fn analyze_without_rtk() {
    let env = Env::new();
    env.add_session("normal.jsonl", "s1.jsonl");
    let a = env.json(&["analyze", "--json", "--days", "36500"]);
    assert_eq!(a["coverage_source"], "none");
    assert_eq!(row(&a["commands"], "git status")["coverage"], "unknown");
    assert!(a["waste"].as_array().unwrap().is_empty());
}

#[test]
fn analyze_empty_project() {
    let env = Env::new().with_rtk();
    let text = env.ok(&["analyze"]);
    assert!(text.contains("No Bash calls found"));
}

#[test]
fn analyze_uses_hook_decisions() {
    let env = Env::new().with_rtk();
    env.add_session("normal.jsonl", "s1.jsonl");
    let db = rusqlite::Connection::open(env.home.join("rtk-history.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE hook_decisions (id INTEGER PRIMARY KEY, timestamp TEXT, session_id TEXT, \
         tool_use_id TEXT, project_path TEXT, raw_cmd TEXT, decision TEXT, rewritten_cmd TEXT, rtk_version TEXT);
         INSERT INTO hook_decisions (timestamp, session_id, tool_use_id, raw_cmd, decision, rewritten_cmd, rtk_version)
         VALUES ('2026-09-20T10:00:00Z', '11111111-1111-4111-8111-111111111111', 't01', 'git status', 'allow', 'rtk git status', '0.49.0');",
    )
    .unwrap();
    drop(db);
    let a = env.json(&["analyze", "--json", "--days", "36500"]);
    assert_eq!(a["coverage_source"], "hook_decisions");
    assert_eq!(row(&a["commands"], "git status")["coverage"], "rtk");
}

// ---------------------------------------------------------------- suggest

#[test]
fn suggest_dry_run_apply_and_idempotence() {
    let env = Env::new().with_rtk();
    env.add_session("normal.jsonl", "s1.jsonl");
    let filters = env.project.join(".rtk/filters.toml");
    let user = "schema_version = 1\n\n# my filter\n[filters.mine]\nmatch_command = \"^foo\\\\b\"\nmax_lines = 5\n";
    env.write(&filters, user);

    let dry = env.ok(&["suggest", "--days", "36500"]);
    assert!(dry.contains("mzn-scripts-build-sh"), "{dry}");
    assert!(dry.contains("+[filters.mzn-scripts-build-sh]"));
    assert_eq!(
        fs::read_to_string(&filters).unwrap(),
        user,
        "dry run must not write"
    );

    env.ok(&["suggest", "--days", "36500", "--apply"]);
    let written = fs::read_to_string(&filters).unwrap();
    assert!(written.starts_with(user), "user content must be untouched");
    assert!(written.contains("# >>> managed by mzn >>>"));
    let parsed: toml::Table = written.parse().unwrap();
    let f = &parsed["filters"]["mzn-scripts-build-sh"];
    assert_eq!(
        f["match_command"].as_str().unwrap(),
        r"^\./scripts/build\.sh(\s|$)"
    );

    let again = env.ok(&["suggest", "--days", "36500", "--apply"]);
    assert!(again.contains("already up to date"));
    assert_eq!(fs::read_to_string(&filters).unwrap(), written);

    env.ok(&["suggest", "--clear"]);
    assert_eq!(fs::read_to_string(&filters).unwrap(), format!("{user}\n"));
}

#[test]
fn suggest_respects_user_filters() {
    let env = Env::new().with_rtk();
    env.add_session("normal.jsonl", "s1.jsonl");
    let filters = env.project.join(".rtk/filters.toml");
    env.write(
        &filters,
        "schema_version = 1\n[filters.build]\nmatch_command = \"^\\\\./scripts/build\"\n",
    );
    let out = env.ok(&["suggest", "--days", "36500", "--apply"]);
    assert!(
        out.contains("your filter `build` already matches it"),
        "{out}"
    );
    assert!(!fs::read_to_string(&filters).unwrap().contains("mzn-"));
}

#[test]
fn suggest_creates_file_when_missing() {
    let env = Env::new().with_rtk();
    env.add_session("normal.jsonl", "s1.jsonl");
    env.ok(&["suggest", "--days", "36500", "--apply"]);
    let written = fs::read_to_string(env.project.join(".rtk/filters.toml")).unwrap();
    assert!(written.contains("schema_version = 1"));
}

#[test]
fn suggest_apply_keeps_earlier_managed_filters() {
    let env = Env::new().with_rtk();
    env.add_session("normal.jsonl", "s1.jsonl");
    let filters = env.project.join(".rtk/filters.toml");
    env.write(
        &filters,
        "schema_version = 1\n\n# >>> managed by mzn >>>\n[filters.mzn-old-tool]\nmatch_command = \"^old-tool(\\\\s|$)\"\nstrip_lines_matching = [\"^\\\\s*$\"]\n# <<< managed by mzn <<<\n",
    );
    env.ok(&["suggest", "--days", "36500", "--apply"]);
    let parsed: toml::Table = fs::read_to_string(&filters).unwrap().parse().unwrap();
    let names: Vec<&String> = parsed["filters"].as_table().unwrap().keys().collect();
    assert!(names.iter().any(|n| *n == "mzn-old-tool"), "{names:?}");
    assert!(
        names.iter().any(|n| *n == "mzn-scripts-build-sh"),
        "{names:?}"
    );
}

#[test]
fn writes_refused_outside_supported_rtk_range() {
    let env = Env::new().with_rtk();
    env.add_session("normal.jsonl", "s1.jsonl");
    fs::write(
        env.rtk.as_ref().unwrap(),
        FAKE_RTK.replace("0.49.0", "0.30.0"),
    )
    .unwrap();
    let out = env.mzn(&["suggest", "--days", "36500", "--apply"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("read-only"));
    assert!(!env.project.join(".rtk/filters.toml").exists());
    let out = env.mzn(&["activate", "--yes"]);
    assert!(!out.status.success());
    assert!(!env.user_settings().exists());
    // Analysis still works.
    env.ok(&["analyze", "--days", "36500"]);
}

// ---------------------------------------------------------------- doctor

fn findings(v: &Value) -> Vec<(String, String)> {
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["level"].as_str().unwrap().to_string(),
                f["message"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn doctor_no_rtk() {
    let env = Env::new();
    let d = env.json(&["doctor", "--json"]);
    assert_eq!(d["rtk"]["mode"], "missing");
    assert!(
        findings(&d)
            .iter()
            .any(|(l, m)| l == "error" && m.contains("not found"))
    );
}

#[test]
fn doctor_rtk_without_hook() {
    let env = Env::new().with_rtk();
    let d = env.json(&["doctor", "--json"]);
    assert_eq!(d["rtk"]["mode"], "full");
    assert!(
        findings(&d)
            .iter()
            .any(|(l, m)| l == "warn" && m.contains("no Claude Code hook"))
    );
}

#[test]
fn doctor_hook_in_global_settings() {
    let env = Env::new().with_rtk();
    env.write(
        &env.user_settings(),
        &fs::read_to_string(format!("{FIXTURES}/settings/global_with_rtk_hook.json")).unwrap(),
    );
    let d = env.json(&["doctor", "--json"]);
    assert!(
        findings(&d)
            .iter()
            .any(|(l, m)| l == "ok" && m.contains("one RTK hook") && m.contains("user settings"))
    );
}

#[test]
fn doctor_hook_in_project_settings() {
    let env = Env::new().with_rtk();
    env.write(
        &env.project_settings(),
        &fs::read_to_string(format!("{FIXTURES}/settings/project_with_legacy_hook.json")).unwrap(),
    );
    let d = env.json(&["doctor", "--json"]);
    let f = findings(&d);
    assert!(f.iter().any(|(_, m)| m.contains("project settings")));
    assert!(f.iter().any(|(_, m)| m.contains("legacy rtk-rewrite.sh")));
    assert!(f.iter().any(|(_, m)| m.contains("other PreToolUse hook")));
}

#[test]
fn doctor_detects_old_and_foreign_rtk() {
    let env = Env::new().with_rtk();
    let mut cmd_env = env.mzn(&["doctor", "--json"]);
    let d: Value = serde_json::from_slice(&cmd_env.stdout).unwrap();
    assert_eq!(d["rtk"]["mode"], "full");

    // Rust Type Kit answers --version but has no `gain`.
    let fake = env.rtk.as_ref().unwrap();
    fs::write(
        fake,
        "#!/bin/sh\n[ \"$1\" = --version ] && echo 'rtk 0.5.0' && exit 0\nexit 2\n",
    )
    .unwrap();
    cmd_env = env.mzn(&["doctor", "--json"]);
    let d: Value = serde_json::from_slice(&cmd_env.stdout).unwrap();
    assert_eq!(d["rtk"]["mode"], "not_rtk");

    fs::write(fake, FAKE_RTK.replace("0.49.0", "0.30.0")).unwrap();
    cmd_env = env.mzn(&["doctor", "--json"]);
    let d: Value = serde_json::from_slice(&cmd_env.stdout).unwrap();
    assert_eq!(d["rtk"]["mode"], "degraded");
}

#[test]
fn doctor_lerim_duplicate_mcp() {
    let env = Env::new().with_rtk();
    let key = env.project.to_string_lossy().to_string();
    env.write(
        &env.home.join(".claude.json"),
        &serde_json::json!({
            "mcpServers": {"lerim": {"type": "stdio", "command": "python"}},
            "projects": {key: {"mcpServers": {"lerim": {"type": "stdio", "command": "python"}}}}
        })
        .to_string(),
    );
    let d = env.json(&["doctor", "--json"]);
    assert!(
        findings(&d)
            .iter()
            .any(|(l, m)| l == "warn" && m.contains("registered 2 times"))
    );
}

// ---------------------------------------------------------------- activate / deactivate

/// Duplicate RTK hooks: native in user settings, legacy in project settings.
fn env_with_duplicate_hooks() -> (Env, Vec<u8>, Vec<u8>) {
    let env = Env::new().with_rtk();
    // Deliberately odd formatting so an exact restore is observable.
    let user = fs::read(format!("{FIXTURES}/settings/global_with_rtk_hook.json")).unwrap();
    let project = fs::read(format!("{FIXTURES}/settings/project_with_legacy_hook.json")).unwrap();
    env.write(&env.user_settings(), std::str::from_utf8(&user).unwrap());
    env.write(
        &env.project_settings(),
        std::str::from_utf8(&project).unwrap(),
    );
    (env, user, project)
}

fn pre_tool_use_commands(path: &Path) -> Vec<String> {
    let v: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    v["hooks"]["PreToolUse"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .flat_map(|g| g["hooks"].as_array().unwrap().iter())
                .map(|h| h["command"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn activate_requires_confirmation() {
    let (env, user, project) = env_with_duplicate_hooks();
    let out = env.mzn(&["activate"]);
    assert!(!out.status.success());
    assert_eq!(fs::read(env.user_settings()).unwrap(), user);
    assert_eq!(fs::read(env.project_settings()).unwrap(), project);
}

#[test]
fn activate_deactivate_round_trip_is_byte_identical() {
    let (env, user, project) = env_with_duplicate_hooks();
    env.ok(&["activate", "--yes"]);

    assert_eq!(
        pre_tool_use_commands(&env.user_settings()),
        vec!["rtk hook claude"]
    );
    assert_eq!(
        pre_tool_use_commands(&env.project_settings()),
        vec!["./tools/audit.sh"]
    );
    let v: Value = serde_json::from_slice(&fs::read(env.user_settings()).unwrap()).unwrap();
    assert!(
        v["hooks"]["SessionEnd"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("hook session-end")
    );
    assert!(env.home.join(".config/mzn/state.json").is_file());

    env.ok(&["deactivate", "--yes"]);
    assert_eq!(fs::read(env.user_settings()).unwrap(), user);
    assert_eq!(fs::read(env.project_settings()).unwrap(), project);
    assert!(!env.home.join(".config/mzn/state.json").exists());
}

#[test]
fn activate_twice_changes_nothing() {
    let (env, _, _) = env_with_duplicate_hooks();
    env.ok(&["activate", "--yes"]);
    let user = fs::read(env.user_settings()).unwrap();
    let project = fs::read(env.project_settings()).unwrap();
    let state = fs::read(env.home.join(".config/mzn/state.json")).unwrap();
    let out = env.ok(&["activate", "--yes"]);
    assert!(out.contains("Already active"));
    assert_eq!(fs::read(env.user_settings()).unwrap(), user);
    assert_eq!(fs::read(env.project_settings()).unwrap(), project);
    assert_eq!(
        fs::read(env.home.join(".config/mzn/state.json")).unwrap(),
        state
    );
}

#[test]
fn activate_creates_missing_settings_and_deactivate_removes_it() {
    let env = Env::new().with_rtk();
    env.ok(&["activate", "--yes"]);
    assert!(env.user_settings().is_file());
    env.ok(&["deactivate", "--yes"]);
    assert!(!env.user_settings().exists());
}

#[test]
fn deactivate_merges_user_edits() {
    let (env, _, _) = env_with_duplicate_hooks();
    env.ok(&["activate", "--yes"]);

    // The user edits both files after activation.
    let mut v: Value = serde_json::from_slice(&fs::read(env.user_settings()).unwrap()).unwrap();
    v["theme"] = "dark".into();
    env.write(&env.user_settings(), &serde_json::to_string(&v).unwrap());
    let mut p: Value = serde_json::from_slice(&fs::read(env.project_settings()).unwrap()).unwrap();
    p["permissions"]["allow"]
        .as_array_mut()
        .unwrap()
        .push("Bash(make)".into());
    env.write(&env.project_settings(), &serde_json::to_string(&p).unwrap());

    // Merging needs confirmation.
    let out = env.mzn(&["deactivate"]);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("changed since activation"), "{stdout}");

    env.ok(&["deactivate", "--yes"]);
    let v: Value = serde_json::from_slice(&fs::read(env.user_settings()).unwrap()).unwrap();
    assert_eq!(v["theme"], "dark", "user edit kept");
    assert!(v["hooks"].get("SessionEnd").is_none(), "our hook removed");
    assert_eq!(
        pre_tool_use_commands(&env.user_settings()),
        vec!["rtk hook claude"]
    );
    let p: Value = serde_json::from_slice(&fs::read(env.project_settings()).unwrap()).unwrap();
    assert_eq!(p["permissions"]["allow"][1], "Bash(make)", "user edit kept");
    let cmds = pre_tool_use_commands(&env.project_settings());
    assert!(
        cmds.contains(&"~/.claude/hooks/rtk-rewrite.sh".to_string()),
        "RTK hook restored"
    );
    assert!(cmds.contains(&"./tools/audit.sh".to_string()));
}

#[test]
fn deactivate_when_inactive() {
    let env = Env::new();
    assert!(env.ok(&["deactivate"]).contains("not active"));
}

// ---------------------------------------------------------------- report / hook

#[test]
fn report_text_and_markdown() {
    let env = Env::new().with_rtk();
    env.add_session("normal.jsonl", "s1.jsonl");
    let text = env.ok(&["report", "--days", "36500"]);
    assert!(text.contains("Bash calls"));
    assert!(text.contains("./scripts/build.sh"));
    let md = env.ok(&["report", "--days", "36500", "--markdown"]);
    assert!(md.starts_with("## mzn report"));
}

#[test]
fn session_end_hook_records_and_never_fails() {
    let env = Env::new();
    env.add_session("normal.jsonl", "s1.jsonl");
    let transcript = env.session_dir().join("s1.jsonl");
    let mut child = Command::new(env!("CARGO_BIN_EXE_mzn"))
        .args(["hook", "session-end"])
        .env_clear()
        .env("HOME", &env.home)
        .env("XDG_DATA_HOME", env.home.join(".local/share"))
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            serde_json::json!({"session_id": "s1", "transcript_path": transcript, "cwd": env.project, "reason": "other"})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    assert!(child.wait().unwrap().success());
    let log = fs::read_to_string(env.home.join(".local/share/mzn/sessions.jsonl")).unwrap();
    let v: Value = serde_json::from_str(log.trim()).unwrap();
    assert_eq!(v["bash_calls"], 6);

    // Garbage input: still exits 0.
    let out = Command::new(env!("CARGO_BIN_EXE_mzn"))
        .args(["hook", "session-end"])
        .env_clear()
        .env("HOME", &env.home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success());
}
