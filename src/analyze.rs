//! `mzn analyze`: per-command token cost, RTK coverage, waste and
//! over-compression.

use crate::cmdkey::{self, CmdInfo};
use crate::rtk::db::HookDecision;
use crate::rtk::{Classifier, Verdict};
use crate::session::{BashCall, ParseStats};
use crate::util::{ellipsize, est_tokens, human, table};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

/// How many later Bash calls count as "a few turns" for re-run detection.
pub const RERUN_WINDOW: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// Output went through an RTK filter.
    Rtk,
    /// Explicitly bypassed: `rtk proxy …` or `RTK_DISABLED=1`.
    Bypassed,
    /// RTK has a filter, but this call did not go through it.
    Missed,
    /// RTK has no filter for this command.
    Uncovered,
    /// No RTK available to ask.
    Unknown,
}

impl Coverage {
    pub fn label(self) -> &'static str {
        match self {
            Coverage::Rtk => "rtk",
            Coverage::Bypassed => "bypassed",
            Coverage::Missed => "missed",
            Coverage::Uncovered => "uncovered",
            Coverage::Unknown => "unknown",
        }
    }
}

/// Where coverage information came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageSource {
    /// RTK's `hook_decisions`, joined on `tool_use_id`.
    HookDecisions,
    /// Command text (`rtk …` prefix) plus `rtk rewrite` classification.
    CommandPrefix,
    /// Command text only; RTK not available.
    None,
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub key: String,
    pub runs: u64,
    pub sidechain_runs: u64,
    pub error_runs: u64,
    pub seen_chars: u64,
    pub raw_chars: u64,
    /// Estimate: characters / 4 of what the model received.
    pub est_tokens: u64,
    pub rtk_runs: u64,
    pub coverage: Coverage,
    pub example: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtk_equivalent: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rerun {
    pub key: String,
    pub session_id: String,
    pub rtk_command: String,
    pub raw_command: String,
    /// Bash calls between the two runs.
    pub gap: usize,
    pub raw_est_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Totals {
    pub bash_calls: u64,
    pub sidechain_calls: u64,
    pub est_tokens: u64,
    pub rtk_calls: u64,
    pub rtk_est_tokens: u64,
    pub waste_est_tokens: u64,
    pub sessions: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Analysis {
    pub project: String,
    pub days: u32,
    pub coverage_source: CoverageSource,
    pub totals: Totals,
    pub commands: Vec<Row>,
    pub waste: Vec<Row>,
    pub reruns: Vec<Rerun>,
    pub parse: ParseStats,
    pub token_note: &'static str,
}

pub struct Inputs<'a> {
    pub project: String,
    pub days: u32,
    pub calls: &'a [BashCall],
    pub parse: ParseStats,
    pub decisions: &'a HashMap<String, HookDecision>,
    pub classifier: &'a mut Classifier,
}

struct Classified<'a> {
    call: &'a BashCall,
    info: CmdInfo,
    coverage: Coverage,
    rtk_equivalent: Option<String>,
}

