//! Runtime commands: `hook`, `serve`, `show`, `report`, `enable`/`disable`.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aaes_runtime::{Runtime, hook, paths, server, store};
use anyhow::{Result, bail};
use serde_json::Value;

fn read_stdin_json() -> Value {
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    serde_json::from_str(&buf).unwrap_or(Value::Null)
}

/// Command-mode hook: event JSON on stdin, response JSON on stdout. Never fails the tool call.
pub fn hook_cmd() -> Result<()> {
    let rt = Runtime::from_env();
    let input = read_stdin_json();
    let out = hook::handle(&rt, &input);
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
            let _ = hook::handle(&rt, &input);
        }
        let _ = server::token(&rt.home);
        if !server::healthy(port, Duration::from_millis(300)) {
            let _ = spawn_detached(port);
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_millis(2000)
                && !server::healthy(port, Duration::from_millis(200))
            {
                std::thread::sleep(Duration::from_millis(50));
            }
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
                "net_saved_tokens_est": tok(saved).saturating_sub(tok(rebound_bytes)),
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
        "\nSaved by shaping: ~{} tokens · rebound (ranged/full re-reads of shaped files): ~{} tokens in {rebound_n} reads · net: ~{} tokens (estimates at {bpt} bytes/token)",
        tok(saved),
        tok(rebound_bytes),
        tok(saved) as i64 - tok(rebound_bytes) as i64
    );
    Ok(())
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
