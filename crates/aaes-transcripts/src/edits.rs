//! Edit/MultiEdit outcomes from Claude Code transcripts (MET-016).
//!
//! Hooks cannot measure the Edit failure rate: Claude Code 2.1.233 rejects a bad `old_string`
//! ("String to replace not found", "Found N matches") during input validation, before the tool
//! runs, and fires no `PostToolUseFailure` for it. The transcript records every call and its
//! `is_error` result, so the rate is computed from there. Only the error class is kept: the
//! message quotes the `old_string`, which is file content.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditOutcome {
    /// Call time, milliseconds since the UNIX epoch (UTC), if the transcript has a timestamp.
    pub ts_ms: Option<u64>,
    pub tool: String,
    /// `file_path` as the agent passed it, plus the working directory, to rebuild AAES file keys.
    pub path: String,
    pub cwd: Option<String>,
    pub failed: bool,
    /// `ok`, or the failure class (see [`error_class`]).
    pub class: &'static str,
}

/// Failure class of an Edit/MultiEdit tool result text.
pub fn error_class(text: &str) -> &'static str {
    let e = text.to_ascii_lowercase();
    if e.contains("string to replace not found") {
        "not_found"
    } else if e.contains("matches of the string to replace") {
        "not_unique"
    } else if e.contains("has not been read yet") {
        "not_read"
    } else if e.contains("modified since read") {
        "stale"
    } else if e.contains("are exactly the same") {
        "no_change"
    } else if e.contains("doesn't want to proceed") || e.contains("request interrupted") {
        "rejected"
    } else {
        "other"
    }
}

fn result_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.chars().take(300).collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(|it| it["text"].as_str())
            .flat_map(str::chars)
            .take(300)
            .collect(),
        _ => String::new(),
    }
}

/// Edit/MultiEdit calls in one transcript file, each paired with its first result.
pub fn edit_outcomes<R: BufRead>(reader: R) -> Vec<EditOutcome> {
    let mut pending: HashMap<String, EditOutcome> = HashMap::new();
    let mut out = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        // Transcripts run to hundreds of MB: parse only lines that can hold an Edit call or the
        // result of a pending one (Claude Code writes compact JSON).
        let call = line.contains(r#""name":"Edit""#) || line.contains(r#""name":"MultiEdit""#);
        let result = !pending.is_empty()
            && line.contains(r#""tool_result""#)
            && pending.keys().any(|id| line.contains(id.as_str()));
        if !call && !result {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(blocks) = v["message"]["content"].as_array() else {
            continue;
        };
        match v["type"].as_str() {
            Some("assistant") => {
                for b in blocks {
                    let tool = b["name"].as_str().unwrap_or_default();
                    if b["type"] != "tool_use" || !matches!(tool, "Edit" | "MultiEdit") {
                        continue;
                    }
                    let (Some(id), Some(path)) =
                        (b["id"].as_str(), b["input"]["file_path"].as_str())
                    else {
                        continue;
                    };
                    pending.insert(
                        id.to_owned(),
                        EditOutcome {
                            ts_ms: v["timestamp"].as_str().and_then(parse_iso_ms),
                            tool: tool.to_owned(),
                            path: path.to_owned(),
                            cwd: v["cwd"].as_str().map(str::to_owned),
                            failed: false,
                            class: "ok",
                        },
                    );
                }
            }
            Some("user") => {
                for b in blocks {
                    if b["type"] != "tool_result" {
                        continue;
                    }
                    let Some(mut e) = b["tool_use_id"].as_str().and_then(|id| pending.remove(id))
                    else {
                        continue;
                    };
                    if b["is_error"].as_bool() == Some(true) {
                        e.failed = true;
                        e.class = error_class(&result_text(&b["content"]));
                    }
                    out.push(e);
                }
            }
            _ => {}
        }
    }
    out
}

/// Transcript files of one Claude Code session under `root` (`~/.claude/projects`): the main
/// `<project>/<id>.jsonl` and its sub-agent files `<project>/<id>/subagents/*.jsonl`.
pub fn session_files(root: &Path, session_id: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for proj in rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let main = proj.join(format!("{session_id}.jsonl"));
        if main.is_file() {
            out.push(main);
            out.extend(crate::discover::jsonl_files(&proj.join(session_id)));
        }
    }
    out
}

/// `YYYY-MM-DDTHH:MM:SS[.fff]Z` to milliseconds since the UNIX epoch.
pub fn parse_iso_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[10] != b'T' || !s.ends_with('Z') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let ms = match s.get(19..s.len() - 1) {
        Some(f) if f.starts_with('.') && f.len() > 1 => {
            let digits: String = f[1..].chars().chain("000".chars()).take(3).collect();
            digits.parse::<i64>().ok()?
        }
        _ => 0,
    };
    // Days from civil (Howard Hinnant's algorithm).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(((days * 24 + h) * 60 + mi) * 60_000 + sec * 1000 + ms).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_timestamps() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso_ms("2026-11-05T09:48:03.412Z"),
            Some(1_793_872_083_412)
        );
        assert_eq!(parse_iso_ms("2026-11-05 09:48"), None);
    }

    #[test]
    fn pairs_calls_with_first_result_and_classifies() {
        let t = [
            r#"{"type":"assistant","timestamp":"2026-11-05T09:48:03.000Z","cwd":"C:\\repo","message":{"content":[
                {"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"src/a.ts","old_string":"SECRET","new_string":"x"}},
                {"type":"tool_use","id":"e2","name":"MultiEdit","input":{"file_path":"C:\\repo\\b.ts","edits":[]}},
                {"type":"tool_use","id":"r1","name":"Read","input":{"file_path":"c.ts"}}]}}"#,
            r#"{"type":"user","message":{"content":[
                {"type":"tool_result","tool_use_id":"e1","is_error":true,"content":"<tool_use_error>String to replace not found in file.\nString: SECRET</tool_use_error>"},
                {"type":"tool_result","tool_use_id":"e2","content":[{"type":"text","text":"ok"}]},
                {"type":"tool_result","tool_use_id":"r1","is_error":true,"content":"no"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"e1","content":"again"}]}}"#,
        ]
        .map(|l| l.replace('\n', " "))
        .join("\n");
        let got = edit_outcomes(t.as_bytes());
        assert_eq!(got.len(), 2);
        assert_eq!(
            (
                got[0].tool.as_str(),
                got[0].path.as_str(),
                got[0].failed,
                got[0].class
            ),
            ("Edit", "src/a.ts", true, "not_found")
        );
        assert_eq!(got[0].cwd.as_deref(), Some("C:\\repo"));
        assert_eq!(got[0].ts_ms, parse_iso_ms("2026-11-05T09:48:03.000Z"));
        assert_eq!((got[1].failed, got[1].class), (false, "ok"));
    }

    #[test]
    fn error_classes() {
        assert_eq!(
            error_class("Found 3 matches of the string to replace, but replace_all is false."),
            "not_unique"
        );
        assert_eq!(
            error_class("File has not been read yet. Read it first before writing to it."),
            "not_read"
        );
        assert_eq!(
            error_class("File has been modified since read, either by the user or by a linter."),
            "stale"
        );
        assert_eq!(
            error_class("The user doesn't want to proceed with this tool use."),
            "rejected"
        );
    }
}