pub fn analyze(input: Inputs<'_>) -> Analysis {
    let have_decisions = !input.decisions.is_empty();
    let source = if have_decisions {
        CoverageSource::HookDecisions
    } else if input.classifier.available() {
        CoverageSource::CommandPrefix
    } else {
        CoverageSource::None
    };

    let mut classified = Vec::with_capacity(input.calls.len());
    for call in input.calls {
        let info = cmdkey::classify(&call.command);
        let decision = input.decisions.get(&call.tool_use_id);
        let (coverage, equivalent) = if info.rtk_proxy || info.rtk_disabled {
            (Coverage::Bypassed, None)
        } else if info.rtk_prefixed || decision.is_some_and(HookDecision::covered) {
            (
                Coverage::Rtk,
                decision.and_then(|d| d.rewritten_cmd.clone()),
            )
        } else {
            match input.classifier.classify(&call.command) {
                Verdict::Covered(rw) => (Coverage::Missed, Some(rw)),
                Verdict::Unsupported => (Coverage::Uncovered, None),
                Verdict::Denied | Verdict::Unknown => (Coverage::Unknown, None),
            }
        };
        classified.push(Classified {
            call,
            info,
            coverage,
            rtk_equivalent: equivalent,
        });
    }

    let commands = aggregate(&classified);
    let mut waste: Vec<Row> = commands
        .iter()
        .filter(|r| matches!(r.coverage, Coverage::Missed | Coverage::Uncovered))
        .cloned()
        .collect();
    waste.sort_by(|a, b| b.est_tokens.cmp(&a.est_tokens).then(a.key.cmp(&b.key)));

    let reruns = find_reruns(&classified, have_decisions);

    let mut sessions: Vec<&str> = input.calls.iter().map(|c| c.session_id.as_str()).collect();
    sessions.sort_unstable();
    sessions.dedup();
    let totals = Totals {
        bash_calls: classified.len() as u64,
        sidechain_calls: classified.iter().filter(|c| c.call.sidechain).count() as u64,
        est_tokens: classified
            .iter()
            .map(|c| est_tokens(c.call.seen_chars))
            .sum(),
        rtk_calls: classified
            .iter()
            .filter(|c| c.coverage == Coverage::Rtk)
            .count() as u64,
        rtk_est_tokens: classified
            .iter()
            .filter(|c| c.coverage == Coverage::Rtk)
            .map(|c| est_tokens(c.call.seen_chars))
            .sum(),
        waste_est_tokens: waste.iter().map(|r| r.est_tokens).sum(),
        sessions: sessions.len() as u64,
    };

    Analysis {
        project: input.project,
        days: input.days,
        coverage_source: source,
        totals,
        commands,
        waste,
        reruns,
        parse: input.parse,
        token_note: "tokens are estimates: characters / 4 of the tool output the model received",
    }
}

fn aggregate(classified: &[Classified<'_>]) -> Vec<Row> {
    struct Acc {
        row: Row,
        cov: BTreeMap<Coverage, u64>,
    }
    let mut map: HashMap<String, Acc> = HashMap::new();
    for c in classified {
        let key = if c.info.key.is_empty() {
            "(empty)".to_string()
        } else {
            c.info.key.clone()
        };
        let acc = map.entry(key.clone()).or_insert_with(|| Acc {
            row: Row {
                key,
                runs: 0,
                sidechain_runs: 0,
                error_runs: 0,
                seen_chars: 0,
                raw_chars: 0,
                est_tokens: 0,
                rtk_runs: 0,
                coverage: Coverage::Unknown,
                example: c.call.command.clone(),
                rtk_equivalent: None,
            },
            cov: BTreeMap::new(),
        });
        let r = &mut acc.row;
        r.runs += 1;
        r.sidechain_runs += c.call.sidechain as u64;
        r.error_runs += c.call.is_error as u64;
        r.seen_chars += c.call.seen_chars;
        r.raw_chars += c.call.raw_chars;
        r.est_tokens += est_tokens(c.call.seen_chars);
        r.rtk_runs += (c.coverage == Coverage::Rtk) as u64;
        if r.rtk_equivalent.is_none() {
            r.rtk_equivalent = c.rtk_equivalent.clone();
        }
        *acc.cov.entry(c.coverage).or_default() += 1;
    }
    let mut rows: Vec<Row> = map
        .into_values()
        .map(|mut acc| {
            // The most common coverage wins; ties go to the "worse" state so
            // waste is not hidden.
            acc.row.coverage = acc
                .cov
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)))
                .map(|(c, _)| *c)
                .unwrap_or(Coverage::Unknown);
            acc.row
        })
        .collect();
    rows.sort_by(|a, b| b.est_tokens.cmp(&a.est_tokens).then(a.key.cmp(&b.key)));
    rows
}

