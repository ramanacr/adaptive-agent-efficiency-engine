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
    agent_type: Option<&'a str>,
    tool: &'a str,
    session: &'a str,
    t0: Instant,
}

/// Lower-cased file extension (diagnostics only).
fn ext(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

impl Ctx<'_> {
    /// Record one decision: a content-free event (for `aaes report`) and a diagnostic log line.
    /// `detail` holds structural facts only (sizes, line numbers, extensions, command shapes) —
    /// never file content, paths, patterns or arguments.
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
        detail: Value,
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
        let mut rec = json!({
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
        });
        if self.ctx != "main" {
            rec["agent"] = json!(log::short_hash(self.ctx));
            if let Some(t) = self.agent_type {
                rec["agent_type"] = json!(t);
            }
        }
        if let Some(k) = key {
            rec["key"] = json!(log::short_hash(k));
        }
        if let (Some(r), Value::Object(d)) = (rec.as_object_mut(), detail) {
            r.extend(d);
        }
        log::info("hook", "decision", rec);
    }

    /// Ask-twice bookkeeping for a whole-file read. Returns (previous count).
    fn bump(&self, key: &str) -> u64 {
        self.with_state(|st| st.bump_read(self.ctx, key))
    }

    fn mark_shaped(&self, key: &str, head_lines: usize, total_lines: usize) {
        self.with_state(|st| st.mark_shaped(self.ctx, key, head_lines, total_lines));
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        let _g = self.rt.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = State::load(self.dir);
        let out = f(&mut st);
        if let Err(e) = st.save(self.dir) {
            log::error(
                "state",
                "saving session state failed",
                json!({ "err": e.to_string() }),
            );
        }
        out
    }

    /// `(ms since shaped, head lines shown, total lines)` if `key` was shaped in this context.
    fn shaped_info(&self, key: &str) -> Option<(u64, usize, usize)> {
        let _g = self.rt.lock.lock().unwrap_or_else(|e| e.into_inner());
        State::load(self.dir).shaped_info(self.ctx, key)
    }
}

