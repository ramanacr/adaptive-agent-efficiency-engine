//! Claude Code transcript parser (`~/.claude/projects/<project>/<session>.jsonl`).
//!
//! Format notes (verified against Claude Code transcripts, Nov 2026):
//! - One JSON object per line. `type` is `user`, `assistant`, `system`, or one of several metadata types.
//! - An assistant API response is split across **several lines, one per content block**, each
//!   repeating the same `message.usage`. Usage is therefore de-duplicated by `message.id`
//!   (falling back to `requestId`).
//! - Tool calls are `tool_use` blocks in assistant messages. Results are `tool_result` blocks in
//!   user messages (`content` is a string or an array of `text`/`image` blocks).
//! - Large outputs are replaced by a `<persisted-output>` preview, which is what the model sees.
//! - Compaction: a `system` line with `subtype == "compact_boundary"`, or a line flagged `isCompactSummary`.
//! - Sub-agent messages are flagged `isSidechain` (older versions) or live in separate files. Sidechains
//!   are returned as a separate session because they have their own context window.

use std::collections::HashSet;
use std::io::BufRead;

use serde_json::Value;

use crate::model::{Agent, Event, Session, Usage};

const SHELL_TOOLS: &[&str] = &["Bash", "PowerShell"];
const MUTATING_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Parse one transcript file. Returns the main session and, if present, the sidechain session.
pub fn parse_reader<R: BufRead>(reader: R) -> Vec<Session> {
    let mut main = Session::new(Agent::ClaudeCode);
    let mut side = Session::new(Agent::ClaudeCode);
    let mut seen_requests: HashSet<String> = HashSet::new();

    for line in reader.lines() {
        let Ok(line) = line else {
            main.stats.bad_lines += 1;
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        main.stats.lines += 1;
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                main.stats.bad_lines += 1;
                continue;
            }
        };
        let sess = if v["isSidechain"].as_bool() == Some(true) {
            &mut side
        } else {
            &mut main
        };
        sess.note_ts(v["timestamp"].as_str());

        let ty = v["type"].as_str().unwrap_or_default();
        if (ty == "system" && v["subtype"].as_str() == Some("compact_boundary"))
            || v["isCompactSummary"].as_bool() == Some(true)
        {
            // A compact summary follows its boundary line; don't double count.
            if !matches!(sess.events.last(), Some(Event::Compaction)) {
                sess.events.push(Event::Compaction);
            }
            continue;
        }

        match ty {
            "assistant" => parse_assistant(&v, sess, &mut seen_requests),
            "user" => parse_user(&v, sess),
            _ => {}
        }
    }

    let mut out = vec![main];
    if !side.is_empty() {
        out.push(side);
    }
    out
}

fn parse_assistant(v: &Value, sess: &mut Session, seen: &mut HashSet<String>) {
    let m = &v["message"];
    if m["usage"].is_object() {
        let key = m["id"]
            .as_str()
            .or_else(|| v["requestId"].as_str())
            .map(str::to_owned);
        let first_time = match key {
            Some(k) => seen.insert(k),
            None => true,
        };
        if first_time {
            sess.events.push(Event::Request(parse_usage(&m["usage"])));
        }
    }
    if let Some(blocks) = m["content"].as_array() {
        for b in blocks {
            if b["type"].as_str() != Some("tool_use") {
                continue;
            }
            let tool = b["name"].as_str().unwrap_or("unknown").to_owned();
            let command = if SHELL_TOOLS.contains(&tool.as_str()) {
                b["input"]["command"].as_str().map(str::to_owned)
            } else {
                None
            };
            let mutates = MUTATING_TOOLS.contains(&tool.as_str());
            sess.events.push(Event::ToolCall {
                id: b["id"].as_str().unwrap_or_default().to_owned(),
                tool,
                command,
                mutates,
            });
        }
    }
}

fn parse_user(v: &Value, sess: &mut Session) {
    let Some(blocks) = v["message"]["content"].as_array() else {
        return;
    };
    for b in blocks {
        if b["type"].as_str() != Some("tool_result") {
            continue;
        }
        let (bytes, images, persisted) = measure_content(&b["content"]);
        sess.events.push(Event::ToolResult {
            id: b["tool_use_id"].as_str().unwrap_or_default().to_owned(),
            bytes,
            images,
            is_error: b["is_error"].as_bool() == Some(true),
            persisted,
        });
    }
}