/// A command that went through RTK and then ran again without RTK within a
/// few calls, in the same transcript: a sign RTK's output was not enough.
fn find_reruns(classified: &[Classified<'_>], have_decisions: bool) -> Vec<Rerun> {
    let mut by_file: BTreeMap<&std::path::Path, Vec<&Classified<'_>>> = BTreeMap::new();
    for c in classified {
        by_file.entry(c.call.file.as_path()).or_default().push(c);
    }
    let mut out = Vec::new();
    for calls in by_file.values_mut() {
        calls.sort_by_key(|c| c.call.seq);
        for (i, first) in calls.iter().enumerate() {
            if first.coverage != Coverage::Rtk || first.info.key.is_empty() {
                continue;
            }
            for (gap, next) in calls.iter().skip(i + 1).take(RERUN_WINDOW).enumerate() {
                if next.info.key != first.info.key {
                    continue;
                }
                // Without hook decisions a raw-looking command may still have
                // been rewritten by the hook, so only explicit bypasses count.
                let raw = match next.coverage {
                    Coverage::Bypassed => true,
                    Coverage::Missed | Coverage::Unknown | Coverage::Uncovered => have_decisions,
                    Coverage::Rtk => false,
                };
                if raw {
                    out.push(Rerun {
                        key: first.info.key.clone(),
                        session_id: first.call.session_id.clone(),
                        rtk_command: first.call.command.clone(),
                        raw_command: next.call.command.clone(),
                        gap,
                        raw_est_tokens: est_tokens(next.call.seen_chars),
                    });
                }
                break;
            }
        }
    }
    out
}

pub fn render_text(a: &Analysis, limit: usize) -> String {
    let t = &a.totals;
    let mut s = String::new();
    s.push_str(&format!(
        "mzn analyze — {} (last {} days)\n\n",
        a.project, a.days
    ));
    if t.bash_calls == 0 {
        s.push_str("No Bash calls found in Claude Code sessions for this project.\n");
        s.push_str(&format!(
            "(looked in {} session files; run Claude Code in this directory first)\n",
            a.parse.files
        ));
        return s;
    }
    let pct = |n: u64, d: u64| {
        if d == 0 {
            0.0
        } else {
            n as f64 * 100.0 / d as f64
        }
    };
    s.push_str(&format!(
        "{} Bash calls in {} sessions ({} in subagents), ~{} tokens of output (estimate)\n",
        t.bash_calls,
        t.sessions,
        t.sidechain_calls,
        human(t.est_tokens)
    ));
    s.push_str(&format!(
        "Through RTK: {} calls ({:.0}%), ~{} tokens.  Not covered: ~{} tokens ({:.0}%).\n",
        t.rtk_calls,
        pct(t.rtk_calls, t.bash_calls),
        human(t.rtk_est_tokens),
        human(t.waste_est_tokens),
        pct(t.waste_est_tokens, t.est_tokens)
    ));
    s.push_str(&format!(
        "Coverage source: {}\n\n",
        source_label(a.coverage_source)
    ));

    s.push_str("Top commands by token cost\n");
    s.push_str(&rows_table(&a.commands, limit));

    s.push_str("\nWaste: costly commands RTK is not filtering\n");
    if a.waste.is_empty() {
        s.push_str("  none found\n");
    } else {
        s.push_str(&rows_table(&a.waste, limit));
        if a.waste.iter().any(|r| r.coverage == Coverage::Missed) {
            s.push_str("  `missed` = RTK has a filter but the call bypassed it (is the hook installed? see `mzn doctor`)\n");
        }
        if a.waste.iter().any(|r| r.coverage == Coverage::Uncovered) {
            s.push_str("  `uncovered` = no RTK filter exists; `mzn suggest` can generate one\n");
        }
    }

    s.push_str("\nPossible over-compression: re-run without RTK shortly after an RTK run\n");
    if a.reruns.is_empty() {
        s.push_str("  none found\n");
    } else {
        let mut counts: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
        for r in &a.reruns {
            let e = counts.entry(&r.key).or_default();
            e.0 += 1;
            e.1 += r.raw_est_tokens;
        }
        let mut rows: Vec<Vec<String>> = counts
            .into_iter()
            .map(|(k, (n, tok))| vec![ellipsize(k, 40), n.to_string(), human(tok)])
            .collect();
        rows.sort_by(|a, b| b[1].parse::<u64>().ok().cmp(&a[1].parse::<u64>().ok()));
        rows.truncate(limit);
        s.push_str(&table(&["command", "re-runs", "~tokens"], &rows, 1));
    }

    if a.parse.malformed_lines > 0 {
        s.push_str(&format!(
            "\nSkipped {} malformed log lines in {} files.\n",
            a.parse.malformed_lines, a.parse.files
        ));
    }
    s.push_str(&format!("\nNote: {}.\n", a.token_note));
    s
}

fn rows_table(rows: &[Row], limit: usize) -> String {
    let body: Vec<Vec<String>> = rows
        .iter()
        .take(limit)
        .map(|r| {
            vec![
                ellipsize(&r.key, 40),
                r.runs.to_string(),
                human(r.est_tokens),
                human(r.est_tokens / r.runs.max(1)),
                r.coverage.label().to_string(),
            ]
        })
        .collect();
    table(&["command", "runs", "~tokens", "~per run", "rtk"], &body, 1)
}

pub fn source_label(s: CoverageSource) -> &'static str {
    match s {
        CoverageSource::HookDecisions => "RTK hook decisions (exact)",
        CoverageSource::CommandPrefix => {
            "command text + `rtk rewrite` (approximate: hook rewrites may not show in logs)"
        }
        CoverageSource::None => "command text only (RTK not available)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn call(seq: usize, cmd: &str, chars: u64) -> BashCall {
        BashCall {
            session_id: "s".into(),
            tool_use_id: format!("t{seq}"),
            command: cmd.into(),
            timestamp: None,
            cwd: None,
            sidechain: false,
            seen_chars: chars,
            raw_chars: chars,
            is_error: false,
            has_result: true,
            persisted: false,
            output_sample: None,
            file: PathBuf::from("f.jsonl"),
            seq,
        }
    }

    fn run(calls: &[BashCall]) -> Analysis {
        let decisions = HashMap::new();
        let mut classifier = Classifier::disabled();
        analyze(Inputs {
            project: "p".into(),
            days: 30,
            calls,
            parse: ParseStats::default(),
            decisions: &decisions,
            classifier: &mut classifier,
        })
    }

    #[test]
    fn aggregates_and_detects_explicit_bypass_rerun() {
        let calls = vec![
            call(0, "rtk cargo test", 400),
            call(1, "ls", 40),
            call(2, "rtk proxy cargo test", 8000),
            call(3, "make build", 4000),
        ];
        let a = run(&calls);
        assert_eq!(a.totals.bash_calls, 4);
        assert_eq!(a.commands[0].key, "cargo test");
        assert_eq!(a.commands[0].runs, 2);
        assert_eq!(a.reruns.len(), 1);
        assert_eq!(a.reruns[0].gap, 1);
        assert_eq!(a.totals.rtk_calls, 1);
    }

    #[test]
    fn plain_rerun_not_flagged_without_decisions() {
        let calls = vec![call(0, "rtk git log", 100), call(1, "git log", 100)];
        assert!(run(&calls).reruns.is_empty());
    }

    #[test]
    fn rerun_outside_window_ignored() {
        let mut calls = vec![call(0, "rtk cargo test", 100)];
        for i in 1..=RERUN_WINDOW {
            calls.push(call(i, "ls", 10));
        }
        calls.push(call(RERUN_WINDOW + 1, "rtk proxy cargo test", 100));
        assert!(run(&calls).reruns.is_empty());
    }
}
