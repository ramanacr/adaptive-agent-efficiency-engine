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
//!
//! ## Bytes/token calibration (MET-001)
//! There is no public local Claude tokenizer, so tool-result tokens are estimated from bytes. The
//! ratio is calibrated from the transcripts themselves. For consecutive requests `n → n+1` with
//! exactly one large tool result in between (and no other new content), the prompt grows by about
//! `output_n + tokens(result) + overhead`. So `bytes(result) / (Δprompt − output_n − overhead)` is
//! one sample of bytes/token. The median over many samples is used unless overridden.

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
    /// Bytes per token for estimating tool-result tokens. `None` = calibrate from transcripts
    /// (falls back to [`DEFAULT_BYTES_PER_TOKEN`] when there are too few samples).
    pub bytes_per_token: Option<f64>,
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
            bytes_per_token: None,
            weights: Weights::default(),
            gate: 0.20,
            anonymize: false,
            since: None,
        }
    }
}

pub const DEFAULT_BYTES_PER_TOKEN: f64 = 4.0;
/// Minimum calibration samples before the calibrated ratio replaces the default.
pub const MIN_CALIBRATION_SAMPLES: usize = 200;
/// Only results at least this large are used for calibration (keeps overhead error small).
pub const CALIBRATION_MIN_BYTES: u64 = 4_000;
/// Approximate per-result framing tokens (tool_result wrapper, ids).
const CALIBRATION_OVERHEAD_TOKENS: f64 = 8.0;
/// A whole-file read above this many estimated tokens counts as "large".
pub const LARGE_READ_TOKENS: f64 = 2_000.0;

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

/// Token/cost totals for one "context lever" (e.g. unchanged re-reads).
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Lever {
    pub calls: u64,
    pub est_tokens: f64,
    pub cost_units: f64,
}

impl Lever {
    fn add(&mut self, tokens: f64, units: f64) {
        self.calls += 1;
        self.est_tokens += tokens;
        self.cost_units += units;
    }
    fn merge(&mut self, o: &Lever) {
        self.calls += o.calls;
        self.est_tokens += o.est_tokens;
        self.cost_units += o.cost_units;
    }
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
    /// Bytes/token calibration samples and the ratio actually used.
    pub calibration: Vec<f64>,
    pub bytes_per_token: f64,
    pub bytes_per_token_source: &'static str,
    /// Read-tool calls of the same file+range with no edit to that file (and no compaction) since.
    pub unchanged_rereads: Lever,
    /// Whole-file Read results above [`LARGE_READ_TOKENS`].
    pub large_whole_reads: Lever,
    /// …of which the same file was edited later in the same context.
    pub large_whole_reads_edited: u64,
    /// File-content reads through the shell (cat/sed/head/tail/Get-Content/type/less/more).
    pub shell_file_reads: Lever,
    /// Sub-agent (sidechain) contexts.
    pub side_sessions: u64,
    pub side_input_units: f64,
    pub side_tool_units: f64,
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
        self.calibration.extend(o.calibration);
        self.unchanged_rereads.merge(&o.unchanged_rereads);
        self.large_whole_reads.merge(&o.large_whole_reads);
        self.large_whole_reads_edited += o.large_whole_reads_edited;
        self.shell_file_reads.merge(&o.shell_file_reads);
        self.side_sessions += o.side_sessions;
        self.side_input_units += o.side_input_units;
        self.side_tool_units += o.side_tool_units;
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

struct CallInfo<'a> {
    family: String,
    reread: bool,
    /// Path of a whole-file Read (for large-read and later-edit checks).
    whole_read_path: Option<&'a str>,
    shell_file_read: bool,
    epoch: u64,
}

/// Family for non-shell tools: MCP tools group by server.
fn tool_family(tool: &str) -> String {
    if let Some(rest) = tool.strip_prefix("mcp__") {
        let server = rest.split("__").next().unwrap_or(rest);
        return format!("mcp:{server}");
    }
    format!("tool:{tool}")
}

const SHELL_READ_FAMILIES: &[&str] = &[
    "cat",
    "sed",
    "head",
    "tail",
    "less",
    "more",
    "type",
    "get-content",
    "gc",
    "nl",
    "bat",
];

fn prompt_tokens(u: &Usage) -> u64 {
    u.input + u.cache_write_5m + u.cache_write_1h + u.cache_read
}

