//! The fixed request prefix and what it costs (ADR-025, `aaes mine --tools`).
//!
//! Every model request starts with the same prefix: the system prompt, the tool definitions, and
//! the listings that Claude Code adds as attachments (deferred tool names, MCP server
//! instructions, the skill listing, the agent listing). Claude Code 2.1.304 and later write the
//! exact `system` and `tools` arrays of each request to the transcript (`api-request-shape`, keyed
//! by `shapeHash`, referenced by each `api-request`), so the prefix is measured from its text, not
//! estimated from usage. Tools with `defer_loading` are not sent to the model; only their names
//! appear, in the deferred-tool listing.
//!
//! Cost model: the prefix is read from the cache (weight `cache_read`) on most requests and written
//! again (weight `cache_write_*`) when a request rewrites the cache (its cache write is at least 90%
//! of the prefix: cache expiry, compaction, model change). The prefix of a request is capped at the
//! request's input. Its share of a session's cost-weighted input is split over the groups (built-in
//! tools, each MCP server, each plugin server, the system prompt, each listing) by bytes.
//!
//! Bytes to tokens: calibrated from the first request of fresh sessions (no cache read), where the
//! input is the prefix plus the first message, so `prefix bytes / input tokens` is a lower bound of
//! the true ratio; the median of those samples is used. Read-only: nothing here changes a setting.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::BufRead;

use serde_json::Value;

use crate::analyze::Weights;

/// Default bytes per token for the prefix (JSON schemas and English) when calibration has too few
/// samples.
pub const DEFAULT_BYTES_PER_TOKEN: f64 = 2.5;
const MIN_CALIBRATION: usize = 5;

/// What a group of prefix content is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    System,
    BuiltIn,
    Mcp,
    Plugin,
    Listing,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::System => "system",
            Kind::BuiltIn => "built-in",
            Kind::Mcp => "mcp",
            Kind::Plugin => "plugin",
            Kind::Listing => "listing",
        }
    }
}

const SYSTEM: &str = "system prompt";
const BUILT_IN: &str = "built-in tools";
const SKILLS: &str = "skill listing";
const AGENTS: &str = "agent listing";

/// Group of a tool name: `mcp__plugin_<plugin>_<server>__<tool>` is a plugin server,
/// `mcp__<server>__<tool>` an MCP server, anything else a built-in tool.
pub fn group_of_tool(name: &str) -> (Kind, String) {
    if let Some(rest) = name.strip_prefix("mcp__") {
        let server = rest.split_once("__").map_or(rest, |(s, _)| s);
        if let Some(p) = server.strip_prefix("plugin_") {
            return match p.split_once('_') {
                Some((plugin, srv)) => (Kind::Plugin, format!("plugin:{plugin}:{srv}")),
                None => (Kind::Plugin, format!("plugin:{p}")),
            };
        }
        return (Kind::Mcp, format!("mcp:{server}"));
    }
    (Kind::BuiltIn, BUILT_IN.to_string())
}

/// Group of an MCP instructions block, named like `claude-in-chrome` or
/// `plugin:bio-research:biorxiv`.
fn group_of_server(name: &str) -> (Kind, String) {
    if name.starts_with("plugin:") {
        (Kind::Plugin, name.to_string())
    } else {
        (Kind::Mcp, format!("mcp:{name}"))
    }
}

#[derive(Debug, Default, Clone)]
struct Shape {
    system_bytes: u64,
    /// (tool name, definition bytes, deferred)
    tools: Vec<(String, u64, bool)>,
}

#[derive(Debug, Clone, Copy)]
struct Req {
    input: u64,
    cache_write: u64,
    cache_read: u64,
    write_5m: u64,
    write_1h: u64,
}

/// Prefix data of one transcript file (one session).
#[derive(Debug, Default)]
pub struct FileTools {
    shapes: HashMap<String, Shape>,
    shape_requests: HashMap<String, u64>,
    reqs: Vec<Req>,
    /// tool name → (calls, last call timestamp)
    calls: HashMap<String, (u64, String)>,
    skills_bytes: u64,
    deferred_lines: BTreeMap<String, u64>,
    instructions: BTreeMap<String, u64>,
    agents: BTreeMap<String, u64>,
    pub first_ts: Option<String>,
}

fn compact_len(v: &Value) -> u64 {
    serde_json::to_string(v).map_or(0, |s| s.len() as u64)
}

