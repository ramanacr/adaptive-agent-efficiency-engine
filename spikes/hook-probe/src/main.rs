//! M1 spike probe (see `docs/spikes/M1-hook-spike.md`). **Not product code.**
//!
//! A Claude Code hook handler that answers four questions:
//! 1. Does `PostToolUse.updatedToolOutput` replace what the model sees for Read/Grep/Bash/PowerShell?
//! 2. Do hooks fire for tool calls inside sub-agents (`agent_id`/`agent_type`)?
//! 3. Does a shaped Read still satisfy Claude Code's read-before-Edit check?
//! 4. What does a command hook cost versus an HTTP hook?
//!
//! Modes:
//!   hook-probe cmd   --log <file> [--replace]                 # command hook: JSON on stdin/stdout
//!   hook-probe serve --log <file> [--replace] --port <n>      # HTTP hook on 127.0.0.1, bearer token
//!                                                             # from env HOOK_PROBE_TOKEN
//! With `--replace`, the main text field of the tool response (`content`/`stdout`/…, or the longest
//! string) is replaced by a marker. The rest of the response object is kept, so the replacement
//! matches the tool's output shape.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

const MARKER: &str = "AAES-SHAPED-7731";
const REPLACE_TOOLS: &[&str] = &["Read", "Grep", "Bash", "PowerShell"];
const PREFERRED_KEYS: &[&str] = &["content", "stdout", "output", "text"];

#[derive(Clone)]
struct Opts {
    log: String,
    replace: bool,
    port: u16,
    token: String,
}

fn parse_args() -> (String, Opts) {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).cloned().unwrap_or_default();
    let mut o = Opts {
        log: "hook-probe.jsonl".into(),
        replace: false,
        port: 47_731,
        token: std::env::var("HOOK_PROBE_TOKEN").unwrap_or_default(),
    };
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--log" => {
                o.log = args.get(i + 1).cloned().unwrap_or_default();
                i += 1;
            }
            "--port" => {
                o.port = args
                    .get(i + 1)
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(o.port);
                i += 1;
            }
            "--replace" => o.replace = true,
            _ => {}
        }
        i += 1;
    }
    (mode, o)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Structure of a value without its content: strings become `"str(len)"`.
fn shape(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
        Value::Array(a) => json!({ "array_len": a.len(), "first": a.first().map(shape) }),
        Value::String(s) => Value::String(format!("str({})", s.len())),
        Value::Number(_) => Value::String("num".into()),
        Value::Bool(_) => Value::String("bool".into()),
        Value::Null => Value::Null,
    }
}

#[derive(Clone, Debug)]
enum Seg {
    Key(String),
    Idx(usize),
}

/// Pick the field to replace: first string under a preferred key (depth-first), else the longest string.
fn pick(v: &Value) -> Option<(Vec<Seg>, usize)> {
    fn walk(
        v: &Value,
        path: &mut Vec<Seg>,
        pref: &mut Option<(Vec<Seg>, usize)>,
        long: &mut Option<(Vec<Seg>, usize)>,
    ) {
        match v {
            Value::Object(m) => {
                for (k, c) in m {
                    path.push(Seg::Key(k.clone()));
                    if let Value::String(s) = c {
                        if pref.is_none() && PREFERRED_KEYS.contains(&k.as_str()) {
                            *pref = Some((path.clone(), s.len()));
                        }
                    }
                    walk(c, path, pref, long);
                    path.pop();
                }
            }
            Value::Array(a) => {
                for (i, c) in a.iter().enumerate() {
                    path.push(Seg::Idx(i));
                    walk(c, path, pref, long);
                    path.pop();
                }
            }
            Value::String(s) if long.as_ref().is_none_or(|(_, l)| s.len() > *l) => {
                *long = Some((path.clone(), s.len()));
            }
            _ => {}
        }
    }
    let (mut pref, mut long) = (None, None);
    walk(v, &mut Vec::new(), &mut pref, &mut long);
    pref.or(long)
}

fn set_at(v: &mut Value, path: &[Seg], new: Value) {
    let mut cur = v;
    for seg in path {
        cur = match (seg, cur) {
            (Seg::Key(k), Value::Object(m)) => m.entry(k.clone()).or_insert(Value::Null),
            (Seg::Idx(i), Value::Array(a)) if *i < a.len() => &mut a[*i],
            _ => return,
        };
    }
    *cur = new;
}

fn path_string(path: &[Seg]) -> String {
    path.iter()
        .map(|s| match s {
            Seg::Key(k) => format!(".{k}"),
            Seg::Idx(i) => format!("[{i}]"),
        })
        .collect()
}

