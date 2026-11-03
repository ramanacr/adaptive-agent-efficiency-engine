//! Turn normalised sessions into aggregate statistics.
//!
//! ## Cost model (01_REQUIREMENTS §4)
//! Input is measured in **cost-weighted input units**: tokens weighted by their price relative to
//! uncached input. Defaults: uncached ×1, 5-minute cache write ×1.25, 1-hour cache write ×2,
//! cache read ×0.1. The same units are a proxy for subscription usage consumption, since plan
//! limits are not published in token terms. This is an assumption and is labelled as such in the report.
//!
//! A tool result of `T` estimated tokens that stays in context for `k` later requests (until
//! compaction or session end) costs roughly `T · (w_first + (k − 1) · w_read)`, where `w_first` is the
//! session's observed cache-write multiplier (Claude Code) or 1.0 (Codex, no billed writes).

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::command;
use crate::model::{Agent, Event, Session, Usage};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Weights {
    pub uncached: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            uncached: 1.0,
            cache_write_5m: 1.25,
            cache_write_1h: 2.0,
            cache_read: 0.1,
        }
    }
}

impl Weights {
    pub fn input_units(&self, u: &Usage) -> f64 {
        u.input as f64 * self.uncached
            + u.cache_write_5m as f64 * self.cache_write_5m
            + u.cache_write_1h as f64 * self.cache_write_1h
            + u.cache_read as f64 * self.cache_read
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    /// Bytes per token for estimating tool-result tokens (no public local Claude tokenizer; MET-001).
    pub bytes_per_token: f64,
    pub weights: Weights,
    /// Go/no-go threshold on tool-result share of cost-weighted input (M-1 gate).
    pub gate: f64,
    pub anonymize: bool,
    /// Only sessions whose first timestamp is >= this ISO-8601 prefix.
    pub since: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bytes_per_token: 4.0,
            weights: Weights::default(),
            gate: 0.20,
            anonymize: false,
            since: None,
        }
    }
}

const SHELL_TOOLS: &[&str] = &["Bash", "PowerShell", "shell", "local_shell", "exec_command"];

#[derive(Debug, Default, Clone)]
pub struct FamilyAcc {
    pub calls: u64,
    pub results: u64,
    pub errors: u64,
    pub est_tokens: Vec<f64>,
    pub cost_units: f64,
    pub occupancy_sum: u64,
    pub redundant_reruns: u64,
}

#[derive(Debug, Default, Clone)]
pub struct AgentAcc {
    pub sessions: u64,
    pub lines: u64,
    pub bad_lines: u64,
    pub requests: u64,
    pub usage: Usage,
    pub input_units: f64,
    pub tool_units: f64,
    pub results: u64,
    pub result_est_tokens: f64,
    pub persisted_previews: u64,
    pub images: u64,
    pub orphan_results: u64,
    pub compactions: u64,
    pub sessions_with_compaction: u64,
    pub shell_calls: u64,
    pub redundant_reruns: u64,
    pub post_compaction_reruns: u64,
    pub occupancy_sum: u64,
    /// Σ total prompt tokens of each session's first request (system prompt + tool schemas + memory).
    pub initial_context_sum: u64,
    pub tools: BTreeMap<String, u64>,
    pub families: HashMap<String, FamilyAcc>,
}

impl AgentAcc {
    fn merge(&mut self, o: AgentAcc) {
        self.sessions += o.sessions;
        self.lines += o.lines;
        self.bad_lines += o.bad_lines;
        self.requests += o.requests;
        self.usage.add(&o.usage);
        self.input_units += o.input_units;
        self.tool_units += o.tool_units;
        self.results += o.results;
        self.result_est_tokens += o.result_est_tokens;
        self.persisted_previews += o.persisted_previews;
        self.images += o.images;
        self.orphan_results += o.orphan_results;
        self.compactions += o.compactions;
        self.sessions_with_compaction += o.sessions_with_compaction;
        self.shell_calls += o.shell_calls;
        self.redundant_reruns += o.redundant_reruns;
        self.post_compaction_reruns += o.post_compaction_reruns;
        self.occupancy_sum += o.occupancy_sum;
        self.initial_context_sum += o.initial_context_sum;
        for (k, v) in o.tools {
            *self.tools.entry(k).or_default() += v;
        }
        for (k, v) in o.families {
            let f = self.families.entry(k).or_default();
            f.calls += v.calls;
            f.results += v.results;
            f.errors += v.errors;
            f.est_tokens.extend(v.est_tokens);
            f.cost_units += v.cost_units;
            f.occupancy_sum += v.occupancy_sum;
            f.redundant_reruns += v.redundant_reruns;
        }
    }
}

struct CallInfo {
    family: String,
}

