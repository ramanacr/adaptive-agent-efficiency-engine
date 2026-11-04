//! Claude Code hook protocol (FUN-014, FUN-019, FUN-025, FUN-026).
//!
//! Input: the hook event JSON. Output: hook response JSON (`{}` = no change). Every failure path
//! returns `{}`, so the agent always gets the original tool output (fail-open, FUN-007).
//!
//! Output shapes confirmed by the M1 spike (Claude Code 2.1.233):
//! - Read: `file.content`; Claude Code adds line numbers from `file.startLine`. Shaped reads
//!   therefore keep the **real** first lines in `file.content` and put the outline in
//!   `additionalContext`.
//! - Grep (content mode): `content`.
//! - Bash / PowerShell: `stdout`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::time::Instant;

use aaes_core::shape::{self, ShapeConfig};
use serde_json::{Value, json};

use crate::events::{self, Action, Event};
use crate::log;
use crate::state::State;
use crate::{Runtime, paths, store};

/// [`handle`], but a panic is caught, logged and turned into `{}` (fail-open) instead of killing
/// the server thread or the hook process.
pub fn handle_safe(rt: &Runtime, input: &Value) -> Value {
    match catch_unwind(AssertUnwindSafe(|| handle(rt, input))) {
        Ok(v) => v,
        Err(e) => {
            let msg = e
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            log::error(
                "hook",
                "panic in handler; passed output through",
                json!({
                    "event": input["hook_event_name"],
                    "tool": input["tool_name"],
                    "panic": msg,
                }),
            );
            json!({})
        }
    }
}

/// Once-per-session notice so the agent knows how shaped output works (FUN-016). ~100 tokens.
pub fn session_guidance() -> String {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "aaes".into());
    format!(
        "[aaes] Token reducer active (aaes = \"{exe}\"). Large whole-file reads show lines 1-40 plus an outline with line ranges: use Read offset/limit for exact text, or read the same file again to get all of it. Large search results are grouped by file with exact totals; capped lines: `aaes show <handle>`. If output looks wrong: `aaes logs --errors`."
    )
}

/// Keys of a JSON object (for schema-drift diagnostics; never values).
fn keys(v: &Value) -> Value {
    match v.as_object() {
        Some(m) => json!(m.keys().collect::<Vec<_>>()),
        None => json!(format!(
            "<{}>",
            match v {
                Value::Null => "null",
                Value::Bool(_) => "bool",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            }
        )),
    }
}

fn shape_warning(tool: &str, input: &Value) {
    log::warn(
        "hook",
        "unexpected tool payload shape; passed through (Claude Code schema change?)",
        json!({
            "tool": tool,
            "tool_input_keys": keys(&input["tool_input"]),
            "tool_response_keys": keys(&input["tool_response"]),
            "file_keys": keys(&input["tool_response"]["file"]),
        }),
    );
}

pub fn handle(rt: &Runtime, input: &Value) -> Value {
    if rt.disabled() {
        log::debug(
            "hook",
            "disabled; pass-through",
            json!({ "event": input["hook_event_name"] }),
        );
        return json!({});
    }
    let session = input["session_id"].as_str().unwrap_or("unknown");
    let ctx = input["agent_id"]
        .as_str()
        .map(paths::sanitize_id)
        .unwrap_or_else(|| "main".to_owned());
    let dir = paths::session_dir(&rt.home, session);

    match input["hook_event_name"].as_str().unwrap_or_default() {
        "SessionStart" => {
            let source = input["source"].as_str().unwrap_or("?");
            let resets = matches!(source, "compact" | "clear" | "resume");
            if resets {
                reset(rt, &dir, &ctx);
            }
            log::info(
                "hook",
                "session start",
                json!({ "session": log::short_hash(session), "source": source, "reset": resets, "guidance": rt.cfg.session_guidance }),
            );
            if rt.cfg.session_guidance {
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": "SessionStart",
                        "additionalContext": session_guidance(),
                    }
                })
            } else {
                json!({})
            }
        }
        "PreCompact" | "PostCompact" => {
            reset(rt, &dir, &ctx);
            log::info(
                "hook",
                "context reset",
                json!({ "session": log::short_hash(session), "ctx": ctx, "event": input["hook_event_name"] }),
            );
            json!({})
        }
        "PostToolUse" => post_tool_use(rt, input, session, &ctx, &dir),
        other => {
            log::debug("hook", "ignored event", json!({ "event": other }));
            json!({})
        }
    }
}

