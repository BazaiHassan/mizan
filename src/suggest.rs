//! `mzn suggest`: generate RTK TOML filters for costly project commands that
//! RTK does not cover.
//!
//! Generated filters only remove lines: noise patterns are chosen from a
//! fixed catalogue, and a pattern is used only if it matched real output
//! lines of that command without ever matching a line that mentions an error,
//! failure or warning.

use crate::analyze::{Analysis, Coverage};
use crate::rtk::filters::{FilterDef, NAME_PREFIX, simulate};
use crate::session::BashCall;
use crate::util::est_tokens;
use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

/// Lines matching this are never removed by a generated filter.
static IMPORTANT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(error|fail|panic|exception|warn|fatal|denied|traceback|assert|abort|refused|not found|cannot|can't|unable|invalid|expected|missing|undefined|segfault|timeout|timed out)")
        .expect("static regex")
});

static ANSI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").expect("static regex"));

/// `(label, pattern)`. Patterns are RTK `strip_lines_matching` regexes.
const NOISE: &[(&str, &str)] = &[
    ("blank lines", r"^\s*$"),
    ("progress percentages", r"^\s*\[?\s*\d{1,3}(\.\d+)?\s*%"),
    ("progress bars", r"[=#>━█▇▆▅▄▃▂▁░▒▓]{8,}"),
    (
        "download/install chatter",
        r"^\s*(Downloading|Downloaded|Fetching|Fetched|Resolving|Resolved|Unpacking|Installing|Installed|Collecting|Using cached|Requirement already satisfied|Preparing|Reading package lists|Building dependency tree|Get:\d+|Hit:\d+|Ign:\d+)\b",
    ),
    (
        "compile progress",
        r"^\s*(Compiling|Checking|Building|Bundling|Transforming|Linking|Generating|Processing|Documenting)\b",
    ),
    (
        "passing tests",
        r"^\s*(✓|✔|√|PASS\b|PASSED\b|ok\s+\d|test .* \.\.\. ok$|\S+ PASSED)",
    ),
    ("passing tests (python)", r"^\S+::\S+ PASSED"),
    (
        "docker build steps",
        r"^#\d+ (\[internal\]|DONE|CACHED|sha256:|transferring|resolve|naming to|exporting|writing image)",
    ),
    (
        "docker layers",
        r"^[0-9a-f]{12}: (Pulling|Waiting|Verifying|Download complete|Pull complete|Already exists|Extracting)",
    ),
    (
        "dependency stack frames",
        r"^\s+at .*(node_modules|internal/)",
    ),
    (
        "npm funding/audit notes",
        r"^\s*(\d+ packages? (are|is) looking for funding|run `npm fund`|found 0 vulnerabilities)",
    ),
    (
        "timestamped debug/info logs",
        r"^\s*\[?\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}[^\]]*\]?\s*(DEBUG|TRACE|INFO)\b",
    ),
    ("separator lines", r"^\s*[-=_*~#]{10,}\s*$"),
];

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    pub filter: FilterDef,
    pub runs: u64,
    pub est_tokens: u64,
    /// Estimated tokens after filtering, over the same samples.
    pub est_tokens_after: u64,
    pub est_saved_pct: f64,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub key: String,
    pub reason: String,
}

pub struct Options {
    pub min_runs: u64,
    pub min_tokens: u64,
    pub min_saving_pct: f64,
    pub max: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            min_runs: 2,
            min_tokens: 500,
            min_saving_pct: 20.0,
            max: 10,
        }
    }
}

pub fn filter_name(key: &str) -> String {
    let mut slug = String::new();
    for c in key.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    format!(
        "{NAME_PREFIX}{}",
        if slug.is_empty() { "cmd" } else { slug }
    )
}

/// A fully anchored regex matching the key followed by end or whitespace.
pub fn match_regex(key: &str) -> String {
    let words: Vec<String> = key.split_whitespace().map(regex::escape).collect();
    format!(r"^{}(\s|$)", words.join(r"\s+"))
}

/// Candidate commands and the reason each non-candidate was skipped.
pub fn suggest(
    analysis: &Analysis,
    calls: &[BashCall],
    user_filter_for: &dyn Fn(&str) -> Option<String>,
    opts: &Options,
) -> (Vec<Suggestion>, Vec<Skipped>) {
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for row in &analysis.waste {
        if out.len() >= opts.max {
            break;
        }
        if row.coverage == Coverage::Missed {
            skipped.push(Skipped {
                key: row.key.clone(),
                reason: "RTK already has a filter; the call bypassed the hook (see `mzn doctor`)"
                    .into(),
            });
            continue;
        }
        if row.runs < opts.min_runs || row.est_tokens < opts.min_tokens {
            continue;
        }
        if let Some(name) = user_filter_for(&row.key) {
            skipped.push(Skipped {
                key: row.key.clone(),
                reason: format!("your filter `{name}` already matches it (yours take precedence)"),
            });
            continue;
        }
        let samples: Vec<&str> = calls
            .iter()
            .filter(|c| crate::cmdkey::classify(&c.command).key == row.key)
            .filter_map(|c| c.output_sample.as_deref())
            .collect();
        match build_filter(&row.key, &samples, opts) {
            Some(mut s) => {
                s.runs = row.runs;
                s.est_tokens = row.est_tokens;
                out.push(s);
            }
            None => skipped.push(Skipped {
                key: row.key.clone(),
                reason: format!(
                    "no safe noise pattern saves at least {:.0}% on recorded output",
                    opts.min_saving_pct
                ),
            }),
        }
    }
    (out, skipped)
}

