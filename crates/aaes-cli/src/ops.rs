//! Runtime commands: `hook`, `serve`, `show`, `report`, `enable`/`disable`.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aaes_runtime::{Runtime, hook, log, paths, server, store};
use anyhow::{Result, bail};
use serde_json::{Value, json};

/// Longest wait for hook input. Claude Code writes the event and closes stdin at once; a caller
/// that leaves the pipe open (a tool shell, a script) must not hang the hook or the session start.
const STDIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Hook input from stdin. Returns `Null` when stdin is an interactive terminal (run by hand) or
/// stays open past [`STDIN_TIMEOUT`], so `aaes serve --ensure` never waits for input.
fn read_stdin_json() -> Value {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return Value::Null;
    }
    read_json_from(std::io::stdin(), STDIN_TIMEOUT)
}

/// Reads all of `src` on a helper thread and parses it as JSON. On timeout the thread stays
/// blocked in `read`; the process exits soon after, which ends it.
fn read_json_from<R: Read + Send + 'static>(mut src: R, timeout: Duration) -> Value {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = src.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    let buf = match rx.recv_timeout(timeout) {
        Ok(b) => b,
        Err(_) => {
            log::warn(
                "cli",
                "hook stdin not closed in time; input ignored",
                json!({ "timeout_ms": timeout.as_millis() as u64 }),
            );
            return Value::Null;
        }
    };
    parse_hook_json(&buf)
}

fn parse_hook_json(buf: &str) -> Value {
    let buf = buf.trim_start_matches('\u{feff}'); // PowerShell pipes a UTF-8 BOM
    if buf.trim().is_empty() {
        return Value::Null;
    }
    match serde_json::from_str(buf) {
        Ok(v) => v,
        Err(e) => {
            log::warn(
                "cli",
                "hook stdin is not valid JSON; ignored",
                json!({ "bytes": buf.len(), "err": e.to_string() }),
            );
            Value::Null
        }
    }
}

/// Command-mode hook: event JSON on stdin, response JSON on stdout. Never fails the tool call.
pub fn hook_cmd() -> Result<()> {
    let rt = Runtime::from_env();
    let input = read_stdin_json();
    let out = hook::handle_safe(&rt, &input);
    if out.as_object().is_some_and(|o| !o.is_empty()) {
        print!("{out}");
    }
    Ok(())
}

