//! Claude Code session log reader.
//!
//! The JSONL format is undocumented and changes between Claude Code versions,
//! so parsing is deliberately lenient: every line is read as a generic JSON
//! value, unknown fields are ignored, and lines that are not valid JSON are
//! counted and skipped. See `docs/recon.md` §1 for the observed format.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Cap on raw output kept per call for filter generation.
const SAMPLE_CAP: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct BashCall {
    pub session_id: String,
    pub tool_use_id: String,
    pub command: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub cwd: Option<String>,
    /// Ran inside a subagent (sidechain) rather than the main conversation.
    pub sidechain: bool,
    /// Characters of tool result the model actually received.
    pub seen_chars: u64,
    /// Characters the command printed (stdout + stderr), when recorded.
    pub raw_chars: u64,
    pub is_error: bool,
    pub has_result: bool,
    /// Output was too large and spilled to a file; the model saw a preview.
    pub persisted: bool,
    #[serde(skip)]
    pub output_sample: Option<String>,
    #[serde(skip)]
    pub file: PathBuf,
    /// Position of the call within its file, for "within N turns" checks.
    pub seq: usize,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ParseStats {
    pub files: usize,
    pub lines: usize,
    pub malformed_lines: usize,
    pub bash_calls: usize,
    pub calls_without_result: usize,
    pub orphan_results: usize,
}

impl ParseStats {
    pub fn merge(&mut self, other: &ParseStats) {
        self.files += other.files;
        self.lines += other.lines;
        self.malformed_lines += other.malformed_lines;
        self.bash_calls += other.bash_calls;
        self.calls_without_result += other.calls_without_result;
        self.orphan_results += other.orphan_results;
    }
}

/// Every session file (including subagent transcripts) that belongs to
/// `project`. Directories are matched by Claude Code's path encoding; for
/// directories that could belong to a sibling project (a shared prefix),
/// each call's `cwd` is checked later by [`load_project`].
pub fn session_files(projects_dir: &Path, project: &Path) -> Vec<(PathBuf, bool)> {
    let enc = crate::paths::encode_project(project);
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(projects_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let exact = name == enc;
        if !exact && !name.starts_with(&format!("{enc}-")) {
            continue;
        }
        let mut files = Vec::new();
        collect_jsonl(&entry.path(), &mut files, 0);
        files.sort();
        out.extend(files.into_iter().map(|f| (f, exact)));
    }
    out
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if path.file_name().is_some_and(|n| n == "tool-results") {
                continue;
            }
            collect_jsonl(&path, out, depth + 1);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}

/// Load all Bash calls for a project, newer than `since` when given.
pub fn load_project(
    projects_dir: &Path,
    project: &Path,
    since: Option<DateTime<Utc>>,
) -> (Vec<BashCall>, ParseStats) {
    let mut stats = ParseStats::default();
    let mut calls = Vec::new();
    for (file, exact_dir) in session_files(projects_dir, project) {
        let (file_calls, file_stats) = parse_file(&file);
        stats.merge(&file_stats);
        for call in file_calls {
            let in_project = match &call.cwd {
                Some(cwd) => Path::new(cwd).starts_with(project),
                None => exact_dir,
            };
            if !in_project {
                continue;
            }
            if let (Some(since), Some(ts)) = (since, call.timestamp)
                && ts < since
            {
                continue;
            }
            calls.push(call);
        }
    }
    calls.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.file.cmp(&b.file))
            .then(a.seq.cmp(&b.seq))
    });
    (calls, stats)
}

/// Parse one session file. Never fails: unreadable files yield no calls.
pub fn parse_file(path: &Path) -> (Vec<BashCall>, ParseStats) {
    let mut stats = ParseStats {
        files: 1,
        ..Default::default()
    };
    let Ok(file) = std::fs::File::open(path) else {
        return (Vec::new(), stats);
    };
    let mut reader = BufReader::new(file);
    let mut calls: Vec<BashCall> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut buf = Vec::new();
    let fallback_session = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => {
                stats.malformed_lines += 1;
                break;
            }
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        stats.lines += 1;
        let v = match serde_json::from_str::<Value>(line) {
            Ok(v) if v.is_object() => v,
            _ => {
                stats.malformed_lines += 1;
                continue;
            }
        };
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                read_tool_uses(&v, path, &fallback_session, &mut calls, &mut index)
            }
            Some("user") => read_tool_results(&v, &mut calls, &index, &mut stats),
            _ => {}
        }
    }
    stats.bash_calls = calls.len();
    stats.calls_without_result = calls.iter().filter(|c| !c.has_result).count();
    (calls, stats)
}