/// Diagnostics for a follow-up read of a shaped file: how soon, which lines, overlap with the head.
fn rebound_detail(info: (u64, usize, usize), start: usize, num: usize) -> Value {
    let (since_ms, head, total) = info;
    let end = start + num.saturating_sub(1);
    let overlap = if num == 0 || start > head {
        0
    } else {
        end.min(head) - start + 1
    };
    json!({
        "since_ms": since_ms,
        "shaped_head_lines": head,
        "head_overlap_lines": overlap,
        "coverage_pct": (num * 100).checked_div(total).map_or(0, |p| p.min(100)),
    })
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
        agent_type: input["agent_type"].as_str(),
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
            let file = &resp["file"];
            let total_lines = file["totalLines"].as_u64().map(|t| t as usize);
            if !ti["offset"].is_null() || !ti["limit"].is_null() {
                let start = file["startLine"].as_u64().unwrap_or(1) as usize;
                let num = file["numLines"]
                    .as_u64()
                    .map_or_else(|| content.lines().count(), |n| n as usize);
                let mut d = json!({
                    "ext": ext(path),
                    "offset": ti["offset"],
                    "limit": ti["limit"],
                    "start_line": start,
                    "num_lines": num,
                    "total_lines": total_lines,
                });
                let info = c.shaped_info(&key);
                if let Some(i) = info {
                    if let (Some(dm), Value::Object(m)) =
                        (d.as_object_mut(), rebound_detail(i, start, num))
                    {
                        dm.extend(m);
                    }
                }
                c.log(
                    "read",
                    Action::PassThrough,
                    "ranged",
                    content.len(),
                    content.len(),
                    Some(&key),
                    info.is_some(),
                    d,
                );
                return json!({});
            }
            if c.bump(&key) >= 1 {
                let info = c.shaped_info(&key);
                c.log(
                    "read",
                    Action::FullOnRepeat,
                    "repeat",
                    content.len(),
                    content.len(),
                    Some(&key),
                    info.is_some(),
                    json!({ "ext": ext(path), "total_lines": total_lines, "since_ms": info.map(|i| i.0) }),
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
                    json!({ "ext": ext(path), "total_lines": total_lines, "est_tokens": cfg.est_tokens(content.len()) }),
                );
                return json!({});
            };
            let total = total_lines.unwrap_or(r.total_lines);
            let shown = display_path(path, cwd);
            let context = shape::read_context(shown, &r, total);
            let mut new = resp.clone();
            new["file"]["content"] = json!(r.head);
            new["file"]["numLines"] = json!(r.head_lines);
            c.mark_shaped(&key, r.head_lines, total);
            c.log(
                "read",
                Action::Shaped,
                "large",
                content.len(),
                r.head.len() + context.len(),
                Some(&key),
                false,
                json!({
                    "ext": ext(path),
                    "total_lines": total,
                    "head_lines": r.head_lines,
                    "head_bytes": r.head.len(),
                    "outline_items": r.outline_items,
                    "outline_omitted": r.omitted_items,
                    "outline_bytes": r.outline.len(),
                    "est_tokens": r.raw_tokens,
                }),
            );
            replace(new, Some(context))
        }
        "Grep" => {
            let mode = resp["mode"].as_str().unwrap_or("?");
            if mode != "content" {
                let size = resp["filenames"].as_array().map_or(0, |a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(|s| s.len() + 1)
                        .sum()
                });
                c.log(
                    "search",
                    Action::PassThrough,
                    "not_content_mode",
                    size,
                    size,
                    None,
                    false,
                    json!({ "mode": mode, "num_files": resp["numFiles"] }),
                );
                return json!({});
            }
            let Some(content) = resp["content"].as_str() else {
                shape_warning(tool, input);
                return json!({});
            };
            let d = json!({
                "mode": mode,
                "context": ti["-C"].is_number() || ti["-A"].is_number() || ti["-B"].is_number(),
            });
            search(&c, session, content, cwd, cfg, d, |text| {
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
            let cmd_shape = aaes_core::command::shape(cmd);
            if let Some(target) = shape::shell_read_target(cmd) {
                let key = read_key(&target, cwd);
                let base = json!({ "cmd": cmd_shape, "ext": ext(&target) });
                if c.bump(&key) >= 1 {
                    let info = c.shaped_info(&key);
                    c.log(
                        "shell_read",
                        Action::FullOnRepeat,
                        "repeat",
                        stdout.len(),
                        stdout.len(),
                        Some(&key),
                        info.is_some(),
                        base,
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
                        base,
                    );
                    return json!({});
                };
                let text = shape::shell_read_text(display_path(&target, cwd), &r);
                c.mark_shaped(&key, r.head_lines, r.total_lines);
                let mut d = base;
                d["total_lines"] = json!(r.total_lines);
                d["outline_items"] = json!(r.outline_items);
                c.log(
                    "shell_read",
                    Action::Shaped,
                    "large",
                    stdout.len(),
                    text.len(),
                    Some(&key),
                    false,
                    d,
                );
                let mut new = resp.clone();
                new["stdout"] = json!(text);
                return replace(new, None);
            }
            if shape::is_shell_search(cmd) {
                let d = json!({ "cmd": cmd_shape });
                return search(&c, session, stdout, cwd, cfg, d, |text| {
                    let mut new = resp.clone();
                    new["stdout"] = json!(text);
                    new
                });
            }
            // Not handled: record its shape and size so the next levers can be found from data.
            let stderr = resp["stderr"].as_str().map_or(0, str::len);
            c.log(
                "other",
                Action::PassThrough,
                "not_handled",
                stdout.len(),
                stdout.len(),
                None,
                false,
                json!({
                    "cmd": cmd_shape,
                    "family": aaes_core::command::family(cmd, true),
                    "stderr_bytes": stderr,
                    "lines": stdout.lines().count(),
                }),
            );
            json!({})
        }
        _ => json!({}),
    }
}