/// Bytes/token samples from one session (see module docs).
pub fn calibration_samples(s: &Session) -> Vec<f64> {
    let mut out = Vec::new();
    let mut prev: Option<(u64, u64)> = None; // (prompt tokens, output tokens) of previous request
    let mut results = 0u32;
    let mut bytes = 0u64;
    let mut clean = true;
    for e in &s.events {
        match e {
            Event::Request(u) => {
                let prompt = prompt_tokens(u);
                if let Some((pp, po)) = prev {
                    if clean && results == 1 && bytes >= CALIBRATION_MIN_BYTES && prompt > pp {
                        let delta = (prompt - pp) as f64 - po as f64 - CALIBRATION_OVERHEAD_TOKENS;
                        if delta > 0.0 {
                            let r = bytes as f64 / delta;
                            if (1.0..=12.0).contains(&r) {
                                out.push(r);
                            }
                        }
                    }
                }
                prev = Some((prompt, u.output));
                results = 0;
                bytes = 0;
                clean = true;
            }
            Event::ToolResult {
                bytes: b, images, ..
            } => {
                results += 1;
                bytes += b;
                if *images > 0 {
                    clean = false;
                }
            }
            Event::Noise | Event::Compaction => clean = false,
            Event::ToolCall { .. } => {}
        }
    }
    out
}

pub fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

pub fn analyze_session(s: &Session, set: &Settings, bytes_per_token: f64) -> AgentAcc {
    let w = &set.weights;
    let mut acc = AgentAcc {
        sessions: 1,
        lines: s.stats.lines,
        bad_lines: s.stats.bad_lines,
        ..AgentAcc::default()
    };

    // Requests before each event, compaction epoch of each event, and the request count at the
    // end of each event's context epoch.
    let n = s.events.len();
    let mut reqs_before = vec![0u64; n];
    let mut epoch_of = vec![0u64; n];
    let mut r = 0u64;
    let mut ep = 0u64;
    // path -> event indices (and epochs) of edits to it, for "read then edited" checks.
    let mut edits: HashMap<&str, Vec<(usize, u64)>> = HashMap::new();
    for (i, e) in s.events.iter().enumerate() {
        reqs_before[i] = r;
        match e {
            Event::Request(u) => {
                if r == 0 {
                    acc.initial_context_sum = prompt_tokens(u);
                }
                r += 1;
                acc.usage.add(u);
                acc.input_units += w.input_units(u);
            }
            Event::Compaction => {
                acc.compactions += 1;
                ep += 1;
            }
            Event::ToolCall {
                mutates: true,
                path: Some(p),
                ..
            } => edits.entry(p.as_str()).or_default().push((i, ep)),
            _ => {}
        }
        epoch_of[i] = ep;
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
    // read key (path|range) -> (path version, compaction epoch) of its last read
    let mut last_read: HashMap<String, (u64, u64)> = HashMap::new();
    let mut path_version: HashMap<&str, u64> = HashMap::new();
    let mut mutation_epoch = 0u64;

    for (i, e) in s.events.iter().enumerate() {
        match e {
            Event::Request(_) | Event::Compaction | Event::Noise => {}
            Event::ToolCall {
                id,
                tool,
                command,
                mutates,
                path,
                range,
            } => {
                *acc.tools.entry(tool.clone()).or_default() += 1;
                if *mutates {
                    mutation_epoch += 1;
                    if let Some(p) = path {
                        *path_version.entry(p.as_str()).or_default() += 1;
                    }
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
                        if m == mutation_epoch && ce == epoch_of[i] {
                            acc.redundant_reruns += 1;
                            fam.redundant_reruns += 1;
                        } else if m == mutation_epoch {
                            acc.post_compaction_reruns += 1;
                        }
                    }
                    last_run.insert(norm, (mutation_epoch, epoch_of[i]));
                }

                let mut reread = false;
                let mut whole_read_path = None;
                if tool == "Read" {
                    if let Some(p) = path {
                        let key = format!("{p}|{}", range.as_deref().unwrap_or(""));
                        let version = path_version.get(p.as_str()).copied().unwrap_or(0);
                        reread = last_read.get(&key) == Some(&(version, epoch_of[i]));
                        last_read.insert(key, (version, epoch_of[i]));
                        if range.is_none() {
                            whole_read_path = Some(p.as_str());
                        }
                    }
                }
                let shell_file_read = is_shell
                    && SHELL_READ_FAMILIES.contains(&family.split(' ').next().unwrap_or_default());
                calls.insert(
                    id.as_str(),
                    CallInfo {
                        family,
                        reread,
                        whole_read_path,
                        shell_file_read,
                        epoch: epoch_of[i],
                    },
                );
            }
            Event::ToolResult {
                id,
                bytes,
                images,
                is_error,
                persisted,
            } => {
                let info = calls.get(id.as_str());
                let family = match info {
                    Some(c) => c.family.clone(),
                    None => {
                        acc.orphan_results += 1;
                        "(unmatched)".to_owned()
                    }
                };
                let est = *bytes as f64 / bytes_per_token;
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

                if let Some(c) = info {
                    if c.reread {
                        acc.unchanged_rereads.add(est, units);
                    }
                    if c.shell_file_read {
                        acc.shell_file_reads.add(est, units);
                    }
                    if let Some(p) = c.whole_read_path {
                        if est >= LARGE_READ_TOKENS {
                            acc.large_whole_reads.add(est, units);
                            let edited_later = edits
                                .get(p)
                                .is_some_and(|v| v.iter().any(|&(j, e2)| j > i && e2 == c.epoch));
                            acc.large_whole_reads_edited += u64::from(edited_later);
                        }
                    }
                }

                let fam = acc.families.entry(family).or_default();
                fam.results += 1;
                fam.errors += u64::from(*is_error);
                fam.est_tokens.push(est);
                fam.cost_units += units;
                fam.occupancy_sum += k;
            }
        }
    }
    if s.sidechain {
        acc.side_sessions = 1;
        acc.side_input_units = acc.input_units;
        acc.side_tool_units = acc.tool_units;
    }
    acc
}

