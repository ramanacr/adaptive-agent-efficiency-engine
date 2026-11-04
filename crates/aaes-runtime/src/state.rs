//! Per-session state, keyed by context (`main` or a sub-agent id), stored as `state.json`.
//!
//! Each context has an epoch (bumped on compaction / clear / resume) and per-file whole-read
//! counters used for "ask twice, get full" (ADR-022). Sub-agents have their own context because
//! they have their own context window.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::paths;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct State {
    v: Value,
}

impl State {
    pub fn load(session_dir: &Path) -> Self {
        let v = std::fs::read_to_string(session_dir.join("state.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({ "v": 1, "contexts": {} }));
        Self { v }
    }

    pub fn save(&mut self, session_dir: &Path) -> std::io::Result<()> {
        self.v["last_seen_ms"] = json!(now_ms());
        paths::write_private(
            &session_dir.join("state.json"),
            self.v.to_string().as_bytes(),
        )
    }

    fn ctx(&mut self, ctx: &str) -> &mut Map<String, Value> {
        if !self.v["contexts"].is_object() {
            self.v["contexts"] = json!({});
        }
        let contexts = self.v["contexts"].as_object_mut().expect("object");
        contexts
            .entry(ctx.to_owned())
            .or_insert_with(|| json!({ "epoch": 0, "reads": {} }))
            .as_object_mut()
            .expect("object")
    }

    /// Increment the whole-read counter for `key`; returns the count *before* this read.
    pub fn bump_read(&mut self, ctx: &str, key: &str) -> u64 {
        let c = self.ctx(ctx);
        if !c.get("reads").is_some_and(Value::is_object) {
            c.insert("reads".into(), json!({}));
        }
        let reads = c
            .get_mut("reads")
            .and_then(Value::as_object_mut)
            .expect("object");
        let prev = reads.get(key).and_then(Value::as_u64).unwrap_or(0);
        reads.insert(key.to_owned(), json!(prev + 1));
        prev
    }

    /// Has `key` been shaped in this context epoch? (used to attribute rebound reads)
    pub fn was_read(&mut self, ctx: &str, key: &str) -> bool {
        self.ctx(ctx)
            .get("reads")
            .and_then(|r| r.get(key))
            .and_then(Value::as_u64)
            .is_some_and(|n| n > 0)
    }

    /// Remember that `key` was shaped (when, how many head lines were shown, total lines), so a
    /// follow-up read of the same file can be measured against it (diagnostics).
    pub fn mark_shaped(&mut self, ctx: &str, key: &str, head_lines: usize, total_lines: usize) {
        let c = self.ctx(ctx);
        if !c.get("shaped").is_some_and(Value::is_object) {
            c.insert("shaped".into(), json!({}));
        }
        c.get_mut("shaped")
            .and_then(Value::as_object_mut)
            .expect("object")
            .insert(
                key.to_owned(),
                json!({ "ts": now_ms(), "head": head_lines, "total": total_lines }),
            );
    }

    /// `(ms since shaped, head lines shown, total lines)` if `key` was shaped in this epoch.
    pub fn shaped_info(&mut self, ctx: &str, key: &str) -> Option<(u64, usize, usize)> {
        let s = self.ctx(ctx).get("shaped")?.get(key)?;
        let ts = s["ts"].as_u64()?;
        Some((
            now_ms().saturating_sub(ts),
            s["head"].as_u64()? as usize,
            s["total"].as_u64()? as usize,
        ))
    }

    /// New context epoch: the model no longer has earlier content (compaction, clear, resume).
    pub fn reset(&mut self, ctx: &str) {
        let c = self.ctx(ctx);
        let epoch = c.get("epoch").and_then(Value::as_u64).unwrap_or(0) + 1;
        c.insert("epoch".into(), json!(epoch));
        c.insert("reads".into(), json!({}));
        c.insert("shaped".into(), json!({}));
    }

    pub fn epoch(&mut self, ctx: &str) -> u64 {
        self.ctx(ctx)
            .get("epoch")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_twice_counter_and_reset() {
        let dir = std::env::temp_dir().join(format!("aaes-state-{}", std::process::id()));
        let mut s = State::load(&dir);
        assert_eq!(s.bump_read("main", "read:a.cs"), 0);
        assert_eq!(s.bump_read("main", "read:a.cs"), 1);
        assert_eq!(
            s.bump_read("agent-1", "read:a.cs"),
            0,
            "sub-agents have their own context"
        );
        s.save(&dir).unwrap();
        let mut s2 = State::load(&dir);
        assert!(s2.was_read("main", "read:a.cs"));
        s2.mark_shaped("main", "read:a.cs", 40, 900);
        let (ms, head, total) = s2.shaped_info("main", "read:a.cs").unwrap();
        assert!(ms < 60_000);
        assert_eq!((head, total), (40, 900));
        assert!(s2.shaped_info("agent-1", "read:a.cs").is_none());
        s2.reset("main");
        assert!(s2.shaped_info("main", "read:a.cs").is_none());
        assert_eq!(s2.epoch("main"), 1);
        assert_eq!(s2.bump_read("main", "read:a.cs"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
