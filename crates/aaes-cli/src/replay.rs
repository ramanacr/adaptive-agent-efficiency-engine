//! `aaes replay`: run the current shapers over the tool results in past Claude Code transcripts
//! (read-only) and report what each shaping family would have saved, token-weighted and
//! cache-aware.
//!
//! Claude Code stores each tool result twice: as the text the model saw and as `toolUseResult`,
//! which has the same shape as the hook's `tool_response`. Replay feeds `toolUseResult` to the
//! same decision path as the hook (`hook.rs`), including the repeat rule (a second whole read of a
//! file in one context returns it in full) and its reset at compaction.
//!
//! The figures are an upper bound: the agent saw the full output, so the follow-up reads that a
//! shaped result can cause are not in the history. Results that AAES already shaped live are
//! counted apart; their savings are in `aaes report`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use aaes_core::shape::{self, ShapeConfig};
use aaes_runtime::hook::read_key;
use aaes_runtime::{Config, paths};
use aaes_transcripts::analyze::Weights;
use aaes_transcripts::discover;
use aaes_transcripts::edits::parse_iso_ms;
use aaes_transcripts::occupancy::Timeline;
use anyhow::{Result, bail};
use clap::Args;
use rayon::prelude::*;
use serde_json::{Value, json};

#[derive(Args)]
pub struct ReplayArgs {
    /// Claude Code projects dir [default: $CLAUDE_CONFIG_DIR/projects or ~/.claude/projects]
    #[arg(long)]
    claude_dir: Option<PathBuf>,
    /// Only tool results on/after this date (ISO-8601 prefix, e.g. 2026-10-01).
    #[arg(long)]
    since: Option<String>,
    /// Only project folders whose name contains this text (case-insensitive).
    #[arg(long)]
    project: Option<String>,
    /// Use the built-in shaping thresholds, not the ones in AAES_HOME/config.json.
    #[arg(long)]
    defaults: bool,
    /// Rows in the per-project table.
    #[arg(long, default_value_t = 10)]
    top: usize,
    /// Print JSON instead of text.
    #[arg(long)]
    json: bool,
}

const FAMILIES: [&str; 3] = ["read", "shell_read", "search"];

/// Per-family replay totals.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Fam {
    /// Tool results of this family that replay looked at.
    pub results: u64,
    pub raw_bytes: u64,
    /// Results the shaper would have changed.
    pub shaped: u64,
    pub shaped_raw_bytes: u64,
    pub shaped_out_bytes: u64,
    /// Saved tokens weighted by how many later requests resend them (cache-aware).
    pub saved_units: f64,
    pub later_sum: u64,
    /// Pass-through reasons.
    pub small: u64,
    pub repeat: u64,
    pub ranged: u64,
    pub unparsed: u64,
    pub low_saving: u64,
    pub host_truncated: u64,
    /// Already shaped by AAES when it happened (raw size unknown).
    pub live: u64,
}

impl Fam {
    fn add(&mut self, o: &Fam) {
        self.results += o.results;
        self.raw_bytes += o.raw_bytes;
        self.shaped += o.shaped;
        self.shaped_raw_bytes += o.shaped_raw_bytes;
        self.shaped_out_bytes += o.shaped_out_bytes;
        self.saved_units += o.saved_units;
        self.later_sum += o.later_sum;
        self.small += o.small;
        self.repeat += o.repeat;
        self.ranged += o.ranged;
        self.unparsed += o.unparsed;
        self.low_saving += o.low_saving;
        self.host_truncated += o.host_truncated;
        self.live += o.live;
    }

    fn saved_bytes(&self) -> u64 {
        self.shaped_raw_bytes.saturating_sub(self.shaped_out_bytes)
    }
}

/// Replay totals of one or more transcript files.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tally {
    pub fams: BTreeMap<&'static str, Fam>,
    /// Results that carried no timestamp or no matching model request (no cost weight).
    pub unweighted: u64,
}