fn strs(v: &Value) -> impl Iterator<Item = &str> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str)
}

impl FileTools {
    pub fn from_reader<R: BufRead>(reader: R) -> Self {
        let mut f = FileTools::default();
        let mut seen: HashSet<String> = HashSet::new();
        for line in reader.lines().map_while(Result::ok) {
            // Cheap pre-filter: most lines are irrelevant.
            if !(line.contains("api-request")
                || line.contains("\"usage\"")
                || line.contains("\"tool_use\"")
                || line.contains("\"attachment\""))
            {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if f.first_ts.is_none() {
                if let Some(ts) = v["timestamp"].as_str() {
                    f.first_ts = Some(ts.to_string());
                }
            }
            match v["type"].as_str() {
                Some("api-request-shape") => f.shape(&v),
                Some("api-request") => {
                    if let Some(h) = v["shapeHash"].as_str() {
                        *f.shape_requests.entry(h.to_string()).or_default() += 1;
                    }
                }
                Some("attachment") => f.attachment(&v["attachment"]),
                Some("assistant") if v["isSidechain"] != true => f.assistant(&v, &mut seen),
                _ => {}
            }
        }
        f
    }

    fn shape(&mut self, v: &Value) {
        let Some(h) = v["shapeHash"].as_str() else {
            return;
        };
        let s = &v["shape"];
        let tools = s["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| {
                (
                    t["name"].as_str().unwrap_or("?").to_string(),
                    compact_len(t),
                    t["defer_loading"] == true,
                )
            })
            .collect();
        self.shapes.insert(
            h.to_string(),
            Shape {
                system_bytes: compact_len(&s["system"]),
                tools,
            },
        );
    }

    /// Listings are deltas, and compaction re-adds them: keep the current state, keyed by name.
    fn attachment(&mut self, a: &Value) {
        match a["type"].as_str() {
            Some("skill_listing") => {
                let n = a["content"].as_str().map_or(0, str::len) as u64;
                if a["isInitial"] == true || n > 0 {
                    self.skills_bytes = n;
                }
            }
            Some("deferred_tools_delta") => {
                for name in strs(&a["removedNames"]) {
                    self.deferred_lines.remove(name);
                }
                let lines: Vec<&str> = strs(&a["addedLines"]).collect();
                for (i, name) in strs(&a["addedNames"]).enumerate() {
                    let n = lines.get(i).map_or(name.len() + 1, |l| l.len() + 1);
                    self.deferred_lines.insert(name.to_string(), n as u64);
                }
            }
            Some("mcp_instructions_delta") => {
                for name in strs(&a["removedNames"]) {
                    self.instructions.remove(name);
                }
                let blocks = a["addedBlocks"].as_array();
                for (i, name) in strs(&a["addedNames"]).enumerate() {
                    let n = blocks.and_then(|b| b.get(i)).map_or(0, |b| {
                        b.as_str()
                            .map_or_else(|| compact_len(b), |s| s.len() as u64)
                    });
                    self.instructions.insert(name.to_string(), n);
                }
            }
            Some("agent_listing_delta") => {
                if a["isInitial"] == true {
                    self.agents.clear();
                }
                for name in strs(&a["removedTypes"]) {
                    self.agents.remove(name);
                }
                let lines: Vec<&str> = strs(&a["addedLines"]).collect();
                for (i, name) in strs(&a["addedTypes"]).enumerate() {
                    let n = lines.get(i).map_or(0, |l| l.len() + 1);
                    self.agents.insert(name.to_string(), n as u64);
                }
            }
            _ => {}
        }
    }

    fn assistant(&mut self, v: &Value, seen: &mut HashSet<String>) {
        let m = &v["message"];
        let ts = v["timestamp"].as_str().unwrap_or("");
        for c in m["content"].as_array().into_iter().flatten() {
            if c["type"] == "tool_use" {
                if let Some(name) = c["name"].as_str() {
                    let e = self.calls.entry(name.to_string()).or_default();
                    e.0 += 1;
                    if ts > e.1.as_str() {
                        e.1 = ts.to_string();
                    }
                }
            }
        }
        let u = &m["usage"];
        if !u.is_object() {
            return;
        }
        // One API response is split over several lines that repeat the same usage.
        if let Some(id) = m["id"].as_str() {
            if !seen.insert(id.to_string()) {
                return;
            }
        }
        let n = |k: &str| u[k].as_u64().unwrap_or(0);
        let cache_write = n("cache_creation_input_tokens");
        let write_1h = u["cache_creation"]["ephemeral_1h_input_tokens"]
            .as_u64()
            .unwrap_or(0)
            .min(cache_write);
        self.reqs.push(Req {
            input: n("input_tokens"),
            cache_write,
            cache_read: n("cache_read_input_tokens"),
            write_5m: cache_write - write_1h,
            write_1h,
        });
    }

