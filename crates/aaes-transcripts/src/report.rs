//! Serializable report + Markdown rendering. Contains aggregate numbers and family names only.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

use crate::analyze::{AgentAcc, Settings};
use crate::model::{Agent, Usage};

#[derive(Debug, Serialize)]
pub struct Report {
    pub tool: String,
    pub version: String,
    pub settings: Settings,
    pub agents: Vec<AgentReport>,
    pub overall: Option<Overall>,
}

#[derive(Debug, Serialize)]
pub struct Overall {
    pub input_units: f64,
    pub tool_result_units: f64,
    pub tool_result_share: f64,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Go,
    Rescope,
    InsufficientData,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Go => "GO",
            Verdict::Rescope => "RE-SCOPE",
            Verdict::InsufficientData => "INSUFFICIENT DATA",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AgentReport {
    pub agent: Agent,
    pub sessions: u64,
    pub sessions_with_compaction: u64,
    pub lines: u64,
    pub bad_lines: u64,
    pub requests: u64,
    pub usage: Usage,
    pub input_units: f64,
    pub tool_result_units: f64,
    pub tool_result_share: f64,
    pub verdict: Verdict,
    pub tool_results: u64,
    pub tool_result_est_tokens: f64,
    pub mean_occupancy_requests: f64,
    pub mean_initial_context_tokens: f64,
    pub persisted_previews: u64,
    pub images: u64,
    pub unmatched_results: u64,
    pub compactions: u64,
    pub shell_calls: u64,
    pub redundant_reruns: u64,
    pub redundant_rerun_rate: f64,
    pub post_compaction_reruns: u64,
    pub tools: BTreeMap<String, u64>,
    pub families: Vec<FamilyReport>,
}

#[derive(Debug, Serialize)]
pub struct FamilyReport {
    pub family: String,
    pub calls: u64,
    pub results: u64,
    pub errors: u64,
    pub est_tokens: f64,
    pub cost_units: f64,
    pub share_of_input: f64,
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub max: f64,
    pub mean_occupancy_requests: f64,
    pub redundant_reruns: u64,
}

/// Fewer sessions than this, and the verdict is "insufficient data".
pub const MIN_SESSIONS: u64 = 5;

fn ratio(a: f64, b: f64) -> f64 {
    if b > 0.0 { a / b } else { 0.0 }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((p * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1);
    sorted[idx]
}

fn verdict(sessions: u64, share: f64, gate: f64) -> Verdict {
    if sessions < MIN_SESSIONS {
        Verdict::InsufficientData
    } else if share >= gate {
        Verdict::Go
    } else {
        Verdict::Rescope
    }
}

impl Report {
    pub fn build(accs: BTreeMap<Agent, AgentAcc>, settings: Settings) -> Self {
        let mut agents = Vec::new();
        let (mut in_units, mut tool_units, mut sessions) = (0.0, 0.0, 0);
        for (agent, a) in accs {
            in_units += a.input_units;
            tool_units += a.tool_units;
            sessions += a.sessions;
            let mut families: Vec<FamilyReport> = a
                .families
                .into_iter()
                .map(|(family, mut f)| {
                    f.est_tokens.sort_by(f64::total_cmp);
                    FamilyReport {
                        family,
                        calls: f.calls,
                        results: f.results,
                        errors: f.errors,
                        est_tokens: f.est_tokens.iter().sum(),
                        cost_units: f.cost_units,
                        share_of_input: ratio(f.cost_units, a.input_units),
                        p50: percentile(&f.est_tokens, 0.50),
                        p90: percentile(&f.est_tokens, 0.90),
                        p95: percentile(&f.est_tokens, 0.95),
                        max: f.est_tokens.last().copied().unwrap_or(0.0),
                        mean_occupancy_requests: ratio(f.occupancy_sum as f64, f.results as f64),
                        redundant_reruns: f.redundant_reruns,
                    }
                })
                .collect();
            families.sort_by(|x, y| {
                y.cost_units
                    .total_cmp(&x.cost_units)
                    .then_with(|| x.family.cmp(&y.family))
            });
            let share = ratio(a.tool_units, a.input_units);
            agents.push(AgentReport {
                agent,
                sessions: a.sessions,
                sessions_with_compaction: a.sessions_with_compaction,
                lines: a.lines,
                bad_lines: a.bad_lines,
                requests: a.requests,
                usage: a.usage,
                input_units: a.input_units,
                tool_result_units: a.tool_units,
                tool_result_share: share,
                verdict: verdict(a.sessions, share, settings.gate),
                tool_results: a.results,
                tool_result_est_tokens: a.result_est_tokens,
                mean_occupancy_requests: ratio(a.occupancy_sum as f64, a.results as f64),
                mean_initial_context_tokens: ratio(a.initial_context_sum as f64, a.sessions as f64),
                persisted_previews: a.persisted_previews,
                images: a.images,
                unmatched_results: a.orphan_results,
                compactions: a.compactions,
                shell_calls: a.shell_calls,
                redundant_reruns: a.redundant_reruns,
                redundant_rerun_rate: ratio(a.redundant_reruns as f64, a.shell_calls as f64),
                post_compaction_reruns: a.post_compaction_reruns,
                tools: a.tools,
                families,
            });
        }
        let overall = (!agents.is_empty()).then(|| {
            let share = ratio(tool_units, in_units);
            Overall {
                input_units: in_units,
                tool_result_units: tool_units,
                tool_result_share: share,
                verdict: verdict(sessions, share, settings.gate),
            }
        });
        Report {
            tool: "aaes mine".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            settings,
            agents,
            overall,
        }
    }

    pub fn to_markdown(&self, top: usize) -> String {
        let mut o = String::new();
        let pct = |x: f64| format!("{:.1}%", x * 100.0);
        let _ = writeln!(o, "# AAES M-1 transcript report\n");
        let _ = writeln!(
            o,
            "`{} {}` · tool tokens estimated at {} bytes/token · gate {} · {}\n",
            self.tool,
            self.version,
            self.settings.bytes_per_token,
            pct(self.settings.gate),
            match &self.settings.since {
                Some(s) => format!("sessions since {s}"),
                None => "all sessions".to_owned(),
            }
        );

        let _ = writeln!(o, "## Verdict\n");
        let _ = writeln!(
            o,
            "| Agent | Sessions | Requests | Tool-result share of cost-weighted input | Verdict |"
        );
        let _ = writeln!(o, "|---|---:|---:|---:|---|");
        for a in &self.agents {
            let _ = writeln!(
                o,
                "| {} | {} | {} | **{}** | {} |",
                a.agent.label(),
                a.sessions,
                a.requests,
                pct(a.tool_result_share),
                a.verdict.label()
            );
        }
        if let Some(ov) = &self.overall {
            let _ = writeln!(
                o,
                "| **All** | | | **{}** | **{}** |",
                pct(ov.tool_result_share),
                ov.verdict.label()
            );
        }
        let _ = writeln!(o);

        for a in &self.agents {
            let u = &a.usage;
            let _ = writeln!(o, "## {}\n", a.agent.label());
            let _ = writeln!(o, "### Usage (from transcripts)\n");
            let _ = writeln!(o, "| Metric | Value |\n|---|---:|");
            let rows: [(&str, String); 13] = [
                ("Uncached input tokens", fmt_n(u.input as f64)),
                (
                    "Cache-write tokens (5m / 1h)",
                    format!(
                        "{} / {}",
                        fmt_n(u.cache_write_5m as f64),
                        fmt_n(u.cache_write_1h as f64)
                    ),
                ),
                ("Cache-read tokens", fmt_n(u.cache_read as f64)),
                ("Output tokens", fmt_n(u.output as f64)),
                ("Cost-weighted input units", fmt_n(a.input_units)),
                ("Tool results", fmt_n(a.tool_results as f64)),
                (
                    "Tool-result tokens (est., as delivered)",
                    fmt_n(a.tool_result_est_tokens),
                ),
                (
                    "Tool-result cost-weighted units (est.)",
                    fmt_n(a.tool_result_units),
                ),
                (
                    "Mean prompt at first request (system + tool schemas + memory)",
                    fmt_n(a.mean_initial_context_tokens),
                ),
                (
                    "Mean requests a result stays in context",
                    format!("{:.1}", a.mean_occupancy_requests),
                ),
                (
                    "Compactions (sessions affected)",
                    format!("{} ({})", a.compactions, a.sessions_with_compaction),
                ),
                (
                    "Shell calls · redundant re-runs",
                    format!(
                        "{} · {} ({})",
                        a.shell_calls,
                        a.redundant_reruns,
                        pct(a.redundant_rerun_rate)
                    ),
                ),
                (
                    "Re-runs right after compaction",
                    fmt_n(a.post_compaction_reruns as f64),
                ),
            ];
            for (k, v) in rows {
                let _ = writeln!(o, "| {k} | {v} |");
            }
            if a.persisted_previews > 0 {
                let _ = writeln!(
                    o,
                    "| Results already truncated to a preview by the host | {} |",
                    a.persisted_previews
                );
            }
            let _ = writeln!(o);

            let _ = writeln!(o, "### Top {top} families by cost-weighted tokens\n");
            let _ = writeln!(
                o,
                "| # | Family | Calls | Est. tokens | Share of input | p50 | p95 | Max | Occupancy | Redundant re-runs |"
            );
            let _ = writeln!(o, "|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|");
            for (i, f) in a.families.iter().take(top).enumerate() {
                let _ = writeln!(
                    o,
                    "| {} | `{}` | {} | {} | {} | {} | {} | {} | {:.1} | {} |",
                    i + 1,
                    f.family,
                    f.calls,
                    fmt_n(f.est_tokens),
                    pct(f.share_of_input),
                    fmt_n(f.p50),
                    fmt_n(f.p95),
                    fmt_n(f.max),
                    f.mean_occupancy_requests,
                    f.redundant_reruns
                );
            }
            let _ = writeln!(o);

            let _ = writeln!(o, "### Tool mix\n");
            let mut tools: Vec<_> = a.tools.iter().collect();
            tools.sort_by(|x, y| y.1.cmp(x.1).then_with(|| x.0.cmp(y.0)));
            let line = tools
                .iter()
                .take(15)
                .map(|(k, v)| format!("`{k}` {v}"))
                .collect::<Vec<_>>()
                .join(" · ");
            let _ = writeln!(o, "{line}\n");
            if a.bad_lines > 0 || a.unmatched_results > 0 {
                let _ = writeln!(
                    o,
                    "_Parse notes: {} unreadable lines of {}, {} results without a matching call._\n",
                    a.bad_lines, a.lines, a.unmatched_results
                );
            }
        }

        let _ = writeln!(o, "## Method and caveats\n");
        let _ = writeln!(
            o,
            "- **Cost-weighted input units** weight tokens by relative API price (uncached ×{}, 5m write ×{}, 1h write ×{}, read ×{}). For subscription plans, the same units are used as a *proxy* for usage-limit consumption; plans don't publish limits in token terms.",
            self.settings.weights.uncached,
            self.settings.weights.cache_write_5m,
            self.settings.weights.cache_write_1h,
            self.settings.weights.cache_read
        );
        let _ = writeln!(
            o,
            "- Tool-result tokens are **estimates** (bytes ÷ {}). There's no public local Claude tokenizer. The share scales linearly with this ratio.",
            self.settings.bytes_per_token
        );
        let _ = writeln!(
            o,
            "- A result's cost assumes it is written once and then read from cache by every later request until compaction or session end."
        );
        let _ = writeln!(
            o,
            "- *Redundant re-run* = the same command run again with no file edit and no compaction in between. It's the baseline for MET-010."
        );
        let _ = writeln!(
            o,
            "- Usage covers model requests only. Output tokens aren't part of the share."
        );
        o
    }
}

fn fmt_n(x: f64) -> String {
    let n = x.round() as i64;
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_formatting_and_percentiles() {
        assert_eq!(fmt_n(1234567.4), "1,234,567");
        assert_eq!(fmt_n(12.0), "12");
        let v = [1.0, 2.0, 3.0, 4.0, 100.0];
        assert_eq!(percentile(&v, 0.5), 3.0);
        assert_eq!(percentile(&v, 0.95), 100.0);
    }

    #[test]
    fn verdict_rules() {
        assert_eq!(verdict(2, 0.9, 0.2), Verdict::InsufficientData);
        assert_eq!(verdict(10, 0.25, 0.2), Verdict::Go);
        assert_eq!(verdict(10, 0.1, 0.2), Verdict::Rescope);
    }
}