#[allow(clippy::too_many_arguments)]
fn search(
    c: &Ctx,
    session: &str,
    raw: &str,
    cwd: Option<&str>,
    cfg: &ShapeConfig,
    mut detail: Value,
    build: impl FnOnce(&str) -> Value,
) -> Value {
    let handle = store::new_handle();
    let s = match shape::shape_search(raw, cwd, cfg, Some(&handle)) {
        Ok(s) => s,
        Err(skip) => {
            detail["lines"] = json!(raw.lines().count());
            if let shape::SearchSkip::Unparsed {
                lines,
                parsed,
                forms,
            } = &skip
            {
                detail["parsed_lines"] = json!(parsed);
                detail["considered_lines"] = json!(lines);
                detail["unparsed_forms"] = Value::Object(
                    forms
                        .iter()
                        .map(|(k, n)| ((*k).to_owned(), json!(n)))
                        .collect(),
                );
            }
            c.log(
                "search",
                Action::PassThrough,
                skip.reason(),
                raw.len(),
                raw.len(),
                None,
                false,
                detail,
            );
            return json!({});
        }
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
            detail,
        );
        return json!({});
    }
    detail["matches"] = json!(s.matches);
    detail["files"] = json!(s.files);
    detail["omitted_lines"] = json!(s.omitted_lines);
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
        detail,
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
    fn rebound_counts_only_follow_ups_of_shaped_files() {
        let (rt, home) = rt();
        // small file read whole, then ranged: not a rebound (nothing was withheld)
        handle(
            &rt,
            &read_event("s5", None, "a.ts", "const a = 1;\n", false),
        );
        handle(&rt, &read_event("s5", None, "a.ts", "const a = 1;\n", true));
        let events =
            std::fs::read_to_string(paths::session_dir(&home, "s5").join("events.jsonl")).unwrap();
        assert!(!events.contains("\"rebound\":true"), "{events}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn rebound_detail_measures_overlap_and_coverage() {
        // head showed lines 1-40 of 400; follow-up read lines 30-129
        assert_eq!(
            rebound_detail((1500, 40, 400), 30, 100),
            json!({"since_ms": 1500, "shaped_head_lines": 40, "head_overlap_lines": 11, "coverage_pct": 25})
        );
        assert_eq!(
            rebound_detail((0, 40, 400), 41, 10)["head_overlap_lines"],
            0
        );
        assert_eq!(rebound_detail((0, 40, 0), 1, 10)["coverage_pct"], 0);
    }

    #[test]
    fn unparsed_search_and_unhandled_commands_are_recorded() {
        let (rt, home) = rt();
        let stdout: String = (0..400)
            .map(|i| format!("{i}:  matching line text\n"))
            .collect();
        let bash = |cmd: &str, out: &str| {
            json!({
                "hook_event_name": "PostToolUse", "session_id": "s6", "cwd": "/r",
                "tool_name": "Bash", "tool_input": { "command": cmd },
                "tool_response": { "stdout": out, "stderr": "", "interrupted": false, "isImage": false }
            })
        };
        assert_eq!(
            handle(&rt, &bash("grep -n foo src/one.rs", &stdout)),
            json!({})
        );
        assert_eq!(
            handle(&rt, &bash("cargo test --workspace", "ok\n")),
            json!({})
        );
        let events =
            std::fs::read_to_string(paths::session_dir(&home, "s6").join("events.jsonl")).unwrap();
        assert!(events.contains("\"reason\":\"unparsed\""), "{events}");
        assert!(events.contains("\"reason\":\"not_handled\""), "{events}");
        assert!(!events.contains("foo") && !events.contains("one.rs"));
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
