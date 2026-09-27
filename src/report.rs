//! `mzn report`: a short, shareable summary for the last N days.

use crate::analyze::{Analysis, Coverage};
use crate::rtk::db::Savings;
use crate::suggest::Suggestion;
use crate::util::human;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub days: u32,
    pub project: String,
    /// From RTK's own stats, all projects (RTK does not record the project).
    pub rtk_savings: Option<Savings>,
    pub bash_calls: u64,
    pub sessions: u64,
    pub est_tokens: u64,
    pub rtk_share_pct: f64,
    pub waste_est_tokens: u64,
    /// False when RTK was unavailable, so coverage could not be determined.
    pub coverage_known: bool,
    pub top_waste: Vec<(String, u64, &'static str)>,
    pub reruns: usize,
    pub suggestions: usize,
    pub suggestion_est_saved: u64,
}

pub fn build(a: &Analysis, savings: Option<Savings>, suggestions: &[Suggestion]) -> Report {
    let t = &a.totals;
    Report {
        days: a.days,
        project: a.project.clone(),
        rtk_savings: savings,
        bash_calls: t.bash_calls,
        sessions: t.sessions,
        est_tokens: t.est_tokens,
        rtk_share_pct: if t.bash_calls == 0 {
            0.0
        } else {
            t.rtk_calls as f64 * 100.0 / t.bash_calls as f64
        },
        waste_est_tokens: t.waste_est_tokens,
        coverage_known: a.coverage_source != crate::analyze::CoverageSource::None,
        top_waste: a
            .waste
            .iter()
            .take(3)
            .map(|r| {
                (
                    r.key.clone(),
                    r.est_tokens,
                    if r.coverage == Coverage::Missed {
                        "hook bypassed"
                    } else {
                        "no RTK filter"
                    },
                )
            })
            .collect(),
        reruns: a.reruns.len(),
        suggestions: suggestions.len(),
        suggestion_est_saved: suggestions
            .iter()
            .map(|s| s.est_tokens.saturating_sub(s.est_tokens_after))
            .sum(),
    }
}

pub fn render(r: &Report, markdown: bool) -> String {
    let (h, b, li) = if markdown {
        ("## ", "**", "- ")
    } else {
        ("", "", "  - ")
    };
    let mut s = String::new();
    s.push_str(&format!(
        "{h}mzn report: last {} days — {}\n\n",
        r.days, r.project
    ));
    match &r.rtk_savings {
        Some(sv) if sv.commands > 0 => {
            s.push_str(&format!(
                "RTK saved you {b}~{} tokens{b} this period across {} filtered commands (all projects, from `rtk gain` data).\n",
                human(sv.saved_tokens),
                sv.commands
            ));
            if !sv.top.is_empty() {
                s.push_str("Top wins:\n");
                for (cmd, saved) in &sv.top {
                    s.push_str(&format!("{li}`{cmd}`: ~{} tokens\n", human(*saved)));
                }
            }
        }
        _ => s.push_str("No RTK savings recorded for this period (is RTK installed and hooked? `mzn doctor`).\n"),
    }
    s.push('\n');
    if r.bash_calls == 0 {
        s.push_str("No Claude Code Bash activity for this project in the period.\n");
        return s;
    }
    s.push_str(&format!(
        "This project: {} Bash calls in {} sessions, ~{} tokens of command output; {:.0}% of calls went through RTK.\n",
        r.bash_calls,
        r.sessions,
        human(r.est_tokens),
        r.rtk_share_pct
    ));
    if !r.coverage_known {
        s.push_str("RTK is not available, so coverage could not be measured. Run `mzn doctor`.\n");
    } else if r.top_waste.is_empty() {
        s.push_str("No significant uncovered commands. Nice.\n");
    } else {
        s.push_str(&format!(
            "Still unfiltered: ~{} tokens. Biggest:\n",
            human(r.waste_est_tokens)
        ));
        for (k, t, why) in &r.top_waste {
            s.push_str(&format!("{li}`{k}`: ~{} tokens ({why})\n", human(*t)));
        }
    }
    if r.suggestions > 0 {
        s.push_str(&format!(
            "{b}{} filter suggestion(s){b} ready, estimated to save ~{} tokens on the same workload: run `mzn suggest`.\n",
            r.suggestions,
            human(r.suggestion_est_saved)
        ));
    }
    if r.reruns > 0 {
        s.push_str(&format!(
            "{} time(s) a command was re-run without RTK right after an RTK run: RTK's output may be too terse there (`mzn analyze`).\n",
            r.reruns
        ));
    }
    let note = "Token counts are estimates (characters / 4).";
    if markdown {
        s.push_str(&format!("\n_{note}_\n"));
    } else {
        s.push_str(&format!("\n{note}\n"));
    }
    s
}
