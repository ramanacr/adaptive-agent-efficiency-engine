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
    if let Ok(rd) = std::fs::read_dir(rt.home.join("sessions")) {
        for e in rd.flatten() {
            let Ok(text) = std::fs::read_to_string(e.path().join("events.jsonl")) else {
                continue;
            };
            sessions += 1;
            for line in text.lines() {
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                let kind = v["kind"].as_str().unwrap_or("?").to_owned();
                let action = v["action"].as_str().unwrap_or("?").to_owned();
                let raw = v["raw_bytes"].as_u64().unwrap_or(0);
                let a = by.entry((kind, action)).or_default();
                a.n += 1;
                a.raw += raw;
                a.delivered += v["delivered_bytes"].as_u64().unwrap_or(0);
                if v["rebound"].as_bool() == Some(true) {
                    rebound_bytes += raw;
                    rebound_n += 1;
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
                }))).collect::<serde_json::Map<String, Value>>(),
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
    }
    Ok(())
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
    shaped_n: u64,
    saved: u64,
    follow_n: u64,
    follow_bytes: u64,
}

impl Family {
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
    fn empty_or_invalid_stdin_is_null() {
        assert!(parse_hook_json("  \n").is_null());
        assert!(parse_hook_json("not json").is_null());
    }
}
