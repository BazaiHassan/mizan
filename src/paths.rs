//! Filesystem locations used by Claude Code, RTK, Lerim and mzn itself.
//!
//! Everything goes through `dirs` so Linux (XDG), macOS and Windows resolve
//! correctly without special cases.

use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// `~/.claude`, or `$CLAUDE_CONFIG_DIR` when set.
pub fn claude_dir() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home().join(".claude"),
    }
}

pub fn claude_projects_dir() -> PathBuf {
    claude_dir().join("projects")
}

/// `~/.claude.json` (MCP servers and per-project state).
pub fn claude_json() -> PathBuf {
    home().join(".claude.json")
}

pub fn user_settings() -> PathBuf {
    claude_dir().join("settings.json")
}

pub fn project_settings(project: &Path) -> PathBuf {
    project.join(".claude").join("settings.json")
}

pub fn project_local_settings(project: &Path) -> PathBuf {
    project.join(".claude").join("settings.local.json")
}

/// Claude Code's directory name for a project: every character that is not
/// an ASCII letter or digit becomes `-`.
pub fn encode_project(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub fn rtk_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join("rtk")
}

pub fn rtk_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| home().join(".local/share"))
        .join("rtk")
}

pub fn rtk_project_filters(project: &Path) -> PathBuf {
    project.join(".rtk").join("filters.toml")
}

pub fn rtk_global_filters() -> PathBuf {
    rtk_config_dir().join("filters.toml")
}

pub fn rtk_trust_store() -> PathBuf {
    rtk_data_dir().join("trusted_filters.json")
}

pub fn lerim_dir() -> PathBuf {
    home().join(".lerim")
}

pub fn mzn_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join("mzn")
}

pub fn mzn_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| home().join(".local/share"))
        .join("mzn")
}

pub fn mzn_state_file() -> PathBuf {
    mzn_config_dir().join("state.json")
}

/// Resolve the project root: explicit path, else the current directory.
pub fn resolve_project(arg: Option<&Path>) -> PathBuf {
    let p = match arg {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    std::fs::canonicalize(&p).unwrap_or(p)
}

/// Find an executable on `PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
        if cfg!(windows) {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_claude_code() {
        assert_eq!(
            encode_project(Path::new("/home/user/mizan")),
            "-home-user-mizan"
        );
        assert_eq!(
            encode_project(Path::new("/home/u/my_proj.v2")),
            "-home-u-my-proj-v2"
        );
        assert_eq!(encode_project(Path::new("/a/Análise")), "-a-An-lise");
    }
}