fn build_filter(key: &str, samples: &[&str], opts: &Options) -> Option<Suggestion> {
    if samples.is_empty() {
        return None;
    }
    let strip_ansi = samples.iter().any(|s| ANSI.is_match(s));
    let clean: Vec<String> = samples
        .iter()
        .map(|s| ANSI.replace_all(s, "").into_owned())
        .collect();
    let lines: Vec<&str> = clean.iter().flat_map(|s| s.lines()).collect();
    let total = lines.len().max(1);

    let mut patterns = Vec::new();
    let mut reasons = Vec::new();
    for (label, pat) in NOISE {
        let re = Regex::new(pat).expect("catalogue regex");
        let hits: Vec<&&str> = lines.iter().filter(|l| re.is_match(l)).collect();
        if hits.is_empty() || hits.iter().any(|l| IMPORTANT.is_match(l)) {
            continue;
        }
        // Blank lines are cheap to strip; other patterns must be meaningful.
        let share = hits.len() as f64 / total as f64;
        if *label != "blank lines" && share < 0.05 {
            continue;
        }
        patterns.push(pat.to_string());
        reasons.push(format!("{label}: {:.0}% of lines", share * 100.0));
    }

    let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let truncate_lines_at = (longest > 500).then_some(300);
    if truncate_lines_at.is_some() {
        reasons.push("very long lines truncated to 300 chars".into());
    }

    let mut filter = FilterDef {
        name: filter_name(key),
        description: format!(
            "mzn: noise filter for `{key}` generated from this project's sessions"
        ),
        match_command: match_regex(key),
        strip_ansi,
        strip_lines_matching: patterns,
        truncate_lines_at,
        tail_lines: None,
        on_empty: Some(format!("{key}: ok (no output after filtering)")),
    };

    // Long outputs: keep the end, where summaries and failures usually are.
    let after_strip: Vec<usize> = samples
        .iter()
        .map(|s| simulate(&filter, s).lines().count())
        .collect();
    let avg = after_strip.iter().sum::<usize>() / after_strip.len().max(1);
    if avg > 150 {
        filter.tail_lines = Some(120);
        reasons.push(format!(
            "long output (avg {avg} lines after filtering): keep last 120 lines"
        ));
    }

    let before: u64 = samples
        .iter()
        .map(|s| est_tokens(s.chars().count() as u64))
        .sum();
    let after: u64 = samples
        .iter()
        .map(|s| est_tokens(simulate(&filter, s).chars().count() as u64))
        .sum();
    let pct = if before == 0 {
        0.0
    } else {
        (before.saturating_sub(after)) as f64 * 100.0 / before as f64
    };
    if pct < opts.min_saving_pct
        || filter.strip_lines_matching.is_empty() && filter.tail_lines.is_none()
    {
        return None;
    }
    Some(Suggestion {
        filter,
        runs: 0,
        est_tokens: 0,
        est_tokens_after: after,
        est_saved_pct: pct,
        reasons,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_regexes() {
        assert_eq!(filter_name("npm run build"), "mzn-npm-run-build");
        assert_eq!(filter_name("./scripts/ci.sh"), "mzn-scripts-ci-sh");
        let re = Regex::new(&match_regex("./scripts/ci.sh")).unwrap();
        assert!(re.is_match("./scripts/ci.sh --fast"));
        assert!(re.is_match("./scripts/ci.sh"));
        assert!(!re.is_match("./scripts/ci.shx"));
        let re = Regex::new(&match_regex("npm run build")).unwrap();
        assert!(re.is_match("npm  run build --prod"));
        assert!(!re.is_match("npm run builder"));
    }

    #[test]
    fn keeps_error_lines() {
        let out = "Compiling a\nCompiling b\nCompiling c failed with error\n";
        let s = build_filter("make", &[out, out], &Options::default());
        // "compile progress" also hits a line mentioning an error: unsafe.
        assert!(s.is_none());
        let ok = "Compiling a\nCompiling b\nCompiling c\nerror: build failed\n";
        let s = build_filter("make", &[ok, ok], &Options::default()).unwrap();
        assert_eq!(simulate(&s.filter, ok), "error: build failed");
    }

    #[test]
    fn builds_filter_for_noisy_output() {
        let mut out = String::new();
        for i in 0..50 {
            out.push_str(&format!("Downloading package-{i}\n"));
        }
        out.push_str("\nBuild finished: 3 warnings\n");
        let s = build_filter("./build.sh", &[&out], &Options::default()).unwrap();
        assert!(s.est_saved_pct > 80.0);
        let filtered = simulate(&s.filter, &out);
        assert!(filtered.contains("Build finished"));
    }
}