fn reset(rt: &Runtime, dir: &Path, ctx: &str) {
    let _g = rt.lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut st = State::load(dir);
    st.reset(ctx);
    if let Err(e) = st.save(dir) {
        log::error(
            "state",
            "saving session state failed",
            json!({ "err": e.to_string() }),
        );
    }
}

fn replace(new_response: Value, extra_context: Option<String>) -> Value {
    let mut h = json!({
        "hookEventName": "PostToolUse",
        "updatedToolOutput": new_response,
    });
    if let Some(c) = extra_context {
        h["additionalContext"] = json!(c);
    }
    json!({ "hookSpecificOutput": h })
}

/// Normalised key for "the same file" (case/separator-insensitive, relative paths resolved).
fn read_key(path: &str, cwd: Option<&str>) -> String {
    let p = path.trim_matches(['"', '\'']);
    let abs = match cwd {
        Some(c) if !Path::new(p).is_absolute() && !p.starts_with('/') && !p.starts_with('\\') => {
            format!(
                "{}/{}",
                c.trim_end_matches(['/', '\\']),
                p.trim_start_matches("./")
            )
        }
        _ => p.to_owned(),
    };
    format!("read:{}", abs.replace('\\', "/").to_lowercase())
}

/// Path as shown to the model: relative to cwd when inside it.
fn display_path<'a>(path: &'a str, cwd: Option<&str>) -> &'a str {
    if let Some(c) = cwd {
        let (np, nc) = (
            path.replace('\\', "/").to_lowercase(),
            c.replace('\\', "/").trim_end_matches('/').to_lowercase(),
        );
        if np.len() > nc.len() + 1 && np.starts_with(&nc) && np.as_bytes()[nc.len()] == b'/' {
            return &path[nc.len() + 1..];
        }
    }
    path
}

struct Ctx<'a> {
    rt: &'a Runtime,
    dir: &'a Path,
    ctx: &'a str,
    tool: &'a str,
    session: &'a str,
    t0: Instant,
}

impl Ctx<'_> {
    #[allow(clippy::too_many_arguments)]
    fn log(
        &self,
        kind: &str,
        action: Action,
        reason: &str,
        raw: usize,
        delivered: usize,
        key: Option<&str>,
        rebound: bool,
    ) {
        let dur_us = self.t0.elapsed().as_micros() as u64;
        events::append(
            self.dir,
            &Event {
                ctx: self.ctx,
                tool: self.tool,
                kind,
                action,
                reason,
                raw_bytes: raw,
                delivered_bytes: delivered,
                key_hash: key.map(events::fnv_hex),
                rebound,
                dur_us,
            },
        );
        log::info(
            "hook",
            "decision",
            json!({
                "session": log::short_hash(self.session),
                "ctx": if self.ctx == "main" { "main" } else { "subagent" },
                "tool": self.tool,
                "kind": kind,
                "action": action.as_str(),
                "reason": reason,
                "raw_bytes": raw,
                "delivered_bytes": delivered,
                "rebound": rebound,
                "dur_us": dur_us,
            }),
        );
    }

    /// Ask-twice bookkeeping for a whole-file read. Returns (previous count).
    fn bump(&self, key: &str) -> u64 {
        let _g = self.rt.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = State::load(self.dir);
        let n = st.bump_read(self.ctx, key);
        if let Err(e) = st.save(self.dir) {
            log::error(
                "state",
                "saving session state failed",
                json!({ "err": e.to_string() }),
            );
        }
        n
    }

    fn was_read(&self, key: &str) -> bool {
        let _g = self.rt.lock.lock().unwrap_or_else(|e| e.into_inner());
        State::load(self.dir).was_read(self.ctx, key)
    }
}

