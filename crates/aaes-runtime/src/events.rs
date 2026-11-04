//! Content-free event log per session (`events.jsonl`), the input for `aaes report`.
//! Paths are recorded only as short hashes, so rebound reads can be linked to shaped files.

use std::io::Write;
use std::path::Path;

use serde_json::{Value, json};

use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Shaped,
    PassThrough,
    /// Second whole read of the same file in the same context ("ask twice, get full").
    FullOnRepeat,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Shaped => "shaped",
            Action::PassThrough => "pass",
            Action::FullOnRepeat => "full_on_repeat",
        }
    }
}

pub struct Event<'a> {
    pub ctx: &'a str,
    pub tool: &'a str,
    pub kind: &'a str,
    pub action: Action,
    pub reason: &'a str,
    pub raw_bytes: usize,
    pub delivered_bytes: usize,
    pub key_hash: Option<String>,
    /// This read follows an earlier shaped read of the same file in the same context.
    pub rebound: bool,
}

pub fn fnv_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn append(session_dir: &Path, e: &Event) {
    let rec: Value = json!({
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        "ctx": e.ctx,
        "tool": e.tool,
        "kind": e.kind,
        "action": e.action.as_str(),
        "reason": e.reason,
        "raw_bytes": e.raw_bytes,
        "delivered_bytes": e.delivered_bytes,
        "key": e.key_hash,
        "rebound": e.rebound,
    });
    if paths::create_private_dir(session_dir).is_err() {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(session_dir.join("events.jsonl"))
    {
        let _ = writeln!(f, "{rec}");
    }
}