/// Family for non-shell tools: MCP tools group by server.
fn tool_family(tool: &str) -> String {
    if let Some(rest) = tool.strip_prefix("mcp__") {
        let server = rest.split("__").next().unwrap_or(rest);
        return format!("mcp:{server}");
    }
    format!("tool:{tool}")
}

pub fn analyze_session(s: &Session, set: &Settings) -> AgentAcc {
    let w = &set.weights;
    let mut acc = AgentAcc {
        sessions: 1,
        lines: s.stats.lines,
        bad_lines: s.stats.bad_lines,
        ..AgentAcc::default()
    };

    // Requests before each event, and the request count at the end of each event's context epoch.
    let n = s.events.len();
    let mut reqs_before = vec![0u64; n];
    let mut r = 0u64;
    for (i, e) in s.events.iter().enumerate() {
        reqs_before[i] = r;
        match e {
            Event::Request(u) => {
                if r == 0 {
                    acc.initial_context_sum =
                        u.input + u.cache_write_5m + u.cache_write_1h + u.cache_read;
                }
                r += 1;
                acc.usage.add(u);
                acc.input_units += w.input_units(u);
            }
            Event::Compaction => acc.compactions += 1,
            _ => {}
        }
    }
    acc.requests = r;
    if acc.compactions > 0 {
        acc.sessions_with_compaction = 1;
    }
    let mut epoch_end = vec![r; n];
    let mut end = r;
    for i in (0..n).rev() {
        if matches!(s.events[i], Event::Compaction) {
            end = reqs_before[i];
        }
        epoch_end[i] = end;
    }

    let w_first = match s.agent {
        Agent::Codex => w.uncached,
        Agent::ClaudeCode => {
            let writes = acc.usage.cache_write_5m + acc.usage.cache_write_1h;
            if writes > 0 {
                (acc.usage.cache_write_5m as f64 * w.cache_write_5m
                    + acc.usage.cache_write_1h as f64 * w.cache_write_1h)
                    / writes as f64
            } else {
                w.cache_write_5m
            }
        }
    };

    let mut calls: HashMap<&str, CallInfo> = HashMap::new();
    // normalised command -> (mutation epoch, compaction epoch) of its last run
    let mut last_run: HashMap<String, (u64, u64)> = HashMap::new();
    let mut mutation_epoch = 0u64;
    let mut compaction_epoch = 0u64;

    for (i, e) in s.events.iter().enumerate() {
        match e {
            Event::Request(_) => {}
            Event::Compaction => compaction_epoch += 1,
            Event::ToolCall {
                id,
                tool,
                command,
                mutates,
            } => {
                *acc.tools.entry(tool.clone()).or_default() += 1;
                if *mutates {
                    mutation_epoch += 1;
                }
                let is_shell = SHELL_TOOLS.contains(&tool.as_str()) && command.is_some();
                let family = match (is_shell, command) {
                    (true, Some(c)) => command::family(c, set.anonymize),
                    _ => tool_family(tool),
                };
                let fam = acc.families.entry(family.clone()).or_default();
                fam.calls += 1;
                if let (true, Some(c)) = (is_shell, command) {
                    acc.shell_calls += 1;
                    let norm = command::normalize(c);
                    if let Some(&(m, ce)) = last_run.get(&norm) {
                        if m == mutation_epoch && ce == compaction_epoch {
                            acc.redundant_reruns += 1;
                            fam.redundant_reruns += 1;
                        } else if m == mutation_epoch {
                            acc.post_compaction_reruns += 1;
                        }
                    }
                    last_run.insert(norm, (mutation_epoch, compaction_epoch));
                }
                calls.insert(id.as_str(), CallInfo { family });
            }
            Event::ToolResult {
                id,
                bytes,
                images,
                is_error,
                persisted,
            } => {
                let family = match calls.get(id.as_str()) {
                    Some(c) => c.family.clone(),
                    None => {
                        acc.orphan_results += 1;
                        "(unmatched)".to_owned()
                    }
                };
                let est = *bytes as f64 / set.bytes_per_token;
                let k = epoch_end[i].saturating_sub(reqs_before[i]);
                let units = if k >= 1 {
                    est * (w_first + (k - 1) as f64 * w.cache_read)
                } else {
                    0.0
                };
                acc.results += 1;
                acc.result_est_tokens += est;
                acc.tool_units += units;
                acc.occupancy_sum += k;
                acc.images += u64::from(*images);
                acc.persisted_previews += u64::from(*persisted);
                let fam = acc.families.entry(family).or_default();
                fam.results += 1;
                fam.errors += u64::from(*is_error);
                fam.est_tokens.push(est);
                fam.cost_units += units;
                fam.occupancy_sum += k;
            }
        }
    }
    acc
}