    /// The shape used by the most requests (the last one written if none is referenced).
    fn main_shape(&self) -> Option<&Shape> {
        self.shapes
            .iter()
            .max_by_key(|(h, _)| self.shape_requests.get(*h).copied().unwrap_or(0))
            .map(|(_, s)| s)
    }

    /// Prefix bytes by group for this session.
    fn prefix_groups(&self) -> BTreeMap<(Kind, String), GroupBytes> {
        let mut g: BTreeMap<(Kind, String), GroupBytes> = BTreeMap::new();
        let Some(shape) = self.main_shape() else {
            return g;
        };
        g.entry((Kind::System, SYSTEM.into())).or_default().bytes += shape.system_bytes;
        for (name, bytes, deferred) in &shape.tools {
            let e = g.entry(group_of_tool(name)).or_default();
            if *deferred {
                e.deferred += 1;
            } else {
                e.loaded += 1;
                e.bytes += bytes;
            }
        }
        // Deferred tools that are only listed by name (not in the request's `tools` array).
        let in_shape: HashSet<&str> = shape.tools.iter().map(|t| t.0.as_str()).collect();
        for (name, bytes) in &self.deferred_lines {
            let e = g.entry(group_of_tool(name)).or_default();
            e.bytes += bytes;
            if !in_shape.contains(name.as_str()) {
                e.deferred += 1;
            }
        }
        for (name, bytes) in &self.instructions {
            g.entry(group_of_server(name)).or_default().bytes += bytes;
        }
        if self.skills_bytes > 0 {
            g.entry((Kind::Listing, SKILLS.into())).or_default().bytes += self.skills_bytes;
        }
        let agents: u64 = self.agents.values().sum();
        if agents > 0 {
            g.entry((Kind::Listing, AGENTS.into())).or_default().bytes += agents;
        }
        g
    }

    /// One calibration sample: prefix bytes / input tokens of a fresh first request.
    pub fn calibration_sample(&self) -> Option<f64> {
        let first = self.reqs.first()?;
        let tokens = first.input + first.cache_write;
        if first.cache_read > 0 || tokens == 0 {
            return None;
        }
        let bytes: u64 = self.prefix_groups().values().map(|b| b.bytes).sum();
        let r = bytes as f64 / tokens as f64;
        (bytes > 0 && (1.5..=6.0).contains(&r)).then_some(r)
    }

    pub fn has_prefix(&self) -> bool {
        !self.shapes.is_empty() && !self.reqs.is_empty()
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct GroupBytes {
    bytes: u64,
    loaded: usize,
    deferred: usize,
}

/// One row of the report.
#[derive(Debug, Clone)]
pub struct Group {
    pub kind: Kind,
    pub name: String,
    /// Mean prefix tokens per session in which the group is present.
    pub tokens: f64,
    pub tools_loaded: usize,
    pub tools_deferred: usize,
    /// Cost-weighted input units of this group's prefix content, over all sessions.
    pub units: f64,
    pub sessions: u64,
    pub calls: u64,
    pub last_used: Option<String>,
}

/// `aaes mine --tools` totals.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub sessions: u64,
    pub requests: u64,
    /// Cost-weighted input units of all requests (uncached-input equivalents).
    pub input_units: f64,
    /// Units of that which are the fixed prefix.
    pub prefix_units: f64,
    pub prefix_rewrites: u64,
    pub median_prefix_tokens: f64,
    pub bytes_per_token: f64,
    pub calibration_samples: usize,
    pub groups: Vec<Group>,
}

impl Summary {
    pub fn prefix_share(&self) -> f64 {
        if self.input_units > 0.0 {
            self.prefix_units / self.input_units
        } else {
            0.0
        }
    }

