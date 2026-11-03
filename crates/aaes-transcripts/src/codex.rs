//! Codex CLI rollout parser (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`).
//!
//! Format notes (best-effort; Codex's rollout format is not a stable public contract):
//! - One JSON object per line: `{ "timestamp", "type", "payload" }`.
//! - `type == "response_item"` carries model I/O: `payload.type` is `function_call` (with
//!   `arguments` as a JSON string containing `command`), `local_shell_call` (`action.command`),
//!   `custom_tool_call` (e.g. `apply_patch`), or their `*_output` counterparts keyed by `call_id`.
//! - `type == "event_msg"` with `payload.type == "token_count"` carries `info.total_token_usage`
//!   (cumulative) and `info.last_token_usage` (latest request). OpenAI `input_tokens` *includes*
//!   `cached_input_tokens`. Duplicate token_count events (same totals) are skipped.
//! - Compaction: a line or payload of type `compacted` (or event `context_compacted`).
//!
//! Run `aaes mine --probe` to check the structure of your local rollouts when the numbers look off.

use std::io::BufRead;

use serde_json::Value;

use crate::model::{Agent, Event, Session, Usage};

const SHELLS: &[&str] = &[
    "bash",
    "sh",
    "zsh",
    "pwsh",
    "powershell",
    "powershell.exe",
    "pwsh.exe",
    "cmd",
    "cmd.exe",
];

pub fn parse_reader<R: BufRead>(reader: R) -> Session {
    let mut s = Session::new(Agent::Codex);
    let mut prev_total: Option<Value> = None;

    for line in reader.lines() {
        let Ok(line) = line else {
            s.stats.bad_lines += 1;
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        s.stats.lines += 1;
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                s.stats.bad_lines += 1;
                continue;
            }
        };
        s.note_ts(v["timestamp"].as_str());
        let ty = v["type"].as_str().unwrap_or_default();
        let p = &v["payload"];
        let pty = p["type"].as_str().unwrap_or_default();

        if ty == "compacted" || pty == "compacted" || pty == "context_compacted" {
            if !matches!(s.events.last(), Some(Event::Compaction)) {
                s.events.push(Event::Compaction);
            }
            continue;
        }

        match (ty, pty) {
            ("event_msg", "token_count") => {
                let info = &p["info"];
                if !info.is_object() {
                    continue;
                }
                let total = &info["total_token_usage"];
                if prev_total.as_ref() == Some(total) {
                    continue; // duplicate emission
                }
                let usage = if info["last_token_usage"].is_object() {
                    openai_usage(&info["last_token_usage"])
                } else {
                    let prev = prev_total.as_ref().map(openai_usage).unwrap_or_default();
                    let cur = openai_usage(total);
                    Usage {
                        input: cur.input.saturating_sub(prev.input),
                        cache_read: cur.cache_read.saturating_sub(prev.cache_read),
                        output: cur.output.saturating_sub(prev.output),
                        ..Usage::default()
                    }
                };
                prev_total = Some(total.clone());
                s.events.push(Event::Request(usage));
            }
            ("response_item", "function_call") => {
                let name = p["name"].as_str().unwrap_or("unknown").to_owned();
                let args: Value = p["arguments"]
                    .as_str()
                    .and_then(|a| serde_json::from_str(a).ok())
                    .unwrap_or(Value::Null);
                let command = command_text(&args["command"]);
                s.events.push(Event::ToolCall {
                    id: call_id(p),
                    tool: name,
                    command,
                    mutates: false,
                    path: None,
                    range: None,
                });
            }
            ("response_item", "local_shell_call") => {
                s.events.push(Event::ToolCall {
                    id: call_id(p),
                    tool: "local_shell".to_owned(),
                    command: command_text(&p["action"]["command"]),
                    mutates: false,
                    path: None,
                    range: None,
                });
            }
            ("response_item", "custom_tool_call") => {
                let name = p["name"].as_str().unwrap_or("unknown").to_owned();
                let mutates = name == "apply_patch";
                s.events.push(Event::ToolCall {
                    id: call_id(p),
                    tool: name,
                    command: None,
                    mutates,
                    path: None,
                    range: None,
                });
            }
            ("response_item", t) if t.ends_with("_output") => {
                let out = &p["output"];
                let (text, is_error) = match out {
                    Value::String(st) => (st.clone(), exit_code_nonzero(st)),
                    Value::Null => (String::new(), false),
                    other => (other.to_string(), false),
                };
                s.events.push(Event::ToolResult {
                    id: call_id(p),
                    bytes: text.len() as u64,
                    images: 0,
                    is_error,
                    persisted: false,
                });
            }
            _ => {}
        }
    }
    s
}

fn call_id(p: &Value) -> String {
    p["call_id"]
        .as_str()
        .or_else(|| p["id"].as_str())
        .unwrap_or_default()
        .to_owned()
}

/// Codex commands are argv arrays, often `["bash", "-lc", "<script>"]`; return the script text.
fn command_text(c: &Value) -> Option<String> {
    match c {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let argv: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            if argv.len() >= 3
                && SHELLS.contains(&argv[0].to_ascii_lowercase().as_str())
                && argv[1].starts_with('-')
            {
                Some(argv[2..].join(" "))
            } else if argv.is_empty() {
                None
            } else {
                Some(argv.join(" "))
            }
        }
        _ => None,
    }
}

/// Outputs are sometimes a JSON string like `{"output":"…","metadata":{"exit_code":1}}`.
fn exit_code_nonzero(s: &str) -> bool {
    if !s.starts_with('{') {
        return false;
    }
    serde_json::from_str::<Value>(s)
        .ok()
        .and_then(|v| v["metadata"]["exit_code"].as_i64())
        .is_some_and(|c| c != 0)
}

fn openai_usage(u: &Value) -> Usage {
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    let input = n("input_tokens");
    let cached = n("cached_input_tokens").min(input);
    Usage {
        input: input - cached,
        cache_read: cached,
        output: n("output_tokens"),
        ..Usage::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_calls_outputs_usage_and_dedupes_token_counts() {
        let src = r#"{"timestamp":"2026-10-01T10:00:00Z","type":"session_meta","payload":{}}
{"timestamp":"2026-10-01T10:00:01Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"dotnet test\"]}","call_id":"c1"}}
{"timestamp":"2026-10-01T10:00:02Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"{\"output\":\"Failed: 1\",\"metadata\":{\"exit_code\":1}}"}}
{"timestamp":"2026-10-01T10:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50}}}}
{"timestamp":"2026-10-01T10:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50}}}}
{"timestamp":"2026-10-01T10:00:04Z","type":"compacted","payload":{"message":"x"}}
"#;
        let s = parse_reader(src.as_bytes());
        assert_eq!(s.first_ts.as_deref(), Some("2026-10-01T10:00:00Z"));
        assert!(
            matches!(&s.events[0], Event::ToolCall { command: Some(c), .. } if c == "dotnet test")
        );
        assert!(matches!(
            &s.events[1],
            Event::ToolResult { is_error: true, .. }
        ));
        assert_eq!(
            s.events[2],
            Event::Request(Usage {
                input: 200,
                cache_read: 800,
                output: 50,
                ..Usage::default()
            })
        );
        assert_eq!(s.events[3], Event::Compaction);
        assert_eq!(s.events.len(), 4);
    }

    #[test]
    fn apply_patch_mutates() {
        let src = r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","call_id":"c2","input":"*** Begin Patch"}}"#;
        let s = parse_reader(src.as_bytes());
        assert!(matches!(
            &s.events[0],
            Event::ToolCall { mutates: true, .. }
        ));
    }
}