fn post_tool_use(rt: &Runtime, input: &Value, session: &str, ctx: &str, dir: &Path) -> Value {
    let tool = input["tool_name"].as_str().unwrap_or_default();
    let ti = &input["tool_input"];
    let resp = &input["tool_response"];
    let cwd = input["cwd"].as_str();
    let cfg: &ShapeConfig = &rt.cfg.shape;
    let c = Ctx {
        rt,
        dir,
        ctx,
        tool,
        session,
        t0: Instant::now(),
    };

    match tool {
        "Read" => {
            if resp["type"].as_str().is_some_and(|t| t != "text") {
                log::debug(
                    "hook",
                    "non-text read; pass-through",
                    json!({ "type": resp["type"] }),
                );
                return json!({});
            }
            let (Some(path), Some(content)) =
                (ti["file_path"].as_str(), resp["file"]["content"].as_str())
            else {
                shape_warning(tool, input);
                return json!({});
            };
            let key = read_key(path, cwd);
            if !ti["offset"].is_null() || !ti["limit"].is_null() {
                let rebound = c.was_read(&key);
                c.log(
                    "read",
                    Action::PassThrough,
                    "ranged",
                    content.len(),
                    content.len(),
                    Some(&key),
                    rebound,
                );
                return json!({});
            }
            if c.bump(&key) >= 1 {
                c.log(
                    "read",
                    Action::FullOnRepeat,
                    "repeat",
                    content.len(),
                    content.len(),
                    Some(&key),
                    true,
                );
                return json!({});
            }
            let Some(r) = shape::shape_read(path, content, cfg) else {
                c.log(
                    "read",
                    Action::PassThrough,
                    "small",
                    content.len(),
                    content.len(),
                    Some(&key),
                    false,
                );
                return json!({});
            };
            let total = resp["file"]["totalLines"]
                .as_u64()
                .map_or(r.total_lines, |t| t as usize);
            let shown = display_path(path, cwd);
            let context = shape::read_context(shown, &r, total);
            let mut new = resp.clone();
            new["file"]["content"] = json!(r.head);
            new["file"]["numLines"] = json!(r.head_lines);
            c.log(
                "read",
                Action::Shaped,
                "large",
                content.len(),
                r.head.len() + context.len(),
                Some(&key),
                false,
            );
            replace(new, Some(context))
        }
        "Grep" => {
            if resp["mode"].as_str() != Some("content") {
                log::debug(
                    "hook",
                    "grep not in content mode; pass-through",
                    json!({ "mode": resp["mode"] }),
                );
                return json!({});
            }
            let Some(content) = resp["content"].as_str() else {
                shape_warning(tool, input);
                return json!({});
            };
            search(&c, session, content, cwd, cfg, |text| {
                let mut new = resp.clone();
                new["numLines"] = json!(text.lines().count());
                new["content"] = json!(text);
                new
            })
        }
        "Bash" | "PowerShell" => {
            let (Some(cmd), Some(stdout)) = (ti["command"].as_str(), resp["stdout"].as_str())
            else {
                shape_warning(tool, input);
                return json!({});
            };
            if resp["interrupted"].as_bool() == Some(true)
                || resp["isImage"].as_bool() == Some(true)
                || cmd.contains("AAES_RAW=1")
            {
                return json!({});
            }
            if let Some(target) = shape::shell_read_target(cmd) {
                let key = read_key(&target, cwd);
                if c.bump(&key) >= 1 {
                    c.log(
                        "shell_read",
                        Action::FullOnRepeat,
                        "repeat",
                        stdout.len(),
                        stdout.len(),
                        Some(&key),
                        true,
                    );
                    return json!({});
                }
                let Some(r) = shape::shape_read(&target, stdout, cfg) else {
                    c.log(
                        "shell_read",
                        Action::PassThrough,
                        "small",
                        stdout.len(),
                        stdout.len(),
                        Some(&key),
                        false,
                    );
                    return json!({});
                };
                let text = shape::shell_read_text(display_path(&target, cwd), &r);
                c.log(
                    "shell_read",
                    Action::Shaped,
                    "large",
                    stdout.len(),
                    text.len(),
                    Some(&key),
                    false,
                );
                let mut new = resp.clone();
                new["stdout"] = json!(text);
                return replace(new, None);
            }
            if shape::is_shell_search(cmd) {
                return search(&c, session, stdout, cwd, cfg, |text| {
                    let mut new = resp.clone();
                    new["stdout"] = json!(text);
                    new
                });
            }
            json!({})
        }
        _ => json!({}),
    }
}

