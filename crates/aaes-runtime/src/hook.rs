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
pub fn session_guidance(read_shaping: bool) -> String {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "aaes".into());
    let reads = if read_shaping {
        " Large whole-file reads show lines 1-40 plus an outline with line ranges: use Read offset/limit for exact text, or read the same file again to get all of it."
    } else {
        ""
    };
    format!(
        "[aaes] Token reducer active (aaes = \"{exe}\").{reads} Large search results are grouped by file with exact totals; capped lines: `aaes show <handle>`. If output looks wrong: `aaes logs --errors`."
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
                        "additionalContext": session_guidance(rt.cfg.read_shaping),
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
/// Events store its [`events::fnv_hex`]; `aaes report` rebuilds it from transcripts (MET-016).
pub fn read_key(path: &str, cwd: Option<&str>) -> String {
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

/// Whether `cwd` or one of its ancestors holds a CodeGraph index (`.codegraph/codegraph.db`; how
/// CodeGraph finds its project). The database file matters: `~/.codegraph` is CodeGraph's global
/// config folder, not an index. Cached per folder: the server sees the same few folders.
fn in_codegraph_repo(cwd: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(cwd) {
        return *v;
    }
    let v = Path::new(cwd)
        .ancestors()
        .any(|d| d.join(".codegraph").join("codegraph.db").is_file());
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(cwd.to_owned(), v);
    v
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
    codegraph: Option<bool>,
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
        self.log_event(
            kind, action, reason, raw, delivered, key, rebound, None, detail,
        );
    }

    /// Shadow mode: the agent gets the raw output; `would` is the size the shaped output had.
    /// Shadow events never count as shaped, so they set no follow-up state.
    fn log_shadow(&self, kind: &str, raw: usize, would: usize, key: Option<&str>, detail: Value) {
        self.log_event(
            kind,
            Action::Shadow,
            "shadow",
            raw,
            raw,
            key,
            false,
            Some(would),
            detail,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn log_event(
        &self,
        kind: &str,
        action: Action,
        reason: &str,
        raw: usize,
        delivered: usize,
        key: Option<&str>,
        rebound: bool,
        shadow_bytes: Option<usize>,
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
                codegraph: self.codegraph,
                shadow_bytes,
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
        if let Some(c) = self.codegraph {
            rec["codegraph"] = json!(c);
        }
        if let Some(b) = shadow_bytes {
            rec["shadow_bytes"] = json!(b);
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

    /// `(ms since shaped, head lines shown, total lines)` if `key` was shaped in this context
    /// within the follow-up window. A read much later is a new need, not a follow-up of the
    /// shaped view, and must not count against read shaping (MET-015).
    fn shaped_info(&self, key: &str) -> Option<(u64, usize, usize)> {
        let _g = self.rt.lock.lock().unwrap_or_else(|e| e.into_inner());
        State::load(self.dir)
            .shaped_info(self.ctx, key)
            .filter(|(ms, _, _)| *ms <= self.rt.cfg.followup_window_secs.saturating_mul(1000))
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
        codegraph: cwd.map(in_codegraph_repo),
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
            if rt.cfg.shadow("read") {
                if let Some(r) = shape::shape_read(path, content, cfg) {
                    let total = total_lines.unwrap_or(r.total_lines);
                    let context = shape::read_context(display_path(path, cwd), &r, total);
                    c.log_shadow(
                        "read",
                        content.len(),
                        r.head.len() + context.len(),
                        Some(&key),
                        json!({ "ext": ext(path), "total_lines": total, "est_tokens": r.raw_tokens }),
                    );
                    return json!({});
                }
            }
            if !rt.cfg.read_shaping {
                c.log(
                    "read",
                    Action::PassThrough,
                    "read_shaping_off",
                    content.len(),
                    content.len(),
                    Some(&key),
                    false,
                    json!({ "ext": ext(path), "total_lines": total_lines, "est_tokens": cfg.est_tokens(content.len()) }),
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
            search(
                &c,
                session,
                content,
                cwd,
                cfg,
                ti["path"].as_str(),
                d,
                |text| {
                    let mut new = resp.clone();
                    new["numLines"] = json!(text.lines().count());
                    new["content"] = json!(text);
                    new
                },
            )
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
            // Output too large for the host: Claude Code saved the full output to a file and
            // passes the hook only a preview. Totals computed from the preview would be wrong,
            // and a replacement would hide the host's pointer to the full file. Pass through.
            if resp["persistedOutputPath"]
                .as_str()
                .is_some_and(|p| !p.is_empty())
            {
                let kind = if shape::shell_read_target(cmd).is_some() {
                    "shell_read"
                } else if shape::is_shell_search(cmd) {
                    "search"
                } else {
                    "other"
                };
                c.log(
                    kind,
                    Action::PassThrough,
                    "host_truncated",
                    stdout.len(),
                    stdout.len(),
                    None,
                    false,
                    json!({ "cmd": cmd_shape }),
                );
                return json!({});
            }
            if let Some(target) = shape::shell_read_target(cmd) {
                let key = read_key(&target, cwd);
                let base = json!({ "cmd": cmd_shape, "ext": ext(&target) });
                if rt.cfg.shadow("read") {
                    if let Some(r) = shape::shape_read(&target, stdout, cfg) {
                        let text = shape::shell_read_text(display_path(&target, cwd), &r);
                        let mut d = base;
                        d["total_lines"] = json!(r.total_lines);
                        c.log_shadow("shell_read", stdout.len(), text.len(), Some(&key), d);
                        return json!({});
                    }
                }
                if !rt.cfg.read_shaping {
                    c.log(
                        "shell_read",
                        Action::PassThrough,
                        "read_shaping_off",
                        stdout.len(),
                        stdout.len(),
                        Some(&key),
                        false,
                        base,
                    );
                    return json!({});
                }
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
                return search(&c, session, stdout, cwd, cfg, None, d, |text| {
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
    single_path: Option<&str>,
    mut detail: Value,
    build: impl FnOnce(&str) -> Value,
) -> Value {
    let handle = store::new_handle();
    let single_path = single_path.map(|p| shape::display_rel(p, cwd));
    let s = match shape::shape_search(raw, cwd, cfg, Some(&handle), single_path) {
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
    if c.rt.cfg.shadow("search") {
        // Nothing is stored: the agent has every line, so the handle in the shaped text is unused.
        detail["matches"] = json!(s.matches);
        detail["files"] = json!(s.files);
        detail["omitted_lines"] = json!(s.omitted_lines);
        c.log_shadow("search", raw.len(), s.text.len(), None, detail);
        return json!({});
    }
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
        let mut r = Runtime::new(home.clone());
        // Most tests cover the read-shaping path; `read_shaping_is_off_by_default` covers the default.
        r.cfg.read_shaping = true;
        (r, home)
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
    fn read_shaping_is_off_by_default() {
        let home = std::env::temp_dir().join(format!(
            "aaes-hook-{}-{}",
            std::process::id(),
            store::new_handle()
        ));
        let rt = Runtime::new(home.clone());
        assert!(!rt.cfg.read_shaping);
        let src = big_ts();
        // Read tool and shell read both pass through; search shaping is unaffected.
        assert_eq!(
            handle(
                &rt,
                &read_event("s1", None, "C:\\repo\\src\\big.ts", &src, false)
            ),
            json!({})
        );
        let shell = json!({
            "hook_event_name": "PostToolUse", "session_id": "s1", "cwd": "C:\\repo",
            "tool_name": "Bash", "tool_input": { "command": "cat src/big.ts" },
            "tool_response": { "stdout": src, "stderr": "", "interrupted": false }
        });
        assert_eq!(handle(&rt, &shell), json!({}));
        assert!(!session_guidance(false).contains("whole-file"));
        assert!(session_guidance(true).contains("whole-file"));

        // config.json can turn it back on.
        std::fs::write(home.join("config.json"), r#"{"read_shaping": true}"#).unwrap();
        let rt2 = Runtime::new(home.clone());
        assert!(rt2.cfg.read_shaping);
        let out = handle(
            &rt2,
            &read_event("s2", None, "C:\\repo\\src\\big.ts", &src, false),
        );
        assert!(out["hookSpecificOutput"]["updatedToolOutput"].is_object());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn shadow_mode_logs_the_shaped_size_but_delivers_raw() {
        let home = std::env::temp_dir().join(format!(
            "aaes-hook-{}-{}",
            std::process::id(),
            store::new_handle()
        ));
        std::fs::create_dir_all(&home).unwrap();
        // Unknown families are ignored; shadow wins over read_shaping.
        std::fs::write(
            home.join("config.json"),
            r#"{"read_shaping": true, "shadow": ["read", "search", "build"]}"#,
        )
        .unwrap();
        let rt = Runtime::new(home.clone());
        assert_eq!(rt.cfg.shadow, ["read", "search"]);
        let src = big_ts();
        let path = "C:\\repo\\src\\big.ts";
        for _ in 0..2 {
            assert_eq!(
                handle(&rt, &read_event("sh", None, path, &src, false)),
                json!({})
            );
        }
        let shell = json!({
            "hook_event_name": "PostToolUse", "session_id": "sh", "cwd": "C:\\repo",
            "tool_name": "Bash", "tool_input": { "command": "cat src/big.ts" },
            "tool_response": { "stdout": src, "stderr": "", "interrupted": false }
        });
        assert_eq!(handle(&rt, &shell), json!({}));
        let mut content = String::new();
        for l in 0..120 {
            content.push_str(&format!(
                "C:\\repo\\src\\F{}\\H.cs:{}:    await _mediator.Send(new Command(request.Id), ct);\n",
                l % 3,
                l + 1
            ));
        }
        let grep = json!({
            "hook_event_name": "PostToolUse", "session_id": "sh", "cwd": "C:\\repo",
            "tool_name": "Grep", "tool_input": {"pattern": "Send", "output_mode": "content"},
            "tool_response": {"mode": "content", "content": content, "numLines": 120, "filenames": [], "numFiles": 3}
        });
        assert_eq!(handle(&rt, &grep), json!({}));

        let log =
            std::fs::read_to_string(paths::session_dir(&home, "sh").join("events.jsonl")).unwrap();
        let events: Vec<Value> = log
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(events.len(), 4);
        for e in &events {
            assert_eq!(e["action"], "shadow", "{e}");
            assert_eq!(e["raw_bytes"], e["delivered_bytes"], "agent got raw: {e}");
            assert!(e["shadow_bytes"].as_u64().unwrap() < e["raw_bytes"].as_u64().unwrap());
            assert_eq!(e["rebound"], false, "shadow sets no follow-up state: {e}");
        }
        let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["read", "read", "shell_read", "search"]);
        // Nothing was stored: the agent saw every line.
        assert!(
            !paths::session_dir(&home, "sh").join("artifacts").exists()
                || std::fs::read_dir(paths::session_dir(&home, "sh").join("artifacts"))
                    .unwrap()
                    .next()
                    .is_none()
        );
        let _ = std::fs::remove_dir_all(home);
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
        // Neither `path:line:` nor `line:` form (e.g. `grep -c` style counts with labels).
        let stdout: String = (0..400)
            .map(|i| format!("matching line text number {i}\n"))
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

    /// SEC-010/SEC-013 canary: a secret in tool output, a search pattern or a command never reaches
    /// the log, the event log or any other file in the home (artifacts are encrypted), nor the
    /// text AAES delivers in place of an omitted region.
    #[test]
    fn canary_secret_never_in_telemetry_or_footers() {
        const CANARY: &str = "AKIA_CANARY_51d0c3e8";
        let (rt, home) = rt();
        let mut src = big_ts();
        src.push_str(&format!("const awsKey = \"{CANARY}\";\n"));
        let read = handle(
            &rt,
            &read_event("sc", None, "C:\\repo\\src\\big.ts", &src, false),
        );
        let delivered = read["hookSpecificOutput"]["updatedToolOutput"]["file"]["content"]
            .as_str()
            .expect("read shaped");
        assert!(!delivered.contains(CANARY), "omitted region leaked");

        let mut content = String::new();
        for l in 0..200 {
            content.push_str(&format!(
                "C:\\repo\\src\\A.cs:{}: var k = \"{CANARY}\";\n",
                l + 1
            ));
        }
        let search = handle(
            &rt,
            &json!({
                "hook_event_name": "PostToolUse", "session_id": "sc", "cwd": "C:\\repo",
                "tool_name": "Grep", "tool_input": {"pattern": CANARY, "output_mode": "content"},
                "tool_response": {"mode": "content", "content": content, "numLines": 200, "filenames": [], "numFiles": 1}
            }),
        );
        assert!(search["hookSpecificOutput"].is_object(), "search shaped");
        handle(
            &rt,
            &json!({
                "hook_event_name": "PostToolUse", "session_id": "sc", "cwd": "/repo",
                "tool_name": "Bash", "tool_input": {"command": format!("deploy-tool --token {CANARY}")},
                "tool_response": {"stdout": "ok", "stderr": "", "interrupted": false, "isImage": false}
            }),
        );

        let mut roots = vec![home.clone()];
        roots.extend(log::dir());
        let mut scanned = 0;
        while let Some(p) = roots.pop() {
            let Ok(rd) = std::fs::read_dir(&p) else {
                continue;
            };
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    roots.push(path);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    scanned += 1;
                    assert!(
                        !bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()),
                        "canary in {}",
                        path.display()
                    );
                }
            }
        }
        assert!(scanned > 0);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn follow_up_counts_only_within_the_window() {
        let rebound_of_last = |home: &std::path::Path, session: &str| {
            let text =
                std::fs::read_to_string(paths::session_dir(home, session).join("events.jsonl"))
                    .unwrap();
            let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
            last["rebound"].as_bool().unwrap()
        };
        let src = big_ts();
        let path = "C:\\repo\\src\\big.ts";
        // Default window (5 min): a ranged read right after the shaped read is a follow-up.
        let (rt1, home) = rt();
        handle(&rt1, &read_event("w1", None, path, &src, false));
        handle(&rt1, &read_event("w1", None, path, &src, true));
        assert!(rebound_of_last(&home, "w1"));
        let _ = std::fs::remove_dir_all(&home);
        // Window exceeded: the same ranged read is a new need, not a follow-up.
        let (mut rt2, home) = rt();
        rt2.cfg.followup_window_secs = 0;
        handle(&rt2, &read_event("w2", None, path, &src, false));
        std::thread::sleep(std::time::Duration::from_millis(20));
        handle(&rt2, &read_event("w2", None, path, &src, true));
        assert!(!rebound_of_last(&home, "w2"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn events_flag_codegraph_indexed_repos() {
        let (rt, home) = rt();
        let repo = home.join("repo");
        std::fs::create_dir_all(repo.join(".codegraph")).unwrap();
        std::fs::write(repo.join(".codegraph").join("codegraph.db"), b"").unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        // A `.codegraph` folder without a database (like `~/.codegraph`) is not an index.
        let plain = home.join("plain");
        std::fs::create_dir_all(plain.join(".codegraph")).unwrap();
        for cwd in [repo.join("src"), plain] {
            let mut ev = read_event(
                "cg", None, "a.ts", "x
", false,
            );
            ev["cwd"] = json!(cwd.to_string_lossy());
            handle(&rt, &ev);
        }
        let text =
            std::fs::read_to_string(paths::session_dir(&home, "cg").join("events.jsonl")).unwrap();
        let flags: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["codegraph"].clone())
            .collect();
        assert_eq!(flags, vec![json!(true), json!(false)]);
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

    /// M1 exit criterion: two concurrent sessions stay isolated. Each session reads the same files
    /// at the same time, through one shared runtime (resident `aaes serve`) and through separate
    /// runtimes on the same data folder (one process per hook call). Read state, stored raw
    /// output and event logs must never mix between the sessions.
    #[test]
    fn concurrent_sessions_are_isolated() {
        const FILES: usize = 8;
        let (shared, home) = rt();
        let mut sep_a = Runtime::new(home.clone());
        sep_a.cfg.read_shaping = true;
        let mut sep_b = Runtime::new(home.clone());
        sep_b.cfg.read_shaping = true;
        let src = big_ts();

        let run = |rt: &Runtime, session: &str, barrier: &std::sync::Barrier| -> String {
            barrier.wait();
            for i in 0..FILES {
                let path = format!("C:\\repo\\src\\f{i}.ts");
                let first = handle(rt, &read_event(session, None, &path, &src, false));
                assert!(
                    first["hookSpecificOutput"]["updatedToolOutput"].is_object(),
                    "{session}: first whole read of f{i} must be shaped"
                );
                let second = handle(rt, &read_event(session, None, &path, &src, false));
                assert_eq!(
                    second,
                    json!({}),
                    "{session}: second read of f{i} must be full"
                );
            }
            let mut content = String::new();
            for l in 0..60 {
                content.push_str(&format!(
                    "C:\\repo\\src\\Only{session}.cs:{}:    var marker = \"{session}\"; // padding text for size\n",
                    l + 1
                ));
            }
            let out = handle(
                rt,
                &json!({
                    "hook_event_name": "PostToolUse", "session_id": session, "cwd": "C:\\repo",
                    "tool_name": "Grep", "tool_input": {"pattern": "marker", "output_mode": "content"},
                    "tool_response": {"mode": "content", "content": content, "numLines": 60, "filenames": [], "numFiles": 1}
                }),
            );
            let text = out["hookSpecificOutput"]["updatedToolOutput"]["content"]
                .as_str()
                .expect("grep shaped")
                .to_string();
            let h = text
                .split("aaes show ")
                .nth(1)
                .and_then(|t| t.split('`').next())
                .expect("handle in footer")
                .to_string();
            assert_eq!(store::load(&home, &h).as_deref(), Some(content.as_str()));
            h
        };

        for (label, a, b) in [
            ("shared runtime", &shared, &shared),
            ("separate runtimes", &sep_a, &sep_b),
        ] {
            let (sa, sb) = (
                format!("{label}-a").replace(' ', "_"),
                format!("{label}-b").replace(' ', "_"),
            );
            let barrier = std::sync::Barrier::new(2);
            let (ha, hb) = std::thread::scope(|s| {
                let ta = s.spawn(|| run(a, &sa, &barrier));
                let tb = s.spawn(|| run(b, &sb, &barrier));
                (ta.join().unwrap(), tb.join().unwrap())
            });
            assert_ne!(ha, hb, "{label}: each session gets its own handle");
            for (session, other) in [(&sa, &sb), (&sb, &sa)] {
                let events = std::fs::read_to_string(
                    paths::session_dir(&home, session).join("events.jsonl"),
                )
                .unwrap();
                assert_eq!(
                    events.lines().count(),
                    2 * FILES + 1,
                    "{label}: {session} has exactly its own events"
                );
                assert!(
                    !events.contains(other.as_str()),
                    "{label}: no events of {other}"
                );
            }
            // Each session's raw output sits in its own artifacts folder.
            for (session, h) in [(&sa, &ha), (&sb, &hb)] {
                let art = paths::session_dir(&home, session)
                    .join("artifacts")
                    .join(format!("{h}.bin"));
                assert!(art.exists(), "{label}: {h} stored under {session}");
            }
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Claude Code keeps output that is too large in a file and passes the hook only a preview
    /// (`persistedOutputPath`). Shaping the preview would report wrong totals: pass through.
    #[test]
    fn host_truncated_shell_output_passes_through() {
        let (rt, home) = rt();
        let mut out = String::new();
        for l in 0..400 {
            out.push_str(&format!(
                "src/Feature{}/Handler.cs:{}:    await _mediator.Send(new Command(request.Id), token);\n",
                l % 8,
                l + 1
            ));
        }
        let mut ev = json!({
            "hook_event_name": "PostToolUse", "session_id": "trunc", "cwd": "C:\\repo",
            "tool_name": "Bash", "tool_input": { "command": "grep -rn 'Send(' src ." },
            "tool_response": { "stdout": out, "stderr": "", "interrupted": false, "isImage": false,
                "persistedOutputPath": "C:\\Users\\x\\.claude\\tool-results\\b1.txt", "persistedOutputSize": 61000 }
        });
        assert_eq!(handle(&rt, &ev), json!({}));
        let events =
            std::fs::read_to_string(paths::session_dir(&home, "trunc").join("events.jsonl"))
                .unwrap();
        assert!(events.contains("host_truncated"), "{events}");
        // Without the marker the same output is shaped.
        ev["tool_response"]
            .as_object_mut()
            .unwrap()
            .remove("persistedOutputPath");
        assert!(handle(&rt, &ev)["hookSpecificOutput"]["updatedToolOutput"].is_object());
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