/// Analyse sessions in parallel, grouped by agent. Sessions outside `--since` or without any
/// model request are skipped.
pub fn analyze_sessions(sessions: &[Session], set: &Settings) -> BTreeMap<Agent, AgentAcc> {
    use rayon::prelude::*;
    sessions
        .par_iter()
        .filter(|s| {
            let in_window = match (&set.since, &s.first_ts) {
                (Some(since), Some(ts)) => ts.as_str() >= since.as_str(),
                _ => true,
            };
            in_window && s.events.iter().any(|e| matches!(e, Event::Request(_)))
        })
        .map(|s| {
            let mut m = BTreeMap::new();
            m.insert(s.agent, analyze_session(s, set));
            m
        })
        .reduce(BTreeMap::new, |mut a, b| {
            for (k, v) in b {
                a.entry(k).or_default().merge(v);
            }
            a
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(input: u64, w5: u64, read: u64) -> Event {
        Event::Request(Usage {
            input,
            cache_write_5m: w5,
            cache_write_1h: 0,
            cache_read: read,
            output: 10,
        })
    }

    fn call(id: &str, cmd: &str) -> Event {
        Event::ToolCall {
            id: id.into(),
            tool: "Bash".into(),
            command: Some(cmd.into()),
            mutates: false,
        }
    }

    fn result(id: &str, bytes: u64) -> Event {
        Event::ToolResult {
            id: id.into(),
            bytes,
            images: 0,
            is_error: false,
            persisted: false,
        }
    }

    #[test]
    fn occupancy_and_cost_units() {
        // r0, call, result(400B=100tok), r1, r2, r3 -> k = 3 requests after the result.
        let s = Session {
            agent: Agent::ClaudeCode,
            events: vec![
                req(0, 1000, 0),
                call("a", "dotnet test"),
                result("a", 400),
                req(0, 200, 1000),
                req(0, 0, 1200),
                req(0, 0, 1200),
            ],
            stats: Default::default(),
            first_ts: None,
        };
        let acc = analyze_session(&s, &Settings::default());
        assert_eq!(acc.requests, 4);
        assert_eq!(acc.occupancy_sum, 3);
        // 100 tokens * (1.25 + 2 * 0.1) = 145
        assert!((acc.tool_units - 145.0).abs() < 1e-9, "{}", acc.tool_units);
        // input units: 1200*1.25 + 3400*0.1 = 1500 + 340
        assert!((acc.input_units - 1840.0).abs() < 1e-9);
        assert!(acc.families.contains_key("dotnet test"));
    }

    #[test]
    fn compaction_ends_occupancy_and_classifies_reruns() {
        let s = Session {
            agent: Agent::ClaudeCode,
            events: vec![
                req(10, 0, 0),
                call("a", "git status"),
                result("a", 40),
                req(10, 0, 0),
                call("b", "git  status"), // redundant: no edit, no compaction in between
                result("b", 40),
                Event::Compaction,
                req(10, 0, 0),
                req(10, 0, 0),
                call("c", "git status"), // post-compaction rerun
                result("c", 40),
            ],
            stats: Default::default(),
            first_ts: None,
        };
        let acc = analyze_session(&s, &Settings::default());
        assert_eq!(acc.redundant_reruns, 1);
        assert_eq!(acc.post_compaction_reruns, 1);
        // a: 1 request in its epoch, b: 0, c: 0
        assert_eq!(acc.occupancy_sum, 1);
        assert_eq!(acc.compactions, 1);
    }

    #[test]
    fn edits_reset_rerun_detection() {
        let s = Session {
            agent: Agent::ClaudeCode,
            events: vec![
                req(10, 0, 0),
                call("a", "dotnet test"),
                result("a", 40),
                Event::ToolCall {
                    id: "e".into(),
                    tool: "Edit".into(),
                    command: None,
                    mutates: true,
                },
                call("b", "dotnet test"),
                result("b", 40),
            ],
            stats: Default::default(),
            first_ts: None,
        };
        let acc = analyze_session(&s, &Settings::default());
        assert_eq!(acc.redundant_reruns, 0);
        assert!(acc.families.contains_key("tool:Edit"));
    }

    #[test]
    fn since_filter_and_empty_sessions_are_skipped() {
        let mut old = Session::new(Agent::Codex);
        old.first_ts = Some("2025-01-01T00:00:00Z".into());
        old.events.push(req(5, 0, 0));
        let empty = Session::new(Agent::Codex);
        let set = Settings {
            since: Some("2026-01-01".into()),
            ..Settings::default()
        };
        assert!(analyze_sessions(&[old, empty], &set).is_empty());
    }
}
