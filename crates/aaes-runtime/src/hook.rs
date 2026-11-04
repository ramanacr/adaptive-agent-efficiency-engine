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

use std::path::Path;

use aaes_core::shape::{self, ShapeConfig};
use serde_json::{Value, json};

use crate::events::{self, Action, Event};
use crate::state::State;
use crate::{Runtime, paths, store};

pub fn handle(rt: &Runtime, input: &Value) -> Value {
    if rt.disabled() {
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
            if matches!(
                input["source"].as_str(),
                Some("compact" | "clear" | "resume")
            ) {
                reset(rt, &dir, &ctx);
            }
            json!({})
        }
        "PreCompact" | "PostCompact" => {
            reset(rt, &dir, &ctx);
            json!({})
        }
        "PostToolUse" => post_tool_use(rt, input, session, &ctx, &dir),
        _ => json!({}),
    }
}

fn reset(rt: &Runtime, dir: &Path, ctx: &str) {
    let _g = rt.lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut st = State::load(dir);
    st.reset(ctx);
    let _ = st.save(dir);
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
            },
        );
    }

    /// Ask-twice bookkeeping for a whole-file read. Returns (previous count).
    fn bump(&self, key: &str) -> u64 {
        let _g = self.rt.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = State::load(self.dir);
        let n = st.bump_read(self.ctx, key);
        let _ = st.save(self.dir);
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
    let c = Ctx { rt, dir, ctx, tool };

    match tool {
        "Read" => {
            let (Some(path), Some(content)) =
                (ti["file_path"].as_str(), resp["file"]["content"].as_str())
            else {
                return json!({});
            };
            let key = read_key(path, cwd);
            if resp["type"].as_str() != Some("text") {
                return json!({});
            }
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
            let Some(content) = resp["content"].as_str() else {
                return json!({});
            };
            if resp["mode"].as_str() != Some("content") {
                return json!({});
            }
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
    if s.omitted_lines > 0 && store::save(&c.rt.home, session, &handle, raw).is_err() {
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