    /// MCP and plugin servers present in the prefix but never called in the window.
    pub fn unused(&self) -> impl Iterator<Item = &Group> {
        self.groups
            .iter()
            .filter(|g| matches!(g.kind, Kind::Mcp | Kind::Plugin) && g.calls == 0)
    }
}

/// Bytes per token: `fixed` if given, else the median of the calibration samples, else the
/// default.
pub fn bytes_per_token(files: &[FileTools], fixed: Option<f64>) -> (f64, usize) {
    let mut s: Vec<f64> = files
        .iter()
        .filter_map(FileTools::calibration_sample)
        .collect();
    if let Some(b) = fixed {
        return (b, s.len());
    }
    if s.len() < MIN_CALIBRATION {
        return (DEFAULT_BYTES_PER_TOKEN, s.len());
    }
    s.sort_by(f64::total_cmp);
    (s[s.len() / 2], s.len())
}

fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Summarize sessions that have prefix data. `anonymize` hashes MCP and plugin names.
pub fn summarize(
    files: &[FileTools],
    w: &Weights,
    bpt: f64,
    calibration: usize,
    anonymize: bool,
) -> Summary {
    let mut sum = Summary {
        bytes_per_token: bpt,
        calibration_samples: calibration,
        ..Summary::default()
    };
    let mut rows: BTreeMap<(Kind, String), Group> = BTreeMap::new();
    let mut prefix_tokens: Vec<f64> = Vec::new();
    for f in files.iter().filter(|f| f.has_prefix()) {
        let groups = f.prefix_groups();
        let total_bytes: u64 = groups.values().map(|b| b.bytes).sum();
        if total_bytes == 0 {
            continue;
        }
        let p = total_bytes as f64 / bpt;
        prefix_tokens.push(p);
        let mut session_prefix = 0.0;
        for r in &f.reqs {
            let units = r.input as f64 * w.uncached
                + r.cache_read as f64 * w.cache_read
                + r.write_5m as f64 * w.cache_write_5m
                + r.write_1h as f64 * w.cache_write_1h;
            let pr = p.min((r.input + r.cache_write + r.cache_read) as f64);
            let x = if pr > 0.0 && r.cache_write as f64 >= 0.9 * pr {
                sum.prefix_rewrites += 1;
                let wr = if r.write_1h > r.write_5m {
                    w.cache_write_1h
                } else {
                    w.cache_write_5m
                };
                pr * wr
            } else {
                pr * w.cache_read
            };
            sum.input_units += units;
            session_prefix += x.min(units);
            sum.requests += 1;
        }
        sum.prefix_units += session_prefix;
        sum.sessions += 1;
        for (key, b) in groups {
            let row = rows.entry(key.clone()).or_insert_with(|| Group {
                kind: key.0,
                name: key.1.clone(),
                tokens: 0.0,
                tools_loaded: 0,
                tools_deferred: 0,
                units: 0.0,
                sessions: 0,
                calls: 0,
                last_used: None,
            });
            row.tokens += b.bytes as f64 / bpt;
            row.tools_loaded = row.tools_loaded.max(b.loaded);
            row.tools_deferred = row.tools_deferred.max(b.deferred);
            row.units += session_prefix * b.bytes as f64 / total_bytes as f64;
            row.sessions += 1;
        }
        for (name, (n, ts)) in &f.calls {
            let key = group_of_tool(name);
            if let Some(row) = rows.get_mut(&key) {
                row.calls += n;
                if row.last_used.as_deref().is_none_or(|l| ts.as_str() > l) {
                    row.last_used = Some(ts.clone());
                }
            }
        }
    }
    prefix_tokens.sort_by(f64::total_cmp);
    sum.median_prefix_tokens = prefix_tokens
        .get(prefix_tokens.len() / 2)
        .copied()
        .unwrap_or(0.0);
    sum.groups = rows
        .into_values()
        .map(|mut g| {
            g.tokens /= g.sessions.max(1) as f64;
            if anonymize && matches!(g.kind, Kind::Mcp | Kind::Plugin) {
                g.name = format!("{}:{:08x}", g.kind.label(), fnv1a(&g.name));
            }
            g
        })
        .collect();
    sum.groups.sort_by(|a, b| b.units.total_cmp(&a.units));
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_map_to_groups() {
        assert_eq!(group_of_tool("Read"), (Kind::BuiltIn, BUILT_IN.into()));
        assert_eq!(
            group_of_tool("mcp__claude-in-chrome__navigate"),
            (Kind::Mcp, "mcp:claude-in-chrome".into())
        );
        assert_eq!(
            group_of_tool("mcp__plugin_bio-research_c-trials__search_trials"),
            (Kind::Plugin, "plugin:bio-research:c-trials".into())
        );
        assert_eq!(
            group_of_server("plugin:bio-research:c-trials"),
            (Kind::Plugin, "plugin:bio-research:c-trials".into())
        );
        assert_eq!(
            group_of_server("codegraph"),
            (Kind::Mcp, "mcp:codegraph".into())
        );
    }

    fn session() -> String {
        let big = "x".repeat(2400);
        let mut lines = vec![
            format!(
                r#"{{"type":"api-request-shape","shapeHash":"s1","timestamp":"2026-11-01T10:00:00Z","shape":{{"system":[{{"type":"text","text":"{big}"}}],"tools":[{{"name":"Read","description":"{big}"}},{{"name":"mcp__srv__q","description":"{big}"}},{{"name":"mcp__idle__q","description":"{big}"}},{{"name":"mcp__lazy__q","description":"{big}","defer_loading":true}}]}}}}"#
            ),
            r#"{"type":"api-request","shapeHash":"s1"}"#.to_string(),
            r#"{"type":"attachment","attachment":{"type":"deferred_tools_delta","addedNames":["mcp__lazy__q"],"addedLines":["mcp__lazy__q"]}}"#.to_string(),
            r#"{"type":"attachment","attachment":{"type":"deferred_tools_delta","addedNames":["mcp__lazy__q"],"addedLines":["mcp__lazy__q"]}}"#.to_string(),
        ];
        // First request writes the prefix (fresh), the next ones read it.
        lines.push(r#"{"type":"assistant","timestamp":"2026-11-01T10:00:01Z","message":{"id":"m1","usage":{"input_tokens":10,"cache_creation_input_tokens":5000,"cache_read_input_tokens":0,"output_tokens":1},"content":[{"type":"tool_use","name":"mcp__srv__q","input":{}}]}}"#.to_string());
        for i in 2..=10 {
            lines.push(format!(r#"{{"type":"assistant","timestamp":"2026-11-01T10:00:{i:02}Z","message":{{"id":"m{i}","usage":{{"input_tokens":10,"cache_creation_input_tokens":100,"cache_read_input_tokens":5000,"output_tokens":1}},"content":[{{"type":"tool_use","name":"Read","input":{{}}}}]}}}}"#));
        }
        lines.join("\n")
    }

    #[test]
    fn prefix_is_measured_split_by_group_and_unused_servers_listed() {
        let f = FileTools::from_reader(session().as_bytes());
        assert!(f.has_prefix());
        let g = f.prefix_groups();
        let lazy = g[&(Kind::Mcp, "mcp:lazy".to_string())];
        assert_eq!(
            (lazy.loaded, lazy.deferred),
            (0, 1),
            "deferred tool: name line only"
        );
        assert_eq!(
            lazy.bytes, 13,
            "listing line counted once despite the repeated delta"
        );
        let s = summarize(&[f], &Weights::default(), 2.5, 0, false);
        assert_eq!((s.sessions, s.requests, s.prefix_rewrites), (1, 10, 1));
        assert!(
            s.prefix_share() > 0.3 && s.prefix_share() < 1.0,
            "{}",
            s.prefix_share()
        );
        let unused: Vec<&str> = s.unused().map(|g| g.name.as_str()).collect();
        assert_eq!(unused, ["mcp:idle", "mcp:lazy"]);
        let srv = s.groups.iter().find(|g| g.name == "mcp:srv").unwrap();
        assert_eq!((srv.calls, srv.tools_loaded), (1, 1));
        let builtin = s.groups.iter().find(|g| g.kind == Kind::BuiltIn).unwrap();
        assert_eq!(builtin.calls, 9);
        let anon = summarize(
            &[FileTools::from_reader(session().as_bytes())],
            &Weights::default(),
            2.5,
            0,
            true,
        );
        assert!(
            anon.unused()
                .all(|g| g.name.starts_with("mcp:") && !g.name.contains("idle"))
        );
    }

    #[test]
    fn calibration_uses_fresh_first_requests_only() {
        let f = FileTools::from_reader(session().as_bytes());
        let r = f.calibration_sample().unwrap();
        assert!((1.5..=6.0).contains(&r));
        let (b, n) = bytes_per_token(std::slice::from_ref(&f), None);
        assert_eq!(
            (b, n),
            (DEFAULT_BYTES_PER_TOKEN, 1),
            "too few samples: default"
        );
        assert_eq!(bytes_per_token(&[f], Some(3.0)).0, 3.0);
    }
}
