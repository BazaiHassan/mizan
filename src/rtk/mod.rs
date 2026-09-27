//! Everything mzn knows about an installed RTK: the binary, its stats
//! database and its filter files. mzn never bundles or installs RTK; it
//! reuses whatever is on `PATH`.

pub mod db;
pub mod filters;

use crate::util::run_with_timeout;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Oldest RTK with `rtk rewrite`.
pub const MIN_DEGRADED: Version = Version(0, 23, 0);
/// Oldest RTK that logs `hook_decisions` keyed by `tool_use_id`.
pub const MIN_FULL: Version = Version(0, 48, 0);
/// First untested major version.
pub const MAX_TESTED_EXCLUSIVE: Version = Version(1, 0, 0);

pub const INSTALL_HINT: &str = "curl -fsSL https://raw.githubusercontent.com/rtk-ai/rtk/refs/heads/master/install.sh | sh   (or: brew install rtk / cargo install --git https://github.com/rtk-ai/rtk)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches('v');
        let core = s.split(['-', '+', ' ']).next()?;
        let mut it = core.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next().unwrap_or("0").parse().ok()?;
        let patch = it.next().unwrap_or("0").parse().ok()?;
        Some(Version(major, minor, patch))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// In the supported range: hook decisions are joined by tool_use_id.
    Full,
    /// Older RTK: read-only analysis, coverage inferred from command prefixes.
    Degraded,
    /// Newer than tested: read-only analysis with a warning.
    Untested,
    /// Too old to have `rtk rewrite`.
    TooOld,
    /// A binary called `rtk` that is not Rust Token Killer.
    NotRtk,
    Missing,
}

impl Mode {
    pub fn can_classify(self) -> bool {
        matches!(self, Mode::Full | Mode::Degraded | Mode::Untested)
    }
    pub fn read_only(self) -> bool {
        !matches!(self, Mode::Full)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RtkInfo {
    pub path: Option<PathBuf>,
    pub version: Option<Version>,
    pub mode: Mode,
}

impl RtkInfo {
    pub fn describe(&self) -> String {
        let v = self
            .version
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".into());
        match self.mode {
            Mode::Full => format!("rtk {v} (full mode)"),
            Mode::Degraded => format!(
                "rtk {v} is older than {MIN_FULL}: read-only analysis, coverage inferred from command prefixes"
            ),
            Mode::Untested => format!(
                "rtk {v} is newer than mzn has been tested with (<{MAX_TESTED_EXCLUSIVE}): read-only analysis"
            ),
            Mode::TooOld => format!("rtk {v} is too old (need >= {MIN_DEGRADED})"),
            Mode::NotRtk => {
                "the `rtk` on PATH is not Rust Token Killer (name collision with Rust Type Kit?)"
                    .into()
            }
            Mode::Missing => "rtk not found on PATH".into(),
        }
    }
}

fn rtk_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MZN_RTK_BIN") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    crate::paths::which("rtk")
}

pub fn detect() -> RtkInfo {
    let Some(path) = rtk_binary() else {
        return RtkInfo {
            path: None,
            version: None,
            mode: Mode::Missing,
        };
    };
    let out = run_with_timeout(Command::new(&path).arg("--version"), Duration::from_secs(5));
    let version = out.ok().and_then(|o| {
        let line = o.stdout.lines().next().unwrap_or("").trim().to_string();
        line.strip_prefix("rtk ").and_then(Version::parse)
    });
    let Some(version) = version else {
        return RtkInfo {
            path: Some(path),
            version: None,
            mode: Mode::NotRtk,
        };
    };
    // Rust Type Kit also answers `--version`; only Rust Token Killer has `gain`.
    let is_token_killer = run_with_timeout(
        Command::new(&path).args(["gain", "--help"]),
        Duration::from_secs(5),
    )
    .map(|o| o.status == Some(0))
    .unwrap_or(false);
    let mode = if !is_token_killer {
        Mode::NotRtk
    } else if version < MIN_DEGRADED {
        Mode::TooOld
    } else if version < MIN_FULL {
        Mode::Degraded
    } else if version >= MAX_TESTED_EXCLUSIVE {
        Mode::Untested
    } else {
        Mode::Full
    };
    RtkInfo {
        path: Some(path),
        version: Some(version),
        mode,
    }
}

/// What RTK would do with a command, according to `rtk rewrite`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// RTK has a filter; rewrites to the given command.
    Covered(String),
    /// RTK has no equivalent.
    Unsupported,
    /// A deny rule matched; RTK stays out of the way.
    Denied,
    Unknown,
}

/// Asks the installed RTK to classify commands, caching per command. `rtk
/// rewrite` only reads config; it does not write to RTK's stats database.
pub struct Classifier {
    bin: Option<PathBuf>,
    cache: HashMap<String, Verdict>,
    budget: usize,
}

impl Classifier {
    pub fn new(info: &RtkInfo) -> Classifier {
        Classifier {
            bin: info
                .mode
                .can_classify()
                .then(|| info.path.clone())
                .flatten(),
            cache: HashMap::new(),
            budget: 400,
        }
    }

    pub fn disabled() -> Classifier {
        Classifier {
            bin: None,
            cache: HashMap::new(),
            budget: 0,
        }
    }

    pub fn available(&self) -> bool {
        self.bin.is_some()
    }

    pub fn classify(&mut self, command: &str) -> Verdict {
        if let Some(v) = self.cache.get(command) {
            return v.clone();
        }
        let Some(bin) = &self.bin else {
            return Verdict::Unknown;
        };
        if self.budget == 0 {
            return Verdict::Unknown;
        }
        self.budget -= 1;
        let verdict = match run_with_timeout(
            Command::new(bin)
                .arg("rewrite")
                .arg(command)
                .env_remove("RTK_REWRITE_HOST"),
            Duration::from_secs(5),
        ) {
            Ok(o) => match o.status {
                Some(0) | Some(3) => {
                    let rewritten = o.stdout.trim().to_string();
                    if rewritten.is_empty() {
                        Verdict::Unknown
                    } else {
                        Verdict::Covered(rewritten)
                    }
                }
                Some(1) => Verdict::Unsupported,
                Some(2) => Verdict::Denied,
                _ => Verdict::Unknown,
            },
            Err(_) => Verdict::Unknown,
        };
        self.cache.insert(command.to_string(), verdict.clone());
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(Version::parse("0.49.0"), Some(Version(0, 49, 0)));
        assert_eq!(Version::parse("v1.2"), Some(Version(1, 2, 0)));
        assert_eq!(Version::parse("0.48.1-beta.1"), Some(Version(0, 48, 1)));
        assert_eq!(Version::parse("x"), None);
        assert!(Version(0, 47, 9) < MIN_FULL);
    }
}