impl Tally {
    fn fam(&mut self, k: &'static str) -> &mut Fam {
        self.fams.entry(k).or_default()
    }

    fn add(&mut self, o: &Tally) {
        for (k, f) in &o.fams {
            self.fam(k).add(f);
        }
        self.unweighted += o.unweighted;
    }

    fn saved_units(&self) -> f64 {
        self.fams.values().map(|f| f.saved_units).sum()
    }
}

/// Text the model saw for one `tool_result` item.
fn result_text(item: &Value) -> String {
    match &item["content"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Output that AAES search shaping produced (its closing summary line).
fn live_search(text: &str) -> bool {
    text.lines()
        .rev()
        .find(|l| !l.is_empty())
        .is_some_and(|l| l.starts_with("[aaes] ") && l.contains("each path shown once"))
}

struct Replayer<'a> {
    config: &'a Config,
    timeline: &'a Timeline,
    since: Option<&'a str>,
    /// Whole-file reads seen in the current context since the last compaction.
    seen: HashSet<String>,
    /// tool_use id → (tool name, input).
    calls: HashMap<String, (String, Value)>,
    tally: Tally,
}

impl<'a> Replayer<'a> {
    /// Shaping settings for the result's working folder (repo override or global).
    fn shape_for(&self, cwd: Option<&str>) -> &'a ShapeConfig {
        self.config.for_cwd(cwd).shape
    }

    fn line(&mut self, line: &str) {
        let compact = line.contains("compact_boundary") || line.contains("isCompactSummary");
        let call = line.contains(r#""tool_use""#);
        let result = line.contains(r#""toolUseResult""#);
        if !compact && !call && !result {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if (v["type"] == "system" && v["subtype"] == "compact_boundary")
            || v["isCompactSummary"].as_bool() == Some(true)
        {
            self.seen.clear();
            return;
        }
        let Some(items) = v["message"]["content"].as_array() else {
            return;
        };
        if v["type"] == "assistant" {
            for it in items.iter().filter(|it| it["type"] == "tool_use") {
                let name = it["name"].as_str().unwrap_or_default();
                if matches!(name, "Read" | "Grep" | "Bash" | "PowerShell") {
                    if let Some(id) = it["id"].as_str() {
                        self.calls
                            .insert(id.to_owned(), (name.to_owned(), it["input"].clone()));
                    }
                }
            }
            return;
        }
        // `toolUseResult` belongs to the line's single tool result.
        let results: Vec<&Value> = items
            .iter()
            .filter(|it| it["type"] == "tool_result")
            .collect();
        let [item] = results.as_slice() else {
            return;
        };
        let Some((tool, input)) = item["tool_use_id"]
            .as_str()
            .and_then(|id| self.calls.remove(id))
        else {
            return;
        };
        let ts_str = v["timestamp"].as_str().unwrap_or_default();
        if self.since.is_some_and(|s| ts_str < s) {
            return;
        }
        if item["is_error"].as_bool() == Some(true) {
            return;
        }
        let ts = parse_iso_ms(ts_str);
        let resp = &v["toolUseResult"];
        let cwd = v["cwd"].as_str();
        let seen_text = result_text(item);
        self.result(&tool, &input, resp, cwd, &seen_text, ts);
    }

    fn weight(&mut self, ts: Option<u64>) -> (f64, u64) {
        match ts {
            Some(t) if !self.timeline.is_empty() => {
                (self.timeline.weight_at(t), self.timeline.later_requests(t))
            }
            _ => {
                self.tally.unweighted += 1;
                (0.0, 0)
            }
        }
    }

    fn shaped(&mut self, fam: &'static str, raw: usize, out: usize, ts: Option<u64>) {
        let (w, k) = self.weight(ts);
        let saved_tokens = raw.saturating_sub(out) as f64 / self.config.shape.bytes_per_token;
        let f = self.tally.fam(fam);
        f.shaped += 1;
        f.shaped_raw_bytes += raw as u64;
        f.shaped_out_bytes += out as u64;
        f.saved_units += saved_tokens * w;
        f.later_sum += k;
    }

    /// The hook's whole-file read path: repeat rule, then the shaper.
    fn read(
        &mut self,
        fam: &'static str,
        path: &str,
        content: &str,
        cwd: Option<&str>,
        ts: Option<u64>,
    ) {
        if !self.seen.insert(read_key(path, cwd)) {
            self.tally.fam(fam).repeat += 1;
            return;
        }
        let Some(r) = shape::shape_read(path, content, self.shape_for(cwd)) else {
            self.tally.fam(fam).small += 1;
            return;
        };
        let shown = shape::display_rel(path, cwd);
        let out = if fam == "read" {
            r.head.len() + shape::read_context(shown, &r, r.total_lines).len()
        } else {
            shape::shell_read_text(shown, &r).len()
        };
        self.shaped(fam, content.len(), out, ts);
    }

    fn search(&mut self, raw: &str, cwd: Option<&str>, single: Option<&str>, ts: Option<u64>) {
        let single = single.map(|p| shape::display_rel(p, cwd));
        match shape::shape_search(raw, cwd, self.shape_for(cwd), Some("h_replay"), single) {
            Ok(s) => self.shaped("search", raw.len(), s.text.len(), ts),
            Err(skip) => {
                let f = self.tally.fam("search");
                match skip {
                    shape::SearchSkip::Small => f.small += 1,
                    shape::SearchSkip::Unparsed { .. } => f.unparsed += 1,
                    shape::SearchSkip::LowSaving => f.low_saving += 1,
                }
            }
        }
    }

    fn count(&mut self, fam: &'static str, bytes: usize) {
        let f = self.tally.fam(fam);
        f.results += 1;
        f.raw_bytes += bytes as u64;
    }

    fn result(
        &mut self,
        tool: &str,
        ti: &Value,
        resp: &Value,
        cwd: Option<&str>,
        seen_text: &str,
        ts: Option<u64>,
    ) {
        match tool {
            "Read" => {
                if resp["type"].as_str() != Some("text") {
                    return;
                }
                let (Some(path), Some(content)) =
                    (ti["file_path"].as_str(), resp["file"]["content"].as_str())
                else {
                    return;
                };
                self.count("read", content.len());
                if !ti["offset"].is_null() || !ti["limit"].is_null() {
                    self.tally.fam("read").ranged += 1;
                    return;
                }
                let num = resp["file"]["numLines"].as_u64().unwrap_or(0);
                let total = resp["file"]["totalLines"].as_u64().unwrap_or(0);
                if num > 0 && num <= self.shape_for(cwd).head_lines as u64 && total > num {
                    // AAES replaced the content with the head lines.
                    self.seen.insert(read_key(path, cwd));
                    self.tally.fam("read").live += 1;
                    return;
                }
                self.read("read", path, content, cwd, ts);
            }
            "Grep" => {
                if resp["mode"].as_str() != Some("content") {
                    return;
                }
                let Some(content) = resp["content"].as_str() else {
                    return;
                };
                self.count("search", content.len());
                if live_search(content) {
                    self.tally.fam("search").live += 1;
                    return;
                }
                self.search(content, cwd, ti["path"].as_str(), ts);
            }
            "Bash" | "PowerShell" => {
                let (Some(cmd), Some(stdout)) = (ti["command"].as_str(), resp["stdout"].as_str())
                else {
                    return;
                };
                if resp["interrupted"].as_bool() == Some(true)
                    || resp["isImage"].as_bool() == Some(true)
                    || cmd.contains("AAES_RAW=1")
                {
                    return;
                }
                let target = shape::shell_read_target(cmd);
                let fam = if target.is_some() {
                    "shell_read"
                } else if shape::is_shell_search(cmd) {
                    "search"
                } else {
                    return;
                };
                self.count(fam, stdout.len());
                if seen_text.contains("<persisted-output>")
                    || resp["persistedOutputPath"]
                        .as_str()
                        .is_some_and(|p| !p.is_empty())
                {
                    self.tally.fam(fam).host_truncated += 1;
                    return;
                }
                match target {
                    Some(t) => {
                        if stdout.starts_with("[aaes] Large file") {
                            self.seen.insert(read_key(&t, cwd));
                            self.tally.fam(fam).live += 1;
                            return;
                        }
                        self.read(fam, &t, stdout, cwd, ts);
                    }
                    None => {
                        if live_search(stdout) {
                            self.tally.fam(fam).live += 1;
                            return;
                        }
                        self.search(stdout, cwd, None, ts);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Replays one transcript file (one context: the main conversation or one sub-agent).
pub fn replay_file(path: &Path, cfg: &Config, since: Option<&str>) -> Tally {
    let open = || File::open(path).map(|f| BufReader::with_capacity(1 << 16, f));
    let Ok(r) = open() else {
        return Tally::default();
    };
    let timeline = Timeline::from_reader(r, &Weights::default());
    let Ok(r) = open() else {
        return Tally::default();
    };
    replay_reader(r, cfg, &timeline, since)
}

pub fn replay_reader<R: BufRead>(
    reader: R,
    cfg: &Config,
    timeline: &Timeline,
    since: Option<&str>,
) -> Tally {
    let mut r = Replayer {
        config: cfg,
        timeline,
        since,
        seen: HashSet::new(),
        calls: HashMap::new(),
        tally: Tally::default(),
    };
    for line in reader.lines().map_while(Result::ok) {
        r.line(&line);
    }
    r.tally
}

pub fn run(a: ReplayArgs) -> Result<()> {
    let Some(root) = a.claude_dir.clone().or_else(discover::default_claude_root) else {
        bail!("Claude Code projects folder not found; pass --claude-dir");
    };
    if !root.is_dir() {
        bail!("Claude Code transcripts not found at {}", root.display());
    }
    let cfg = if a.defaults {
        Config::default()
    } else {
        Config::load(&paths::aaes_home())
    };
    let shape_cfg = cfg.shape.clone();
    let filter = a.project.as_deref().map(str::to_lowercase);
    let project_of = |p: &Path| -> String {
        p.strip_prefix(&root)
            .ok()
            .and_then(|r| r.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let files: Vec<(String, PathBuf)> = discover::jsonl_files(&root)
        .into_iter()
        .map(|p| (project_of(&p), p))
        .filter(|(proj, _)| {
            filter
                .as_deref()
                .is_none_or(|f| proj.to_lowercase().contains(f))
        })
        .collect();
    if files.is_empty() {
        bail!("no transcript files found under {}", root.display());
    }
    eprintln!(
        "aaes: replaying {} transcript files under {}",
        files.len(),
        root.display()
    );
    let per_file: Vec<(String, Tally)> = files
        .par_iter()
        .map(|(proj, p)| (proj.clone(), replay_file(p, &cfg, a.since.as_deref())))
        .collect();
    let mut total = Tally::default();
    let mut by_project: BTreeMap<String, Tally> = BTreeMap::new();
    for (proj, t) in &per_file {
        total.add(t);
        by_project.entry(proj.clone()).or_default().add(t);
    }
    let mut projects: Vec<(String, Tally)> = by_project.into_iter().collect();
    projects.sort_by(|x, y| y.1.saved_units().total_cmp(&x.1.saved_units()));

    let bpt = shape_cfg.bytes_per_token;
    if a.json {
        let fam_json = |t: &Tally| -> Value {
            let mut m = serde_json::Map::new();
            for k in FAMILIES {
                let f = t.fams.get(k).cloned().unwrap_or_default();
                m.insert(
                    k.to_owned(),
                    json!({
                        "results": f.results,
                        "raw_tokens": tok(f.raw_bytes, bpt),
                        "would_shape": f.shaped,
                        "shaped_raw_tokens": tok(f.shaped_raw_bytes, bpt),
                        "shaped_out_tokens": tok(f.shaped_out_bytes, bpt),
                        "saved_tokens": tok(f.saved_bytes(), bpt),
                        "saved_cost_weighted": f.saved_units.round() as u64,
                        "avg_later_requests": avg(f.later_sum, f.shaped),
                        "pass_through": {
                            "small": f.small,
                            "repeat": f.repeat,
                            "ranged": f.ranged,
                            "unparsed": f.unparsed,
                            "low_saving": f.low_saving,
                            "host_truncated": f.host_truncated,
                        },
                        "already_shaped_live": f.live,
                    }),
                );
            }
            Value::Object(m)
        };
        let out = json!({
            "files": files.len(),
            "since": a.since,
            "config": {
                "source": if a.defaults { "defaults" } else { "config.json" },
                "bytes_per_token": bpt,
                "read_threshold_tokens": shape_cfg.read_threshold_tokens,
                "search_threshold_tokens": shape_cfg.search_threshold_tokens,
                "head_lines": shape_cfg.head_lines,
                "search_max_lines_per_file": shape_cfg.search_max_lines_per_file,
            },
            "families": fam_json(&total),
            "unweighted_results": total.unweighted,
            "projects": projects.iter().take(a.top).map(|(p, t)| json!({
                "project": p,
                "saved_cost_weighted": t.saved_units().round() as u64,
                "families": fam_json(t),
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!(
        "AAES replay: {} transcript files{}{}",
        files.len(),
        a.since
            .as_deref()
            .map(|s| format!(", since {s}"))
            .unwrap_or_default(),
        a.project
            .as_deref()
            .map(|p| format!(", projects matching \"{p}\""))
            .unwrap_or_default(),
    );
    println!(
        "Thresholds ({}): read {} tokens, search {} tokens, {} lines per file",
        if a.defaults {
            "defaults"
        } else {
            "config.json"
        },
        shape_cfg.read_threshold_tokens,
        shape_cfg.search_threshold_tokens,
        shape_cfg.search_max_lines_per_file,
    );
    println!();
    println!(
        "{:<11} {:>8} {:>10} {:>12} {:>12} {:>7} {:>14} {:>10}",
        "family",
        "results",
        "would shape",
        "raw tok",
        "saved tok",
        "saved",
        "cost-weighted",
        "later req"
    );
    for k in FAMILIES {
        let f = total.fams.get(k).cloned().unwrap_or_default();
        println!(
            "{:<11} {:>8} {:>10} {:>12} {:>12} {:>6.1}% {:>14} {:>10}",
            k,
            f.results,
            f.shaped,
            tok(f.raw_bytes, bpt),
            tok(f.saved_bytes(), bpt),
            pct(f.saved_bytes(), f.raw_bytes),
            f.saved_units.round() as u64,
            avg(f.later_sum, f.shaped).map_or_else(|| "-".to_owned(), |x| format!("{x:.1}")),
        );
    }
    println!();
    println!("Pass-through reasons:");
    println!(
        "{:<11} {:>7} {:>7} {:>7} {:>9} {:>10} {:>14} {:>13}",
        "family",
        "small",
        "repeat",
        "ranged",
        "unparsed",
        "low saving",
        "host truncated",
        "shaped live"
    );
    for k in FAMILIES {
        let f = total.fams.get(k).cloned().unwrap_or_default();
        println!(
            "{:<11} {:>7} {:>7} {:>7} {:>9} {:>10} {:>14} {:>13}",
            k, f.small, f.repeat, f.ranged, f.unparsed, f.low_saving, f.host_truncated, f.live
        );
    }
    if projects.len() > 1 {
        println!();
        println!("Top projects by cost-weighted saving:");
        for (p, t) in projects.iter().take(a.top) {
            let get = |k: &str| t.fams.get(k).map_or(0, |f| f.saved_units.round() as u64);
            println!(
                "  {:>12}  read {:>10}  shell_read {:>9}  search {:>10}  {}",
                t.saved_units().round() as u64,
                get("read"),
                get("shell_read"),
                get("search"),
                p
            );
        }
    }
    println!();
    println!(
        "Upper bound: the agent saw the full output, so follow-up reads that shaping causes are not counted. \
         Compare with the follow-up figures in `aaes report`. Cost-weighted = uncached-input-equivalent tokens."
    );
    if total.unweighted > 0 {
        println!(
            "{} shaped results had no matching model request; they count as 0 cost-weighted.",
            total.unweighted
        );
    }
    Ok(())
}

fn tok(bytes: u64, bpt: f64) -> u64 {
    (bytes as f64 / bpt).round() as u64
}

fn pct(a: u64, b: u64) -> f64 {
    if b == 0 {
        0.0
    } else {
        a as f64 * 100.0 / b as f64
    }
}

fn avg(sum: u64, n: u64) -> Option<f64> {
    (n > 0).then(|| sum as f64 / n as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_source(n: usize) -> String {
        let mut s = String::from("namespace Demo;\npublic class Big\n{\n");
        for i in 0..n {
            s.push_str(&format!(
                "    public int Method{i}(int x)\n    {{\n        var y = x * {i};\n        return y + {i};\n    }}\n\n"
            ));
        }
        s.push_str("}\n");
        s
    }

    fn call(id: &str, name: &str, input: Value, ts: &str) -> String {
        json!({
            "type": "assistant", "timestamp": ts,
            "message": { "id": format!("m_{id}"), "content": [{ "type": "tool_use", "id": id, "name": name, "input": input }],
                         "usage": { "input_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0 } }
        })
        .to_string()
    }

    fn result(id: &str, resp: Value, text: &str, ts: &str) -> String {
        json!({
            "type": "user", "timestamp": ts, "cwd": "D:/repo",
            "message": { "content": [{ "type": "tool_result", "tool_use_id": id, "content": text }] },
            "toolUseResult": resp
        })
        .to_string()
    }

    fn request(n: u32, ts: &str) -> String {
        json!({
            "type": "assistant", "timestamp": ts,
            "message": { "id": format!("r{n}"), "content": [{ "type": "text", "text": "ok" }],
                         "usage": { "input_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0 } }
        })
        .to_string()
    }

    fn run_lines(lines: &[String]) -> Tally {
        let text = lines.join("\n");
        let t = Timeline::from_reader(text.as_bytes(), &Weights::default());
        replay_reader(text.as_bytes(), &Config::default(), &t, None)
    }

    fn read_resp(content: &str) -> Value {
        let n = content.lines().count();
        json!({ "type": "text", "file": { "filePath": "D:/repo/Big.cs", "content": content, "numLines": n, "startLine": 1, "totalLines": n } })
    }

    #[test]
    fn whole_read_is_shaped_once_then_full_until_compaction() {
        let src = big_source(200);
        let input = json!({ "file_path": "D:/repo/Big.cs" });
        let lines = vec![
            call("a", "Read", input.clone(), "2026-10-01T10:00:00Z"),
            result("a", read_resp(&src), "x", "2026-10-01T10:00:01Z"),
            request(1, "2026-10-01T10:00:02Z"),
            request(2, "2026-10-01T10:00:03Z"),
            call("b", "Read", input.clone(), "2026-10-01T10:00:04Z"),
            result("b", read_resp(&src), "x", "2026-10-01T10:00:05Z"),
            json!({ "type": "system", "subtype": "compact_boundary", "timestamp": "2026-10-01T10:00:06Z" }).to_string(),
            call("c", "Read", input, "2026-10-01T10:00:07Z"),
            result("c", read_resp(&src), "x", "2026-10-01T10:00:08Z"),
        ];
        let t = run_lines(&lines);
        let f = &t.fams["read"];
        assert_eq!((f.results, f.shaped, f.repeat), (3, 2, 1));
        assert!(f.saved_bytes() > src.len() as u64);
        // The first result is resent by 3 later requests (two plain ones, plus the call "b"),
        // the one after compaction by none.
        assert_eq!(f.later_sum, 3);
        assert!(f.saved_units > 0.0);
    }

    #[test]
    fn ranged_live_and_small_reads_are_not_shaped() {
        let src = big_source(200);
        let mut live = read_resp("head\n");
        live["file"]["numLines"] = json!(1);
        live["file"]["totalLines"] = json!(900);
        let lines = vec![
            call(
                "a",
                "Read",
                json!({ "file_path": "D:/repo/Big.cs", "offset": 10, "limit": 5 }),
                "2026-10-01T10:00:00Z",
            ),
            result("a", read_resp(&src), "x", "2026-10-01T10:00:01Z"),
            call(
                "b",
                "Read",
                json!({ "file_path": "D:/repo/Other.cs" }),
                "2026-10-01T10:00:02Z",
            ),
            result("b", live, "x", "2026-10-01T10:00:03Z"),
            call(
                "c",
                "Read",
                json!({ "file_path": "D:/repo/Small.cs" }),
                "2026-10-01T10:00:04Z",
            ),
            result("c", read_resp("class A {}\n"), "x", "2026-10-01T10:00:05Z"),
        ];
        let f = &run_lines(&lines).fams["read"];
        assert_eq!(
            (f.results, f.shaped, f.ranged, f.live, f.small),
            (3, 0, 1, 1, 1)
        );
    }

    #[test]
    fn grep_and_shell_search_are_shaped_and_persisted_output_skipped() {
        let mut grep = String::new();
        for f in 0..30 {
            for l in 0..40 {
                grep.push_str(&format!(
                    "src/module_{f}/file_{f}.rs:{l}: let value_{l} = compute_something({l});\n"
                ));
            }
        }
        let lines = vec![
            call(
                "a",
                "Grep",
                json!({ "pattern": "x", "output_mode": "content" }),
                "2026-10-01T10:00:00Z",
            ),
            result(
                "a",
                json!({ "mode": "content", "content": grep }),
                "x",
                "2026-10-01T10:00:01Z",
            ),
            request(1, "2026-10-01T10:00:02Z"),
            call(
                "b",
                "Bash",
                json!({ "command": "rg -n value src" }),
                "2026-10-01T10:00:03Z",
            ),
            result(
                "b",
                json!({ "stdout": grep, "stderr": "", "interrupted": false, "isImage": false }),
                "x",
                "2026-10-01T10:00:04Z",
            ),
            call(
                "c",
                "Bash",
                json!({ "command": "rg -n value src" }),
                "2026-10-01T10:00:05Z",
            ),
            result(
                "c",
                json!({ "stdout": grep, "stderr": "", "interrupted": false, "isImage": false }),
                "<persisted-output>\nOutput too large",
                "2026-10-01T10:00:06Z",
            ),
            call(
                "d",
                "Bash",
                json!({ "command": "cargo build" }),
                "2026-10-01T10:00:07Z",
            ),
            result(
                "d",
                json!({ "stdout": grep, "stderr": "", "interrupted": false, "isImage": false }),
                "x",
                "2026-10-01T10:00:08Z",
            ),
        ];
        let t = run_lines(&lines);
        let f = &t.fams["search"];
        assert_eq!((f.results, f.shaped, f.host_truncated), (3, 2, 1));
        assert!(f.saved_bytes() > grep.len() as u64);
        assert!(!t.fams.contains_key("other"));
    }

    #[test]
    fn live_shaped_search_is_recognised() {
        assert!(live_search(
            "a.rs (2 matches)\n  1:x\n[aaes] 2 matches in 1 file; each path shown once.\n"
        ));
        assert!(!live_search("src/a.rs:1: each path shown once\n"));
    }
}