fn handle(input: &Value, o: &Opts, mode: &str, received_ms: u128) -> Value {
    let t0 = Instant::now();
    let event = input["hook_event_name"].as_str().unwrap_or_default();
    let tool = input["tool_name"].as_str().unwrap_or_default();
    let mut out = json!({});
    let mut replaced: Option<String> = None;

    if event == "PostToolUse" && o.replace && REPLACE_TOOLS.contains(&tool) {
        let mut resp = input["tool_response"].clone();
        if let Some((path, len)) = pick(&resp) {
            let text = format!(
                "{MARKER} tool={tool} original_chars={len}\n[outline] class Config (lines 1-20). Exact text: use Read with offset/limit."
            );
            set_at(&mut resp, &path, Value::String(text));
            replaced = Some(path_string(&path));
            out = json!({
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "updatedToolOutput": resp
                }
            });
        }
    }

    let mut rec = Map::new();
    rec.insert("ts_ms".into(), json!(received_ms));
    rec.insert("mode".into(), json!(mode));
    rec.insert("event".into(), json!(event));
    rec.insert("tool".into(), json!(tool));
    rec.insert("agent_id".into(), input["agent_id"].clone());
    rec.insert("agent_type".into(), input["agent_type"].clone());
    rec.insert(
        "session".into(),
        json!(
            input["session_id"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(8)
                .collect::<String>()
        ),
    );
    rec.insert("tool_use_id".into(), input["tool_use_id"].clone());
    rec.insert("duration_ms".into(), input["duration_ms"].clone());
    if event == "PostToolUse" {
        rec.insert("response_shape".into(), shape(&input["tool_response"]));
    }
    rec.insert("replaced".into(), json!(replaced));
    rec.insert("handler_us".into(), json!(t0.elapsed().as_micros()));
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&o.log) {
        let _ = writeln!(f, "{}", Value::Object(rec));
    }
    out
}

fn run_cmd(o: &Opts) {
    let received = now_ms();
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    let input: Value = serde_json::from_str(&buf).unwrap_or(Value::Null);
    let out = handle(&input, o, "cmd", received);
    print!("{out}");
}

fn respond(s: &mut TcpStream, status: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body);
}

fn handle_conn(mut s: TcpStream, o: &Opts) {
    let received = now_ms();
    let Ok(clone) = s.try_clone() else { return };
    let mut r = BufReader::new(clone);
    let mut line = String::new();
    let mut content_len = 0usize;
    let mut auth_ok = o.token.is_empty();
    if r.read_line(&mut line).is_err() {
        return;
    }
    loop {
        line.clear();
        if r.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
            break;
        }
        let (k, v) = line.split_once(':').unwrap_or((&line, ""));
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        if k == "content-length" {
            content_len = v.parse().unwrap_or(0);
        } else if k == "authorization" && !o.token.is_empty() {
            auth_ok = v == format!("Bearer {}", o.token);
        }
    }
    let mut body = vec![0u8; content_len];
    if r.read_exact(&mut body).is_err() {
        return;
    }
    if !auth_ok {
        respond(&mut s, "401 Unauthorized", b"{}");
        return;
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let out = handle(&input, o, "http", received);
    respond(&mut s, "200 OK", out.to_string().as_bytes());
}

fn serve(o: &Opts) {
    let listener = match TcpListener::bind(("127.0.0.1", o.port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("hook-probe: bind 127.0.0.1:{} failed: {e}", o.port);
            std::process::exit(1);
        }
    };
    eprintln!("hook-probe: listening on 127.0.0.1:{}", o.port);
    for stream in listener.incoming().flatten() {
        let o2 = o.clone();
        std::thread::spawn(move || handle_conn(stream, &o2));
    }
}

fn main() {
    let (mode, o) = parse_args();
    match mode.as_str() {
        "cmd" => run_cmd(&o),
        "serve" => serve(&o),
        _ => {
            eprintln!("usage: hook-probe cmd|serve --log <file> [--replace] [--port <n>]");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_preferred_field_and_keeps_shape() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "tool_response": {"type": "text", "file": {"filePath": "C:\\a\\long\\path\\marker.txt", "content": "short", "numLines": 1}}
        });
        let o = Opts {
            log: std::env::temp_dir()
                .join("hp-test.jsonl")
                .display()
                .to_string(),
            replace: true,
            port: 0,
            token: String::new(),
        };
        let out = handle(&input, &o, "test", 0);
        let resp = &out["hookSpecificOutput"]["updatedToolOutput"];
        assert!(
            resp["file"]["content"]
                .as_str()
                .unwrap()
                .starts_with(MARKER)
        );
        assert_eq!(resp["file"]["numLines"], 1);
        assert_eq!(resp["file"]["filePath"], "C:\\a\\long\\path\\marker.txt");
    }

    #[test]
    fn bash_stdout_is_replaced() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_response": {"stdout": "x", "stderr": "", "interrupted": false, "isImage": false}
        });
        let o = Opts {
            log: std::env::temp_dir()
                .join("hp-test.jsonl")
                .display()
                .to_string(),
            replace: true,
            port: 0,
            token: String::new(),
        };
        let out = handle(&input, &o, "test", 0);
        assert!(
            out["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
                .as_str()
                .unwrap()
                .starts_with(MARKER)
        );
    }
}