/// Windows: our stdin/stdout/stderr are the hook's pipes from Claude Code. A child would inherit
/// them, and Claude Code waits for those pipes to close before it treats the hook as finished, so
/// a long-lived server would stall the session (seen in the M1 contract test). Clear the inherit
/// flag first.
#[cfg(windows)]
#[allow(unsafe_code)]
fn disinherit_std_handles() {
    use windows_sys::Win32::Foundation::{
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle returns this process's own standard handle (or null/invalid, which
        // we skip). Clearing HANDLE_FLAG_INHERIT only affects future child processes; the handle
        // stays valid for our own use.
        unsafe {
            let h = GetStdHandle(id);
            if !h.is_null() && h != INVALID_HANDLE_VALUE {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

fn spawn_detached(port: u16) -> Result<()> {
    #[cfg(windows)]
    disinherit_std_handles();
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.args(["serve", "--port", &port.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // Break away from the hook's job object if allowed, so the server outlives the hook.
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        if cmd.spawn().is_ok() {
            return Ok(());
        }
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()?;
    Ok(())
}

/// `aaes serve [--ensure|--stop]`.
pub fn serve_cmd(port: Option<u16>, ensure: bool, stop: bool) -> Result<()> {
    let rt = Runtime::from_env();
    let port = port.unwrap_or(rt.cfg.port);
    if stop {
        let token = server::token(&rt.home)?;
        if server::shutdown(port, &token) {
            eprintln!("aaes: server on port {port} stopped");
        } else {
            eprintln!("aaes: no server responding on port {port}");
        }
        return Ok(());
    }
    if ensure {
        // Called as a SessionStart hook: apply the event, make sure the server runs, print nothing.
        let input = read_stdin_json();
        if !input.is_null() {
            let out = hook::handle_safe(&rt, &input);
            if out.as_object().is_some_and(|o| !o.is_empty()) {
                print!("{out}");
            }
        }
        if let Err(e) = server::token(&rt.home) {
            log::error(
                "ensure",
                "cannot create hook token",
                json!({ "err": e.to_string() }),
            );
        }
        if server::healthy(port, Duration::from_millis(300)) {
            log::debug("ensure", "server already running", json!({ "port": port }));
            return Ok(());
        }
        let t0 = Instant::now();
        if let Err(e) = spawn_detached(port) {
            log::error(
                "ensure",
                "failed to start server; hooks will pass output through",
                json!({ "port": port, "err": e.to_string() }),
            );
            return Ok(());
        }
        let mut up = false;
        while t0.elapsed() < Duration::from_millis(2000) {
            if server::healthy(port, Duration::from_millis(200)) {
                up = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let ms = t0.elapsed().as_millis() as u64;
        if up {
            log::info(
                "ensure",
                "server started",
                json!({ "port": port, "startup_ms": ms }),
            );
        } else {
            log::warn(
                "ensure",
                "server not healthy within 2 s (still starting, or failed: see server log)",
                json!({ "port": port, "waited_ms": ms }),
            );
        }
        return Ok(());
    }
    if server::healthy(port, Duration::from_millis(300)) {
        bail!("an AAES server is already running on port {port}");
    }
    eprintln!(
        "aaes: serving hooks on http://127.0.0.1:{port}/aaes/v1/hook (data: {})",
        rt.home.display()
    );
    server::serve(Arc::new(rt), port)?;
    Ok(())
}

/// `aaes show <handle>`: print a stored raw output, optionally filtered.
pub fn show_cmd(
    handle: &str,
    lines: Option<&str>,
    grep: Option<&str>,
    head: Option<usize>,
    tail: Option<usize>,
) -> Result<()> {
    let rt = Runtime::from_env();
    let Some(text) = store::load(&rt.home, handle) else {
        bail!("unknown or expired handle: {handle}");
    };
    let mut selected: Vec<(usize, &str)> =
        text.lines().enumerate().map(|(i, l)| (i + 1, l)).collect();
    if let Some(spec) = lines {
        let (a, b) = spec.split_once('-').unwrap_or((spec, spec));
        let (a, b): (usize, usize) = (a.trim().parse()?, b.trim().parse()?);
        selected.retain(|(n, _)| *n >= a && *n <= b);
    }
    if let Some(g) = grep {
        let g = g.to_lowercase();
        selected.retain(|(_, l)| l.to_lowercase().contains(&g));
    }
    if let Some(h) = head {
        selected.truncate(h);
    }
    if let Some(t) = tail {
        let skip = selected.len().saturating_sub(t);
        selected.drain(..skip);
    }
    for (_, l) in selected {
        println!("{l}");
    }
    Ok(())
}

#[derive(Default)]
struct Agg {
    n: u64,
    raw: u64,
    delivered: u64,
}

/// `aaes report`: totals from the local event logs (content-free).
pub fn report_cmd(json_out: bool) -> Result<()> {
    let rt = Runtime::from_env();
    let bpt = rt.cfg.shape.bytes_per_token;
    let mut by: BTreeMap<(String, String), Agg> = BTreeMap::new();
    let (mut sessions, mut rebound_bytes, mut rebound_n) = (0u64, 0u64, 0u64);
    let mut fam: BTreeMap<&'static str, Family> = BTreeMap::new();
    // Same totals split by "working folder is in a CodeGraph-indexed repo" (old events: unknown).
    let mut idx: BTreeMap<&'static str, Family> = BTreeMap::new();
    let mut edits = Edits::default();
    let mut by_repo: BTreeMap<String, BTreeMap<&'static str, Family>> = BTreeMap::new();
    let claude_root = aaes_transcripts::discover::default_claude_root();
    if let Ok(rd) = std::fs::read_dir(rt.home.join("sessions")) {
        for e in rd.flatten() {
            let Ok(text) = std::fs::read_to_string(e.path().join("events.jsonl")) else {
                continue;
            };
            sessions += 1;
            // (time, file key hash) of every shaped whole-file read, and when AAES first saw this
            // session, for MET-016.
            let mut shaped: Vec<(u64, String)> = Vec::new();
            let mut first_ts = u64::MAX;
            let mut timelines = Timelines::new(claude_root.as_deref(), e.file_name().to_str());
            for line in text.lines() {
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                first_ts = first_ts.min(v["ts"].as_u64().unwrap_or(u64::MAX));
                timelines.add_cost(&v, &mut fam, bpt);
                if v["action"] == "shaped" && family_of(v["kind"].as_str()) == "read" {
                    if let Some(k) = v["key"].as_str() {
                        shaped.push((v["ts"].as_u64().unwrap_or(0), k.to_owned()));
                    }
                }
                let kind = v["kind"].as_str().unwrap_or("?").to_owned();
                let action = v["action"].as_str().unwrap_or("?").to_owned();
                let raw = v["raw_bytes"].as_u64().unwrap_or(0);
                let ix = idx.entry(index_of(&v)).or_default();
                ix.calls += 1;
                if v["rebound"].as_bool() == Some(true) {
                    ix.follow_n += 1;
                    ix.follow_bytes += raw;
                }
                if v["action"].as_str() == Some("shaped") {
                    ix.shaped_n += 1;
                    ix.saved += raw.saturating_sub(v["delivered_bytes"].as_u64().unwrap_or(0));
                }
                let a = by.entry((kind, action)).or_default();
                a.n += 1;
                a.raw += raw;
                a.delivered += v["delivered_bytes"].as_u64().unwrap_or(0);
                if v["rebound"].as_bool() == Some(true) {
                    rebound_bytes += raw;
                    rebound_n += 1;
                }
                count_family(&v, &mut fam);
                // Events from a folder with a repo override carry its name.
                if let Some(r) = v["repo"].as_str() {
                    let m = by_repo.entry(r.to_owned()).or_default();
                    count_family(&v, m);
                    timelines.add_cost(&v, m, bpt);
                }
            }
            if let (Some(root), Some(id)) = (&claude_root, e.file_name().to_str()) {
                edits.add_session(root, id, first_ts, &shaped);
            }
        }
    }
    let saved: u64 = by
        .iter()
        .filter(|((_, a), _)| a == "shaped")
        .map(|(_, g)| g.raw.saturating_sub(g.delivered))
        .sum();
    let tok = |b: u64| (b as f64 / bpt).round() as u64;
    if json_out {
        let rows: Vec<Value> = by
            .iter()
            .map(|((k, a), g)| serde_json::json!({"kind": k, "action": a, "count": g.n, "raw_bytes": g.raw, "delivered_bytes": g.delivered}))
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "sessions": sessions, "rows": rows, "saved_bytes": saved,
                "rebound_bytes": rebound_bytes, "rebound_events": rebound_n,
                "net_saved_tokens_est": tok(saved) as i64 - tok(rebound_bytes) as i64,
                "families": fam.iter().map(|(k, f)| (k.to_string(), serde_json::json!({
                    "shaped": f.shaped_n, "saved_bytes": f.saved,
                    "follow_ups": f.follow_n, "follow_up_bytes": f.follow_bytes,
                    "net_tokens_est": f.net_tokens(bpt),
                    "follow_ups_per_shaped": f.follow_ups_per_shaped(),
                    "ok": f.ok(bpt),
                    "cost_weighted": {
                        "saved_units": f.saved_units.round(), "follow_up_units": f.follow_units.round(),
                        "net_units": f.net_units().round(), "avg_later_requests": f.avg_later(),
                        "matched_events": f.cost_n, "unmatched_events": f.cost_unmatched,
                    },
                    "shadow": {
                        "results": f.shadow_n, "would_save_bytes": f.shadow_saved,
                        "would_save_units": f.shadow_units.round(),
                    },
                }))).collect::<serde_json::Map<String, Value>>(),
                "cost_weights": aaes_transcripts::analyze::Weights::default(),
                "edits": edits.json(),
                "by_index": idx.iter().map(|(k, f)| (k.to_string(), serde_json::json!({
                    "calls": f.calls, "shaped": f.shaped_n, "saved_bytes": f.saved,
                    "follow_ups": f.follow_n, "net_tokens_est": f.net_tokens(bpt),
                }))).collect::<serde_json::Map<String, Value>>(),
                "by_repo": by_repo.iter().map(|(r, m)| (r.clone(), Value::Object(m.iter().map(|(k, f)| (k.to_string(), serde_json::json!({
                    "shaped": f.shaped_n, "saved_bytes": f.saved,
                    "follow_ups": f.follow_n, "net_tokens_est": f.net_tokens(bpt),
                    "net_units": f.net_units().round(), "matched_events": f.cost_n,
                    "shadow_results": f.shadow_n, "shadow_would_save_units": f.shadow_units.round(),
                    "advice": advice(f, bpt),
                }))).collect()))).collect::<serde_json::Map<String, Value>>(),
                "bytes_per_token": bpt
            })
        );
        return Ok(());
    }
    println!(
        "AAES report — {sessions} sessions (data: {})\n",
        rt.home.display()
    );
    println!(
        "{:<12} {:<16} {:>7} {:>12} {:>12}",
        "kind", "action", "count", "raw tok", "delivered"
    );
    for ((k, a), g) in &by {
        println!(
            "{k:<12} {a:<16} {:>7} {:>12} {:>12}",
            g.n,
            tok(g.raw),
            tok(g.delivered)
        );
    }
    println!(
        "\nSaved by shaping: ~{} tokens · follow-up reads of shaped files: ~{} tokens in {rebound_n} reads · net: ~{} tokens (estimates at {bpt} bytes/token)",
        tok(saved),
        tok(rebound_bytes),
        tok(saved) as i64 - tok(rebound_bytes) as i64
    );
    if !fam.is_empty() {
        println!(
            "\n{:<8} {:>7} {:>12} {:>11} {:>12} {:>13}  MET-015 (net > 0, <= 1 follow-up per shaped)",
            "family", "shaped", "saved tok", "follow-ups", "net tok", "per shaped"
        );
        for (k, f) in &fam {
            println!(
                "{k:<8} {:>7} {:>12} {:>11} {:>12} {:>13.2}  {}",
                f.shaped_n,
                tok(f.saved),
                f.follow_n,
                f.net_tokens(bpt),
                f.follow_ups_per_shaped(),
                if f.shaped_n == 0 {
                    "no data"
                } else if f.ok(bpt) {
                    "ok"
                } else {
                    "review: raise the threshold or disable this family"
                }
            );
        }
        print_cost(&fam);
        print_shadow(&fam, bpt);
    }
    if idx.len() > 1 || idx.contains_key("codegraph") {
        println!(
            "
{:<10} {:>7} {:>7} {:>12} {:>11} {:>12}  by repo index (agents prefer CodeGraph where indexed)",
            "repo", "calls", "shaped", "saved tok", "follow-ups", "net tok"
        );
        for (k, f) in &idx {
            println!(
                "{k:<10} {:>7} {:>7} {:>12} {:>11} {:>12}",
                f.calls,
                f.shaped_n,
                tok(f.saved),
                f.follow_n,
                f.net_tokens(bpt)
            );
        }
    }
    print_repos(&by_repo, bpt);
    edits.print();
    Ok(())
}

/// Adds the shaped and follow-up counts of one event to its family.
fn count_family(v: &Value, fam: &mut BTreeMap<&'static str, Family>) {
    let raw = v["raw_bytes"].as_u64().unwrap_or(0);
    if v["rebound"].as_bool() == Some(true) {
        let f = fam.entry(family_of(v["kind"].as_str())).or_default();
        f.follow_n += 1;
        f.follow_bytes += raw;
    }
    if v["action"].as_str() == Some("shaped") {
        let f = fam.entry(family_of(v["kind"].as_str())).or_default();
        f.shaped_n += 1;
        f.saved += raw.saturating_sub(v["delivered_bytes"].as_u64().unwrap_or(0));
    }
}

/// Fewest shaped (or shadow) results before [`advice`] judges a family in a repo.
const ADVICE_MIN: u64 = 10;

/// What to do with one family in one repo, from its measured net saving (cost-weighted when the
/// transcripts matched) or, for a family in shadow mode, from its would-be saving.
fn advice(f: &Family, bpt: f64) -> &'static str {
    if f.shaped_n >= ADVICE_MIN {
        let net_positive = if f.cost_n > 0 {
            f.net_units() > 0.0
        } else {
            f.net_tokens(bpt) > 0
        };
        if net_positive && f.follow_ups_per_shaped() <= 1.0 {
            "keep"
        } else {
            "turn off or raise the threshold"
        }
    } else if f.shaped_n == 0 && f.shadow_n >= ADVICE_MIN {
        if f.shadow_units > 0.0 || f.shadow_saved > 0 {
            "try it on (shadow saving is an upper bound)"
        } else {
            "no gain in shadow mode"
        }
    } else {
        "collecting data"
    }
}

/// Per-repo totals, for folders with a repo override in `config.json`.
fn print_repos(by_repo: &BTreeMap<String, BTreeMap<&'static str, Family>>, bpt: f64) {
    if by_repo.is_empty() {
        return;
    }
    println!(
        "\nBy repo override (config.json `repos`; cost-weighted = uncached-input-equivalent tokens)"
    );
    println!(
        "{:<8} {:>7} {:>10} {:>13} {:>11} {:>14}  {:<}",
        "family", "shaped", "net tok", "net weighted", "per shaped", "shadow weighted", "advice"
    );
    for (repo, m) in by_repo {
        println!("{repo}");
        for (k, f) in m.iter().filter(|(k, _)| **k != "other") {
            println!(
                "{k:<8} {:>7} {:>10} {:>13.0} {:>11.2} {:>14.0}  {}",
                f.shaped_n,
                f.net_tokens(bpt),
                f.net_units(),
                f.follow_ups_per_shaped(),
                f.shadow_units,
                advice(f, bpt)
            );
        }
    }
}

/// Cache-aware cost per family. Units are uncached-input-equivalent tokens: a saved token counts
/// once at the cache-write price and then at the cache-read price for every later request that
/// would have carried it.
fn print_cost(fam: &BTreeMap<&'static str, Family>) {
    if fam.values().all(|f| f.cost_n == 0) {
        return;
    }
    let w = aaes_transcripts::analyze::Weights::default();
    println!(
        "\nCache-aware cost (uncached-input-equivalent tokens; write x{} / x{}, read x{}, from transcripts)",
        w.cache_write_5m, w.cache_write_1h, w.cache_read
    );
    println!(
        "{:<8} {:>12} {:>14} {:>12} {:>14} {:>9}",
        "family", "saved", "follow-ups", "net", "later requests", "matched"
    );
    for (k, f) in fam.iter().filter(|(_, f)| f.cost_n + f.cost_unmatched > 0) {
        println!(
            "{k:<8} {:>12.0} {:>14.0} {:>12.0} {:>14} {:>9}",
            f.saved_units,
            f.follow_units,
            f.net_units(),
            f.avg_later()
                .map_or("-".to_owned(), |a| format!("{a:.1} avg")),
            format!("{}/{}", f.cost_n, f.cost_n + f.cost_unmatched),
        );
    }
}

/// Shadow-mode families: what shaping would have saved. The agent got the raw output, so there
/// are no follow-ups to subtract; the numbers are an upper bound for the real saving.
fn print_shadow(fam: &BTreeMap<&'static str, Family>, bpt: f64) {
    if fam.values().all(|f| f.shadow_n == 0) {
        return;
    }
    println!(
        "\nShadow mode (shaped but not delivered; upper bound, follow-ups not measured)\n{:<8} {:>9} {:>16} {:>18}",
        "family", "results", "would save tok", "cost-weighted"
    );
    for (k, f) in fam.iter().filter(|(_, f)| f.shadow_n > 0) {
        println!(
            "{k:<8} {:>9} {:>16} {:>18.0}",
            f.shadow_n,
            (f.shadow_saved as f64 / bpt).round(),
            f.shadow_units
        );
    }
}

/// Report bucket for the CodeGraph flag of an event.
fn index_of(v: &Value) -> &'static str {
    match v["codegraph"].as_bool() {
        Some(true) => "codegraph",
        Some(false) => "no index",
        None => "unknown",
    }
}

/// MET-016 Edit failure rate from the Claude Code transcripts of AAES sessions (edits after the
/// session's first AAES event only), split by whether
/// the file was shown shaped earlier in the session (any context). User-rejected calls are counted
/// apart: they say nothing about the agent's view of the file.
#[derive(Default)]
struct Edits {
    /// `[after a shaped read, other files]`: (calls, failures).
    split: [(u64, u64); 2],
    excluded: u64,
    classes: BTreeMap<String, u64>,
}

impl Edits {
    fn add_session(
        &mut self,
        claude_root: &std::path::Path,
        session_id: &str,
        since_ms: u64,
        shaped: &[(u64, String)],
    ) {
        use aaes_transcripts::edits;
        for f in edits::session_files(claude_root, session_id) {
            // A file last written before AAES saw the session holds no edits we count.
            let mtime_ms = std::fs::metadata(&f)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64);
            if mtime_ms.is_some_and(|m| m < since_ms) {
                continue;
            }
            let Ok(file) = std::fs::File::open(&f) else {
                continue;
            };
            for o in edits::edit_outcomes(std::io::BufReader::new(file)) {
                // Only edits made while AAES was active in this session.
                if o.ts_ms.is_none_or(|ts| ts < since_ms) {
                    continue;
                }
                let key = aaes_runtime::events::fnv_hex(&hook::read_key(&o.path, o.cwd.as_deref()));
                let after = shaped
                    .iter()
                    .any(|(t, k)| *k == key && o.ts_ms.is_some_and(|ts| *t <= ts));
                self.add(after, o.failed, o.class);
            }
        }
    }

    fn add(&mut self, after_shaped: bool, failed: bool, class: &str) {
        if class == "rejected" {
            self.excluded += 1;
            return;
        }
        let s = &mut self.split[usize::from(!after_shaped)];
        s.0 += 1;
        if failed {
            s.1 += 1;
            *self.classes.entry(class.to_owned()).or_default() += 1;
        }
    }

    fn rate(c: (u64, u64)) -> Option<f64> {
        (c.0 > 0).then(|| c.1 as f64 * 100.0 / c.0 as f64)
    }

    fn total(&self) -> (u64, u64) {
        (
            self.split[0].0 + self.split[1].0,
            self.split[0].1 + self.split[1].1,
        )
    }

    fn json(&self) -> Value {
        let part = |c: (u64, u64)| serde_json::json!({"calls": c.0, "failed": c.1, "failure_pct": Self::rate(c)});
        serde_json::json!({
            "all": part(self.total()),
            "after_shaped": part(self.split[0]),
            "other": part(self.split[1]),
            "excluded_rejected": self.excluded,
            "failure_classes": self.classes,
        })
    }

    fn print(&self) {
        if self.total().0 == 0 && self.excluded == 0 {
            return;
        }
        println!(
            "\nMET-016 Edit failures, from transcripts of these sessions (user-rejected excluded: {})",
            self.excluded
        );
        for (label, c) in [
            ("all", self.total()),
            ("after shaped read", self.split[0]),
            ("other files", self.split[1]),
        ] {
            let pct = Self::rate(c).map_or("-".to_owned(), |p| format!("{p:.1}%"));
            println!("  {label:<18} {:>5} calls {:>4} failed {pct:>7}", c.0, c.1);
        }
        if !self.classes.is_empty() {
            let list: Vec<String> = self
                .classes
                .iter()
                .map(|(k, n)| format!("{k} {n}"))
                .collect();
            println!("  failure classes: {}", list.join(", "));
        }
    }
}

/// Shaping family for MET-015: whole-file reads (Read tool or shell) vs search.
fn family_of(kind: Option<&str>) -> &'static str {
    match kind {
        Some("read" | "shell_read") => "read",
        Some("search") => "search",
        _ => "other",
    }
}

/// Per-family totals for MET-015: net saving and follow-up calls per shaped result.
#[derive(Default)]
struct Family {
    /// All hooked calls (used by the repo-index split only).
    calls: u64,
    shaped_n: u64,
    saved: u64,
    follow_n: u64,
    follow_bytes: u64,
    /// Cache-aware cost: shaped and follow-up events matched to a transcript timeline.
    cost_n: u64,
    /// Events of [`Family::cost_n`] (shaped or follow-up) that had no transcript to match.
    cost_unmatched: u64,
    /// Cost-weighted tokens saved by shaped results (see [`Timelines`]).
    saved_units: f64,
    /// Cost-weighted tokens of follow-up reads of shaped files.
    follow_units: f64,
    /// Sum of later requests over matched shaped results (for the average).
    later_sum: u64,
    later_n: u64,
    /// Shadow mode: results shaped but not delivered, the bytes they would have saved, and the
    /// same in cost-weighted tokens (transcript-matched events only).
    shadow_n: u64,
    shadow_saved: u64,
    shadow_units: f64,
}

/// Transcript timelines of one AAES session, loaded on first use, one per context (`main` or a
/// sub-agent id). They turn byte savings into cost-weighted tokens: a token removed from a tool
/// result is removed from every later prompt until compaction, written to the cache once and then
/// read (`aaes_transcripts::occupancy`).
struct Timelines {
    files: Vec<std::path::PathBuf>,
    session: String,
    by_ctx: BTreeMap<String, Option<aaes_transcripts::occupancy::Timeline>>,
    weights: aaes_transcripts::analyze::Weights,
}

impl Timelines {
    fn new(claude_root: Option<&std::path::Path>, session: Option<&str>) -> Self {
        let files = match (claude_root, session) {
            (Some(r), Some(s)) => aaes_transcripts::edits::session_files(r, s),
            _ => Vec::new(),
        };
        Self {
            files,
            session: session.unwrap_or_default().to_owned(),
            by_ctx: BTreeMap::new(),
            weights: aaes_transcripts::analyze::Weights::default(),
        }
    }

    /// The transcript file of a context: `<session>.jsonl` for `main`, `agent-<id>.jsonl` for a
    /// sub-agent.
    fn timeline(&mut self, ctx: &str) -> Option<&aaes_transcripts::occupancy::Timeline> {
        if !self.by_ctx.contains_key(ctx) {
            let want = if ctx == "main" {
                self.session.clone()
            } else {
                format!("agent-{ctx}")
            };
            let t = self
                .files
                .iter()
                .find(|f| f.file_stem().and_then(|s| s.to_str()) == Some(want.as_str()))
                .and_then(|f| std::fs::File::open(f).ok())
                .map(|f| {
                    aaes_transcripts::occupancy::Timeline::from_reader(
                        std::io::BufReader::new(f),
                        &self.weights,
                    )
                })
                .filter(|t| !t.is_empty());
            self.by_ctx.insert(ctx.to_owned(), t);
        }
        self.by_ctx.get(ctx).and_then(Option::as_ref)
    }

    /// Adds the cost-weighted saving of a shaped event, or the cost of a follow-up read.
    fn add_cost(&mut self, v: &Value, fam: &mut BTreeMap<&'static str, Family>, bpt: f64) {
        let shaped = v["action"] == "shaped";
        let rebound = v["rebound"].as_bool() == Some(true);
        let raw = v["raw_bytes"].as_u64().unwrap_or(0);
        let ts = v["ts"].as_u64().unwrap_or(0);
        if v["action"] == "shadow" {
            let would = raw.saturating_sub(v["shadow_bytes"].as_u64().unwrap_or(raw));
            let ctx = v["ctx"].as_str().unwrap_or("main").to_owned();
            let w = self.timeline(&ctx).map(|t| t.weight_at(ts));
            let f = fam.entry(family_of(v["kind"].as_str())).or_default();
            f.shadow_n += 1;
            f.shadow_saved += would;
            f.shadow_units += w.unwrap_or(0.0) * would as f64 / bpt;
            return;
        }
        if !shaped && !rebound {
            return;
        }
        let f = fam.entry(family_of(v["kind"].as_str())).or_default();
        let Some(t) = self.timeline(v["ctx"].as_str().unwrap_or("main")) else {
            f.cost_unmatched += 1;
            return;
        };
        let k = t.later_requests(ts);
        let w = t.weight_at(ts);
        f.cost_n += 1;
        if shaped {
            let saved = raw.saturating_sub(v["delivered_bytes"].as_u64().unwrap_or(0));
            f.saved_units += saved as f64 / bpt * w;
            f.later_sum += k;
            f.later_n += 1;
        }
        if rebound {
            f.follow_units += raw as f64 / bpt * w;
        }
    }
}

impl Family {
    fn net_units(&self) -> f64 {
        self.saved_units - self.follow_units
    }

    fn avg_later(&self) -> Option<f64> {
        (self.later_n > 0).then(|| self.later_sum as f64 / self.later_n as f64)
    }

    fn net_tokens(&self, bpt: f64) -> i64 {
        ((self.saved as f64 - self.follow_bytes as f64) / bpt).round() as i64
    }

    fn follow_ups_per_shaped(&self) -> f64 {
        if self.shaped_n == 0 {
            0.0
        } else {
            self.follow_n as f64 / self.shaped_n as f64
        }
    }

    /// MET-015: the family pays for itself and costs at most one extra call per shaped result.
    fn ok(&self, bpt: f64) -> bool {
        self.net_tokens(bpt) > 0 && self.follow_ups_per_shaped() <= 1.0
    }
}

/// `aaes disable` / `aaes enable`: global kill switch (all hooks pass output through unchanged).
pub fn set_enabled(enabled: bool) -> Result<()> {
    let rt = Runtime::from_env();
    let flag = rt.home.join("disabled");
    if enabled {
        let _ = std::fs::remove_file(&flag);
        eprintln!("aaes: enabled");
    } else {
        paths::create_private_dir(&rt.home)?;
        std::fs::write(&flag, "")?;
        eprintln!("aaes: disabled (tool output passes through unchanged)");
    }
    Ok(())
}

/// `aaes logs`: show the diagnostic log (newest file), optionally only warnings/errors.
pub fn logs_cmd(tail: usize, errors: bool, all_files: bool, raw: bool) -> Result<()> {
    let rt = Runtime::from_env();
    let files = log::files(&rt.home);
    if files.is_empty() {
        eprintln!("aaes: no logs yet in {}", rt.home.join("logs").display());
        return Ok(());
    }
    let chosen: Vec<_> = if all_files {
        files
    } else {
        files.into_iter().rev().take(1).collect()
    };
    let mut lines: Vec<String> = Vec::new();
    for f in chosen.iter().rev().rev() {
        if let Ok(t) = std::fs::read_to_string(f) {
            lines.extend(t.lines().map(str::to_owned));
        }
    }
    if errors {
        lines.retain(|l| l.contains("\"lvl\":\"error\"") || l.contains("\"lvl\":\"warn\""));
    }
    let skip = lines.len().saturating_sub(tail);
    for l in &lines[skip..] {
        if raw {
            println!("{l}");
            continue;
        }
        let Ok(Value::Object(mut m)) = serde_json::from_str::<Value>(l) else {
            println!("{l}");
            continue;
        };
        let take = |m: &mut serde_json::Map<String, Value>, k: &str| {
            m.remove(k)
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        };
        let (ts, lvl, comp, msg) = (
            take(&mut m, "ts"),
            take(&mut m, "lvl"),
            take(&mut m, "comp"),
            take(&mut m, "msg"),
        );
        m.remove("pid");
        println!(
            "{ts} {:<5} {comp}: {msg} {}",
            lvl.to_uppercase(),
            Value::Object(m)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that never reaches EOF, like a pipe its writer keeps open.
    struct OpenPipe;
    impl Read for OpenPipe {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_secs(3600));
            Ok(0)
        }
    }

    #[test]
    fn open_stdin_times_out_as_null() {
        let t0 = Instant::now();
        let v = read_json_from(OpenPipe, Duration::from_millis(100));
        assert!(v.is_null());
        assert!(t0.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn closed_stdin_is_parsed() {
        let src = std::io::Cursor::new("\u{feff}{\"hook_event_name\":\"SessionStart\"}");
        let v = read_json_from(src, Duration::from_secs(2));
        assert_eq!(v["hook_event_name"], "SessionStart");
    }

    #[test]
    fn edit_failure_rate_split_by_shaped() {
        let mut e = Edits::default();
        for (shaped, failed, class) in [
            (true, false, "ok"),
            (true, true, "not_found"),
            (false, false, "ok"),
            (false, false, "ok"),
            (false, true, "rejected"),
        ] {
            e.add(shaped, failed, class);
        }
        let j = e.json();
        assert_eq!(j["after_shaped"]["calls"], 2);
        assert_eq!(j["after_shaped"]["failure_pct"], 50.0);
        assert_eq!(j["other"]["failed"], 0);
        assert_eq!(j["all"]["calls"], 4);
        assert_eq!(j["excluded_rejected"], 1);
        assert_eq!(j["failure_classes"]["not_found"], 1);
    }

    #[test]
    fn repo_advice_needs_data_and_uses_net_saving() {
        let bpt = 2.4;
        let few = Family {
            shaped_n: 3,
            saved: 90_000,
            ..Default::default()
        };
        assert_eq!(advice(&few, bpt), "collecting data");
        let good = Family {
            shaped_n: 20,
            saved: 90_000,
            follow_n: 5,
            follow_bytes: 10_000,
            ..Default::default()
        };
        assert_eq!(advice(&good, bpt), "keep");
        // Byte-level net is positive, but the cache-aware net is negative: follow-ups came late
        // in long contexts.
        let costly = Family {
            cost_n: 25,
            saved_units: 1_000.0,
            follow_units: 4_000.0,
            ..good
        };
        assert_eq!(advice(&costly, bpt), "turn off or raise the threshold");
        let shadow = Family {
            shadow_n: 12,
            shadow_saved: 50_000,
            shadow_units: 900.0,
            ..Default::default()
        };
        assert_eq!(
            advice(&shadow, bpt),
            "try it on (shadow saving is an upper bound)"
        );
    }

    #[test]
    fn empty_or_invalid_stdin_is_null() {
        assert!(parse_hook_json("  \n").is_null());
        assert!(parse_hook_json("not json").is_null());
    }
}