/// Analyse sessions in parallel, grouped by agent. Sessions outside `--since` or without any
/// model request are skipped. Bytes/token is calibrated per agent unless set explicitly.
pub fn analyze_sessions(sessions: &[Session], set: &Settings) -> BTreeMap<Agent, AgentAcc> {
    use rayon::prelude::*;
    let selected: Vec<&Session> = sessions
        .par_iter()
        .filter(|s| {
            let in_window = match (&set.since, &s.first_ts) {
                (Some(since), Some(ts)) => ts.as_str() >= since.as_str(),
                _ => true,
            };
            in_window && s.events.iter().any(|e| matches!(e, Event::Request(_)))
        })
        .collect();

    // Pass 1: calibration samples per agent.
    let mut samples: BTreeMap<Agent, Vec<f64>> = selected
        .par_iter()
        .map(|s| (s.agent, calibration_samples(s)))
        .fold(BTreeMap::new, |mut m: BTreeMap<Agent, Vec<f64>>, (a, v)| {
            m.entry(a).or_default().extend(v);
            m
        })
        .reduce(BTreeMap::new, |mut a, b| {
            for (k, v) in b {
                a.entry(k).or_default().extend(v);
            }
            a
        });
    let mut ratio: BTreeMap<Agent, (f64, &'static str)> = BTreeMap::new();
    for (agent, v) in samples.iter_mut() {
        let chosen = match set.bytes_per_token {
            Some(b) => (b, "user-set"),
            None if v.len() >= MIN_CALIBRATION_SAMPLES => {
                (median(v).unwrap_or(DEFAULT_BYTES_PER_TOKEN), "calibrated")
            }
            None => (
                DEFAULT_BYTES_PER_TOKEN,
                "default (too few calibration samples)",
            ),
        };
        ratio.insert(*agent, chosen);
    }

    // Pass 2: analysis with the chosen ratio.
    let mut out = selected
        .par_iter()
        .map(|s| {
            let bpt = ratio
                .get(&s.agent)
                .map(|r| r.0)
                .unwrap_or(DEFAULT_BYTES_PER_TOKEN);
            let mut m = BTreeMap::new();
            m.insert(s.agent, analyze_session(s, set, bpt));
            m
        })
        .reduce(BTreeMap::new, |mut a, b| {
            for (k, v) in b {
                a.entry(k).or_default().merge(v);
            }
            a
        });
    for (agent, acc) in out.iter_mut() {
        let (bpt, src) = ratio
            .get(agent)
            .copied()
            .unwrap_or((DEFAULT_BYTES_PER_TOKEN, "default"));
        acc.bytes_per_token = bpt;
        acc.bytes_per_token_source = src;
        acc.calibration = samples.remove(agent).unwrap_or_default();
    }
    out
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
            path: None,
            range: None,
        }
    }

    fn file_call(id: &str, tool: &str, path: &str, range: Option<&str>) -> Event {
        Event::ToolCall {
            id: id.into(),
            tool: tool.into(),
            command: None,
            mutates: tool != "Read",
            path: Some(path.into()),
            range: range.map(Into::into),
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

    fn session(events: Vec<Event>) -> Session {
        Session {
            agent: Agent::ClaudeCode,
            events,
            stats: Default::default(),
            first_ts: None,
            sidechain: false,
        }
    }

    #[test]
    fn occupancy_and_cost_units() {
        // r0, call, result(400B=100tok), r1, r2, r3 -> k = 3 requests after the result.
        let s = session(vec![
            req(0, 1000, 0),
            call("a", "dotnet test"),
            result("a", 400),
            req(0, 200, 1000),
            req(0, 0, 1200),
            req(0, 0, 1200),
        ]);
        let acc = analyze_session(&s, &Settings::default(), 4.0);
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
        let s = session(vec![
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
        ]);
        let acc = analyze_session(&s, &Settings::default(), 4.0);
        assert_eq!(acc.redundant_reruns, 1);
        assert_eq!(acc.post_compaction_reruns, 1);
        // a: 1 request in its epoch, b: 0, c: 0
        assert_eq!(acc.occupancy_sum, 1);
        assert_eq!(acc.compactions, 1);
    }

    #[test]
    fn edits_reset_rerun_detection() {
        let s = session(vec![
            req(10, 0, 0),
            call("a", "dotnet test"),
            result("a", 40),
            file_call("e", "Edit", "src/a.cs", None),
            call("b", "dotnet test"),
            result("b", 40),
        ]);
        let acc = analyze_session(&s, &Settings::default(), 4.0);
        assert_eq!(acc.redundant_reruns, 0);
        assert!(acc.families.contains_key("tool:Edit"));
    }

    #[test]
    fn unchanged_rereads_and_large_whole_reads() {
        let s = session(vec![
            req(10, 0, 0),
            file_call("r1", "Read", "a.cs", None),
            result("r1", 12_000), // 3000 tok: large whole read, later edited
            req(10, 0, 0),
            file_call("r2", "Read", "a.cs", None), // unchanged re-read
            result("r2", 12_000),
            file_call("r3", "Read", "a.cs", Some("1:20")), // different range: not a re-read
            result("r3", 400),
            file_call("e1", "Edit", "a.cs", None),
            file_call("r4", "Read", "a.cs", None), // after edit: not a re-read
            result("r4", 400),
            req(10, 0, 0),
        ]);
        let acc = analyze_session(&s, &Settings::default(), 4.0);
        assert_eq!(acc.unchanged_rereads.calls, 1);
        assert!((acc.unchanged_rereads.est_tokens - 3000.0).abs() < 1e-9);
        assert_eq!(acc.large_whole_reads.calls, 2);
        assert_eq!(acc.large_whole_reads_edited, 2);
    }

    #[test]
    fn shell_reads_and_sidechain_split() {
        let mut s = session(vec![
            req(100, 0, 0),
            call("a", "sed -n '1,200p' src/app.ts"),
            result("a", 800),
            req(100, 0, 0),
        ]);
        s.sidechain = true;
        let acc = analyze_session(&s, &Settings::default(), 4.0);
        assert_eq!(acc.shell_file_reads.calls, 1);
        assert_eq!(acc.side_sessions, 1);
        assert!((acc.side_input_units - acc.input_units).abs() < 1e-9);
    }

    #[test]
    fn calibration_from_prompt_growth() {
        // prompt 10_000 -> 11_258; output 250; result 4_000 bytes:
        // delta = 1258 - 250 - 8 = 1000 tokens -> 4.0 bytes/token.
        let s = session(vec![
            Event::Request(Usage {
                cache_read: 10_000,
                output: 250,
                ..Usage::default()
            }),
            call("a", "cat big.txt"),
            result("a", 4_000),
            Event::Request(Usage {
                cache_read: 10_000,
                cache_write_5m: 1_258,
                output: 5,
                ..Usage::default()
            }),
            call("b", "cat big.txt"),
            result("b", 4_000),
            Event::Noise, // excluded: extra content in this turn
            Event::Request(Usage {
                cache_read: 11_258,
                cache_write_5m: 3_000,
                ..Usage::default()
            }),
        ]);
        assert_eq!(calibration_samples(&s), vec![4.0]);
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

    #[test]
    fn too_few_samples_fall_back_to_default() {
        let s = session(vec![req(5, 0, 0)]);
        let out = analyze_sessions(&[s], &Settings::default());
        let acc = &out[&Agent::ClaudeCode];
        assert_eq!(acc.bytes_per_token, DEFAULT_BYTES_PER_TOKEN);
        assert!(acc.bytes_per_token_source.starts_with("default"));
    }
}