fn search(
    c: &Ctx,
    session: &str,
    raw: &str,
    cwd: Option<&str>,
    cfg: &ShapeConfig,
    build: impl FnOnce(&str) -> Value,
) -> Value {
    let handle = store::new_handle();
    let Some(s) = shape::shape_search(raw, cwd, cfg, Some(&handle)) else {
        c.log(
            "search",
            Action::PassThrough,
            "small_or_unparsed",
            raw.len(),
            raw.len(),
            None,
            false,
        );
        return json!({});
    };
    let saved = if s.omitted_lines > 0 {
        store::save(&c.rt.home, session, &handle, raw)
    } else {
        Ok(())
    };
    if let Err(e) = saved {
        log::error(
            "store",
            "saving raw search output failed; passed through",
            json!({ "err": e.to_string() }),
        );
        // Can't keep the omitted lines retrievable: don't drop them.
        c.log(
            "search",
            Action::PassThrough,
            "store_failed",
            raw.len(),
            raw.len(),
            None,
            false,
        );
        return json!({});
    }
    c.log(
        "search",
        Action::Shaped,
        if s.omitted_lines > 0 {
            "grouped_capped"
        } else {
            "grouped"
        },
        raw.len(),
        s.text.len(),
        None,
        false,
    );
    replace(build(&s.text), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> (Runtime, std::path::PathBuf) {
        let home = std::env::temp_dir().join(format!(
            "aaes-hook-{}-{}",
            std::process::id(),
            store::new_handle()
        ));
        (Runtime::new(home.clone()), home)
    }

    fn big_ts() -> String {
        let mut s = String::from("export class Big {\n");
        for i in 0..300 {
            s.push_str(&format!(
                "  method{i}(a: number): number {{\n    return a * {i} + this.offset;\n  }}\n"
            ));
        }
        s.push_str("}\n");
        s
    }

    fn read_event(
        session: &str,
        agent: Option<&str>,
        path: &str,
        content: &str,
        ranged: bool,
    ) -> Value {
        let mut ti = json!({ "file_path": path });
        if ranged {
            ti["offset"] = json!(10);
            ti["limit"] = json!(20);
        }
        let mut v = json!({
            "hook_event_name": "PostToolUse",
            "session_id": session,
            "cwd": "C:\\repo",
            "tool_name": "Read",
            "tool_input": ti,
            "tool_response": { "type": "text", "file": {
                "filePath": path, "content": content,
                "numLines": content.lines().count(), "startLine": 1, "totalLines": content.lines().count()
            }}
        });
        if let Some(a) = agent {
            v["agent_id"] = json!(a);
            v["agent_type"] = json!("general-purpose");
        }
        v
    }

    #[test]
    fn read_shaped_then_full_on_repeat_then_reset_by_compaction() {
        let (rt, home) = rt();
        let src = big_ts();
        let path = "C:\\repo\\src\\big.ts";

        let out = handle(&rt, &read_event("s1", None, path, &src, false));
        let h = &out["hookSpecificOutput"];
        let head = h["updatedToolOutput"]["file"]["content"].as_str().unwrap();
        assert!(src.starts_with(head) && !head.is_empty());
        assert_eq!(
            h["updatedToolOutput"]["file"]["numLines"],
            json!(head.lines().count())
        );
        assert_eq!(
            h["updatedToolOutput"]["file"]["totalLines"],
            json!(src.lines().count())
        );
        let ctx = h["additionalContext"].as_str().unwrap();
        assert!(ctx.contains("src\\big.ts") && ctx.contains("export class Big"));

        // ranged read: pass through
        assert_eq!(
            handle(&rt, &read_event("s1", None, path, &src, true)),
            json!({})
        );
        // second whole read: full
        assert_eq!(
            handle(&rt, &read_event("s1", None, path, &src, false)),
            json!({})
        );
        // sub-agent context is separate: shaped again
        assert!(!handle(&rt, &read_event("s1", Some("agent-x"), path, &src, false))["hookSpecificOutput"].is_null());
        // compaction resets the main context
        handle(
            &rt,
            &json!({"hook_event_name": "PostCompact", "session_id": "s1"}),
        );
        assert!(
            !handle(&rt, &read_event("s1", None, path, &src, false))["hookSpecificOutput"]
                .is_null()
        );

        let events =
            std::fs::read_to_string(paths::session_dir(&home, "s1").join("events.jsonl")).unwrap();
        assert_eq!(events.lines().count(), 5);
        assert!(events.contains("\"rebound\":true"));
        assert!(!events.contains("big.ts"), "no paths in the event log");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn small_read_and_non_text_pass_through() {
        let (rt, home) = rt();
        assert_eq!(
            handle(
                &rt,
                &read_event("s2", None, "a.ts", "const a = 1;\n", false)
            ),
            json!({})
        );
        let mut img = read_event("s2", None, "b.png", "x", false);
        img["tool_response"]["type"] = json!("image");
        assert_eq!(handle(&rt, &img), json!({}));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn grep_content_grouped_and_capped_with_retrievable_raw() {
        let (rt, home) = rt();
        let mut content = String::new();
        for f in 0..3 {
            for l in 0..40 {
                content.push_str(&format!("C:\\repo\\src\\Feature{f}\\Handler.cs:{}:    await _mediator.Send(new Command(request.Id), cancellationToken);\n", l + 1));
            }
        }
        let ev = json!({
            "hook_event_name": "PostToolUse", "session_id": "s3", "cwd": "C:\\repo",
            "tool_name": "Grep", "tool_input": {"pattern": "Send", "output_mode": "content"},
            "tool_response": {"mode": "content", "content": content, "numLines": 120, "filenames": [], "numFiles": 3}
        });
        let out = handle(&rt, &ev);
        let text = out["hookSpecificOutput"]["updatedToolOutput"]["content"]
            .as_str()
            .unwrap();
        assert!(text.starts_with("src\\Feature0\\Handler.cs (40 matches, showing first 20 lines)"));
        assert!(text.contains("120 matches in 3 files"));
        let handle_str = text
            .split("aaes show ")
            .nth(1)
            .unwrap()
            .split('`')
            .next()
            .unwrap();
        assert_eq!(
            store::load(&home, handle_str).as_deref(),
            Some(content.as_str())
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn shell_cat_shaped_and_kill_switch() {
        let (rt, home) = rt();
        let src = big_ts();
        let ev = json!({
            "hook_event_name": "PostToolUse", "session_id": "s4", "cwd": "/repo",
            "tool_name": "Bash", "tool_input": {"command": "cat src/big.ts"},
            "tool_response": {"stdout": src, "stderr": "", "interrupted": false, "isImage": false}
        });
        let out = handle(&rt, &ev);
        let stdout = out["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
            .as_str()
            .unwrap();
        assert!(stdout.starts_with("[aaes] Large file src/big.ts"));
        assert!(
            stdout.len() < src.len() / 2,
            "worst case (tiny methods): >= 50% saving"
        );
        // repeat → full
        assert_eq!(handle(&rt, &ev), json!({}));
        // kill switch
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("disabled"), "").unwrap();
        let mut ev2 = ev.clone();
        ev2["session_id"] = json!("s5");
        assert_eq!(handle(&rt, &ev2), json!({}));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn session_start_injects_guidance_unless_disabled() {
        let (rt, home) = rt();
        let ev =
            json!({"hook_event_name": "SessionStart", "session_id": "g1", "source": "startup"});
        let out = handle(&rt, &ev);
        let ctx = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.starts_with("[aaes] Token reducer active"));
        assert!(ctx.len() < 800, "short notice");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.json"), r#"{"session_guidance": false}"#).unwrap();
        let rt2 = Runtime::new(home.clone());
        assert_eq!(handle(&rt2, &ev), json!({}));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn malformed_input_fails_open() {
        let (rt, home) = rt();
        assert_eq!(
            handle(
                &rt,
                &json!({"hook_event_name": "PostToolUse", "tool_name": "Read"})
            ),
            json!({})
        );
        assert_eq!(handle(&rt, &Value::Null), json!({}));
        let _ = std::fs::remove_dir_all(&home);
    }
}
