//! Read-only access to RTK's tracking database (`history.db`).
//!
//! Opened with `mode=ro`; mzn never writes to it. Every query tolerates a
//! missing database or table (older RTK versions), returning empty results.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// `$RTK_DB_PATH` → `tracking.database_path` in RTK's config.toml → default.
pub fn db_path() -> PathBuf {
    if let Some(p) = std::env::var_os("RTK_DB_PATH") {
        return PathBuf::from(p);
    }
    let config = crate::paths::rtk_config_dir().join("config.toml");
    if let Ok(text) = std::fs::read_to_string(&config)
        && let Ok(value) = text.parse::<toml::Table>()
        && let Some(p) = value
            .get("tracking")
            .and_then(|t| t.get("database_path"))
            .and_then(|p| p.as_str())
    {
        return PathBuf::from(p);
    }
    crate::paths::rtk_data_dir().join("history.db")
}

pub struct RtkDb {
    conn: Connection,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookDecision {
    pub decision: String,
    pub rewritten_cmd: Option<String>,
}

impl HookDecision {
    /// `allow` and `ask` mean RTK rewrote the command.
    pub fn covered(&self) -> bool {
        matches!(self.decision.as_str(), "allow" | "ask")
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Savings {
    pub commands: u64,
    pub input_tokens: u64,
    pub saved_tokens: u64,
    /// `(rtk command, saved tokens)`, largest first.
    pub top: Vec<(String, u64)>,
}

impl RtkDb {
    pub fn open() -> Option<RtkDb> {
        let path = db_path();
        if !path.is_file() {
            return None;
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        let _ = conn.busy_timeout(std::time::Duration::from_secs(2));
        Some(RtkDb { conn })
    }

    pub fn has_table(&self, name: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
                |_| Ok(()),
            )
            .is_ok()
    }

    /// Hook decisions for the given sessions, keyed by `tool_use_id`.
    pub fn hook_decisions(&self, sessions: &HashSet<String>) -> HashMap<String, HookDecision> {
        let mut out = HashMap::new();
        if !self.has_table("hook_decisions") || sessions.is_empty() {
            return out;
        }
        let ids: Vec<&String> = sessions.iter().collect();
        for chunk in ids.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT tool_use_id, decision, rewritten_cmd FROM hook_decisions \
                 WHERE session_id IN ({placeholders}) ORDER BY id"
            );
            let Ok(mut stmt) = self.conn.prepare(&sql) else {
                return out;
            };
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    HookDecision {
                        decision: r.get(1)?,
                        rewritten_cmd: r.get(2)?,
                    },
                ))
            });
            if let Ok(rows) = rows {
                for (id, d) in rows.flatten() {
                    out.insert(id, d);
                }
            }
        }
        out
    }

    /// Savings recorded by RTK since `since` (all projects: RTK's `commands`
    /// table has no project column).
    pub fn savings_since(&self, since: DateTime<Utc>) -> Savings {
        let mut s = Savings::default();
        if !self.has_table("commands") {
            return s;
        }
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT timestamp, rtk_cmd, input_tokens, saved_tokens FROM commands")
        else {
            return s;
        };
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        });
        let mut by_cmd: HashMap<String, u64> = HashMap::new();
        if let Ok(rows) = rows {
            for (ts, cmd, input, saved) in rows.flatten() {
                let Ok(ts) = DateTime::parse_from_rfc3339(&ts) else {
                    continue;
                };
                if ts.with_timezone(&Utc) < since {
                    continue;
                }
                s.commands += 1;
                s.input_tokens += input.max(0) as u64;
                s.saved_tokens += saved.max(0) as u64;
                let key = crate::cmdkey::classify(&cmd).key;
                *by_cmd.entry(format!("rtk {key}")).or_default() += saved.max(0) as u64;
            }
        }
        let mut top: Vec<(String, u64)> = by_cmd.into_iter().filter(|(_, v)| *v > 0).collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        top.truncate(5);
        s.top = top;
        s
    }
}