fn read_tool_uses(
    v: &Value,
    path: &Path,
    fallback_session: &str,
    calls: &mut Vec<BashCall>,
    index: &mut HashMap<String, usize>,
) {
    let Some(content) = v.pointer("/message/content").and_then(Value::as_array) else {
        return;
    };
    let session_id = v
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or(fallback_session)
        .to_string();
    let timestamp = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc));
    let cwd = v.get("cwd").and_then(Value::as_str).map(str::to_string);
    let sidechain = v
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_use")
            || block.get("name").and_then(Value::as_str) != Some("Bash")
        {
            continue;
        }
        let (Some(id), Some(command)) = (
            block.get("id").and_then(Value::as_str),
            block.pointer("/input/command").and_then(Value::as_str),
        ) else {
            continue;
        };
        if index.contains_key(id) {
            continue;
        }
        index.insert(id.to_string(), calls.len());
        calls.push(BashCall {
            session_id: session_id.clone(),
            tool_use_id: id.to_string(),
            command: command.to_string(),
            timestamp,
            cwd: cwd.clone(),
            sidechain,
            seen_chars: 0,
            raw_chars: 0,
            is_error: false,
            has_result: false,
            persisted: false,
            output_sample: None,
            file: path.to_path_buf(),
            seq: calls.len(),
        });
    }
}

fn read_tool_results(
    v: &Value,
    calls: &mut [BashCall],
    index: &HashMap<String, usize>,
    stats: &mut ParseStats,
) {
    let Some(content) = v.pointer("/message/content").and_then(Value::as_array) else {
        return;
    };
    let tool_use_result = v.get("toolUseResult");
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(&i) = index.get(id) else {
            stats.orphan_results += 1;
            continue;
        };
        let call = &mut calls[i];
        let seen = content_text(block.get("content"));
        call.seen_chars = seen.chars().count() as u64;
        call.persisted = seen.contains("<persisted-output>");
        call.is_error = block
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        call.has_result = true;

        let raw = match tool_use_result {
            Some(Value::Object(o)) => {
                let stdout = o.get("stdout").and_then(Value::as_str).unwrap_or("");
                let stderr = o.get("stderr").and_then(Value::as_str).unwrap_or("");
                if stderr.is_empty() {
                    stdout.to_string()
                } else if stdout.is_empty() {
                    stderr.to_string()
                } else {
                    format!("{stdout}\n{stderr}")
                }
            }
            Some(Value::String(s)) => s.clone(),
            _ => seen.clone(),
        };
        call.raw_chars = raw.chars().count() as u64;
        call.output_sample = Some(cap(raw));
    }
}

/// `tool_result.content` is either a string or an array of content blocks.
fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn cap(mut s: String) -> String {
    if s.len() > SAMPLE_CAP {
        let mut end = SAMPLE_CAP;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        f
    }

    #[test]
    fn pairs_call_with_result() {
        let f = write(&[
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-09-01T10:00:00Z","cwd":"/p","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"git status"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"clean"}]},"toolUseResult":{"stdout":"clean","stderr":""}}"#,
        ]);
        let (calls, stats) = parse_file(f.path());
        assert_eq!(stats.bash_calls, 1);
        assert_eq!(calls[0].command, "git status");
        assert_eq!(calls[0].seen_chars, 5);
        assert!(calls[0].has_result);
        assert_eq!(calls[0].session_id, "s1");
    }

    #[test]
    fn skips_malformed_and_unknown() {
        let f = write(&[
            "{not json",
            r#"{"type":"new-future-type","whatever":1}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}"#,
        ]);
        let (calls, stats) = parse_file(f.path());
        assert!(calls.is_empty());
        assert_eq!(stats.malformed_lines, 1);
        assert_eq!(stats.lines, 3);
    }

    #[test]
    fn handles_block_content_and_string_tool_use_result() {
        let f = write(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"x"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":[{"type":"text","text":"Error: boom"}]}]},"toolUseResult":"Error: boom"}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"nope","content":"x"}]}}"#,
        ]);
        let (calls, stats) = parse_file(f.path());
        assert!(calls[0].is_error);
        assert_eq!(calls[0].seen_chars, 11);
        assert_eq!(stats.orphan_results, 1);
    }
}