/// Returns (text bytes, image count, persisted-preview flag).
fn measure_content(c: &Value) -> (u64, u32, bool) {
    match c {
        Value::String(s) => (s.len() as u64, 0, s.starts_with("<persisted-output>")),
        Value::Array(items) => {
            let mut bytes = 0u64;
            let mut images = 0u32;
            let mut persisted = false;
            for it in items {
                match it["type"].as_str() {
                    Some("text") => {
                        let t = it["text"].as_str().unwrap_or_default();
                        persisted |= t.starts_with("<persisted-output>");
                        bytes += t.len() as u64;
                    }
                    Some("image") => images += 1,
                    _ => {}
                }
            }
            (bytes, images, persisted)
        }
        _ => (0, 0, false),
    }
}

fn parse_usage(u: &Value) -> Usage {
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    let cache_creation = n("cache_creation_input_tokens");
    let (w5, w1h) = match u["cache_creation"].as_object() {
        Some(cc) => (
            cc.get("ephemeral_5m_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cc.get("ephemeral_1h_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ),
        None => (cache_creation, 0),
    };
    // Trust the split only when it adds up; otherwise treat all writes as 5-minute writes.
    let (w5, w1h) = if w5 + w1h == cache_creation {
        (w5, w1h)
    } else {
        (cache_creation, 0)
    };
    Usage {
        input: n("input_tokens"),
        cache_write_5m: w5,
        cache_write_1h: w1h,
        cache_read: n("cache_read_input_tokens"),
        output: n("output_tokens"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupes_usage_across_content_block_lines() {
        let src = r#"{"type":"assistant","requestId":"r1","message":{"id":"m1","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":50,"output_tokens":7},"content":[{"type":"text","text":"hi"}]}}
{"type":"assistant","requestId":"r1","message":{"id":"m1","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":50,"output_tokens":7},"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"git status"}}]}}
"#;
        let s = &parse_reader(src.as_bytes())[0];
        let requests = s
            .events
            .iter()
            .filter(|e| matches!(e, Event::Request(_)))
            .count();
        assert_eq!(requests, 1);
        assert!(
            matches!(&s.events[1], Event::ToolCall { command: Some(c), .. } if c == "git status")
        );
    }

    #[test]
    fn splits_cache_write_ttl_and_detects_persisted_preview() {
        let src = r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":1,"cache_creation_input_tokens":30,"cache_creation":{"ephemeral_5m_input_tokens":10,"ephemeral_1h_input_tokens":20},"cache_read_input_tokens":0,"output_tokens":1},"content":[]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"<persisted-output>\npreview"}]}}
"#;
        let s = &parse_reader(src.as_bytes())[0];
        assert_eq!(
            s.events[0],
            Event::Request(Usage {
                input: 1,
                cache_write_5m: 10,
                cache_write_1h: 20,
                cache_read: 0,
                output: 1
            })
        );
        assert!(matches!(
            s.events[1],
            Event::ToolResult {
                persisted: true,
                ..
            }
        ));
    }

    #[test]
    fn compaction_boundary_and_summary_count_once() {
        let src = r#"{"type":"system","subtype":"compact_boundary"}
{"type":"user","isCompactSummary":true,"message":{"content":"summary"}}
"#;
        let s = &parse_reader(src.as_bytes())[0];
        assert_eq!(s.events, vec![Event::Compaction]);
    }

    #[test]
    fn sidechain_is_a_separate_session() {
        let src = r#"{"type":"assistant","isSidechain":true,"message":{"id":"m9","usage":{"input_tokens":1,"output_tokens":1},"content":[]}}
"#;
        let out = parse_reader(src.as_bytes());
        assert_eq!(out.len(), 2);
        assert!(out[0].is_empty());
        assert_eq!(out[1].events.len(), 1);
    }

    #[test]
    fn bad_lines_are_counted_not_fatal() {
        let s = &parse_reader("not json\n{}\n".as_bytes())[0];
        assert_eq!(s.stats.bad_lines, 1);
        assert_eq!(s.stats.lines, 2);
    }
}
