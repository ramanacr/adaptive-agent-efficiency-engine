//! Web-search baseline from Claude Code transcripts (MET-017, the entry gate of ADR-024).
//!
//! Format notes (verified on Claude Code 2.1.x transcripts, Nov 2026):
//! - A call is a `tool_use` block named `WebSearch` with `input.query` (plus optional
//!   `allowed_domains` / `blocked_domains`, `mode`).
//! - The result is a normal `tool_result`, not `is_error`, even when the search failed. A failure
//!   is reported inside the text as `Web search error: <code>`; an upstream rate limit is
//!   `Web search error: too_many_requests`.
//!
//! Only hashes of queries are kept in memory, and the output holds counts only (ADR-020).

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io::BufRead;

use serde::Serialize;
use serde_json::Value;

use crate::edits::parse_iso_ms;

/// A later identical search within this window could have been served by ADR-024's cache.
pub const DUP_WINDOW_MS: u64 = 30 * 60 * 1000;
/// MET-017: a burst is more than `BURST_MIN - 1` searches within this window.
pub const BURST_WINDOW_MS: u64 = 2_000;
pub const BURST_MIN: usize = 4;
/// Searches this soon after a rate-limit error count as retries.
pub const RETRY_WINDOW_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    RateLimited,
    /// `Web search error: <code>` other than a rate limit, or an `is_error` result.
    Error,
}

#[derive(Debug, Clone)]
pub struct Call {
    pub session: String,
    /// Context inside the session: `main`, or the sub-agent's `agentId`.
    pub ctx: String,
    pub ts_ms: u64,
    pub sidechain: bool,
    /// Hash of the trimmed query text plus domain filters.
    pub exact: u64,
    /// Hash of the ADR-024 fingerprint (sorted unique terms plus sorted domains).
    pub fingerprint: u64,
    pub outcome: Outcome,
    /// Error code for `Outcome::Error` (`tool_error` for an `is_error` result).
    pub error_code: Option<String>,
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// ADR-024 query fingerprint: lowercase; quoted phrases and operators (`site:x`, `-term`) stay
/// single terms; unique terms sorted; sorted domain filters appended. (The ADR also asks for NFC
/// normalisation; the baseline skips it, which can only under-count duplicates.)
pub fn fingerprint(query: &str, allowed: &[String], blocked: &[String]) -> String {
    let q = query.to_lowercase();
    let mut terms: Vec<String> = Vec::new();
    let mut chars = q.chars().peekable();
    let mut cur = String::new();
    while let Some(c) = chars.next() {
        if c == '"' {
            let phrase: String = chars.by_ref().take_while(|&c| c != '"').collect();
            let phrase = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
            if !phrase.is_empty() {
                terms.push(format!("\"{phrase}\""));
            }
        } else if c.is_whitespace() {
            if !cur.is_empty() {
                terms.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        terms.push(cur);
    }
    terms.sort();
    terms.dedup();
    let dom = |prefix: &str, d: &[String]| {
        let mut d: Vec<String> = d.iter().map(|x| x.trim().to_lowercase()).collect();
        d.sort();
        d.dedup();
        d.into_iter()
            .map(|x| format!("{prefix}{x}"))
            .collect::<Vec<_>>()
    };
    let mut out = terms;
    out.extend(dom("+site:", allowed));
    out.extend(dom("-site:", blocked));
    out.join(" ")
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn result_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|it| it["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn classify(text: &str, is_error: bool) -> (Outcome, Option<String>) {
    if let Some(p) = text.find("Web search error: ") {
        let code: String = text[p + "Web search error: ".len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if code == "too_many_requests" {
            return (Outcome::RateLimited, None);
        }
        return (Outcome::Error, Some(code));
    }
    if is_error {
        return (Outcome::Error, Some("tool_error".into()));
    }
    (Outcome::Ok, None)
}

/// `WebSearch` calls in one transcript file, each paired with its first result. Calls before
/// `since` (an ISO-8601 prefix, compared as text) are skipped.
pub fn calls<R: BufRead>(reader: R, since: Option<&str>) -> Vec<Call> {
    let mut pending: HashMap<String, Call> = HashMap::new();
    let mut out = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        // Parse only lines that can hold a call or the result of a pending one.
        let call = line.contains(r#""name":"WebSearch""#);
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
                let ts = v["timestamp"].as_str().unwrap_or_default();
                if since.is_some_and(|s| ts < s) {
                    continue;
                }
                let Some(ts_ms) = parse_iso_ms(ts) else {
                    continue;
                };
                for b in blocks {
                    if b["type"] != "tool_use" || b["name"] != "WebSearch" {
                        continue;
                    }
                    let (Some(id), Some(q)) = (b["id"].as_str(), b["input"]["query"].as_str())
                    else {
                        continue;
                    };
                    let (allowed, blocked) = (
                        strings(&b["input"]["allowed_domains"]),
                        strings(&b["input"]["blocked_domains"]),
                    );
                    let exact = format!("{}\u{1}{allowed:?}\u{1}{blocked:?}", q.trim());
                    pending.insert(
                        id.to_owned(),
                        Call {
                            session: v["sessionId"].as_str().unwrap_or_default().to_owned(),
                            ctx: v["agentId"].as_str().unwrap_or("main").to_owned(),
                            ts_ms,
                            sidechain: v["isSidechain"].as_bool() == Some(true),
                            exact: fnv(&exact),
                            fingerprint: fnv(&fingerprint(q, &allowed, &blocked)),
                            outcome: Outcome::Ok,
                            error_code: None,
                        },
                    );
                }
            }
            Some("user") => {
                for b in blocks {
                    if b["type"] != "tool_result" {
                        continue;
                    }
                    let Some(mut c) = b["tool_use_id"].as_str().and_then(|id| pending.remove(id))
                    else {
                        continue;
                    };
                    let (o, code) = classify(
                        &result_text(&b["content"]),
                        b["is_error"].as_bool() == Some(true),
                    );
                    c.outcome = o;
                    c.error_code = code;
                    out.push(c);
                }
            }
            _ => {}
        }
    }
    out
}

/// MET-017 baseline: counts only.
#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct Baseline {
    pub searches: u64,
    pub sessions: u64,
    pub in_subagents: u64,
    /// Same query text and filters as an earlier search in the session within 30 min.
    pub exact_duplicates: u64,
    /// Same fingerprint (reordered or re-spaced terms) but different text, within 30 min.
    pub reordered_duplicates: u64,
    /// Duplicates whose earlier twin started at most 10 s before (single-flight candidates).
    pub concurrent_duplicates: u64,
    /// Separate bursts of more than 3 searches within 2 s (all sessions together: the limit is
    /// per account).
    pub bursts: u64,
    pub max_in_2s: u64,
    pub rate_limit_errors: u64,
    pub rate_limit_per_100: f64,
    pub other_errors: BTreeMap<String, u64>,
    /// Searches by the same agent (main or one sub-agent) within 60 s after its rate-limit error.
    pub retries_after_rate_limit: u64,
    pub retries_same_query: u64,
    pub retries_reworded: u64,
    pub retries_rate_limited_again: u64,
}

impl Baseline {
    pub fn from_calls(mut calls: Vec<Call>) -> Self {
        let mut b = Baseline {
            searches: calls.len() as u64,
            ..Default::default()
        };
        if calls.is_empty() {
            return b;
        }
        calls.sort_by(|x, y| x.ts_ms.cmp(&y.ts_ms).then(x.session.cmp(&y.session)));

        // Bursts, across all sessions.
        let mut lo = 0;
        let mut in_burst = false;
        for hi in 0..calls.len() {
            while calls[hi].ts_ms - calls[lo].ts_ms > BURST_WINDOW_MS {
                lo += 1;
            }
            let n = hi - lo + 1;
            b.max_in_2s = b.max_in_2s.max(n as u64);
            if n >= BURST_MIN {
                if !in_burst {
                    b.bursts += 1;
                    in_burst = true;
                }
            } else {
                in_burst = false;
            }
        }

        let mut by_session: BTreeMap<&str, Vec<&Call>> = BTreeMap::new();
        for c in &calls {
            by_session.entry(c.session.as_str()).or_default().push(c);
            b.in_subagents += u64::from(c.sidechain);
            match c.outcome {
                Outcome::Ok => {}
                Outcome::RateLimited => b.rate_limit_errors += 1,
                Outcome::Error => {
                    *b.other_errors
                        .entry(c.error_code.clone().unwrap_or_default())
                        .or_default() += 1;
                }
            }
        }
        b.sessions = by_session.len() as u64;
        b.rate_limit_per_100 = b.rate_limit_errors as f64 * 100.0 / b.searches as f64;

        for list in by_session.values() {
            for (i, c) in list.iter().enumerate() {
                let earlier = list[..i]
                    .iter()
                    .rev()
                    .take_while(|e| c.ts_ms - e.ts_ms <= DUP_WINDOW_MS);
                let mut twin: Option<&Call> = None;
                let mut exact = false;
                for e in earlier {
                    if e.exact == c.exact {
                        twin = Some(e);
                        exact = true;
                        break;
                    }
                    if twin.is_none() && e.fingerprint == c.fingerprint {
                        twin = Some(e);
                    }
                }
                if let Some(t) = twin {
                    if exact {
                        b.exact_duplicates += 1;
                    } else {
                        b.reordered_duplicates += 1;
                    }
                    if c.ts_ms - t.ts_ms <= 10_000 {
                        b.concurrent_duplicates += 1;
                    }
                }
            }
        }
        // Retries: the same agent (main or one sub-agent) searching again after its rate-limit
        // error. Parallel sub-agents searching at the same time are not retries.
        let mut by_ctx: BTreeMap<(&str, &str), Vec<&Call>> = BTreeMap::new();
        for c in &calls {
            by_ctx
                .entry((c.session.as_str(), c.ctx.as_str()))
                .or_default()
                .push(c);
        }
        for list in by_ctx.values() {
            for (i, c) in list.iter().enumerate() {
                if c.outcome == Outcome::RateLimited {
                    for r in list[i + 1..]
                        .iter()
                        .take_while(|r| r.ts_ms - c.ts_ms <= RETRY_WINDOW_MS)
                    {
                        b.retries_after_rate_limit += 1;
                        if r.fingerprint == c.fingerprint {
                            b.retries_same_query += 1;
                        } else {
                            b.retries_reworded += 1;
                        }
                        if r.outcome == Outcome::RateLimited {
                            b.retries_rate_limited_again += 1;
                        }
                    }
                }
            }
        }
        b
    }

    pub fn write_markdown(&self, o: &mut String) {
        let _ = writeln!(
            o,
            "\n## Web search (MET-017 baseline, ADR-024 entry gate)\n"
        );
        if self.searches == 0 {
            let _ = writeln!(o, "No `WebSearch` calls found.");
            return;
        }
        let pct = |n: u64| format!("{:.1}%", n as f64 * 100.0 / self.searches as f64);
        let _ = writeln!(o, "| Measure | Count | Share of searches |");
        let _ = writeln!(o, "|---|---:|---:|");
        let rows: [(&str, u64); 9] = [
            ("Searches", self.searches),
            ("… in sub-agents", self.in_subagents),
            (
                "Exact duplicates (same session, ≤ 30 min)",
                self.exact_duplicates,
            ),
            (
                "Reordered duplicates (same fingerprint)",
                self.reordered_duplicates,
            ),
            (
                "… of which ≤ 10 s after the first (single-flight)",
                self.concurrent_duplicates,
            ),
            (
                "Upstream rate-limit errors (`too_many_requests`)",
                self.rate_limit_errors,
            ),
            (
                "Same agent's searches ≤ 60 s after its rate-limit error",
                self.retries_after_rate_limit,
            ),
            ("… same query", self.retries_same_query),
            ("… rate-limited again", self.retries_rate_limited_again),
        ];
        for (k, v) in rows {
            let _ = writeln!(o, "| {k} | {v} | {} |", pct(v));
        }
        let _ = writeln!(
            o,
            "\nSessions with searches: {} · bursts (> 3 searches within 2 s, all sessions): {} · most searches in any 2 s: {} · rate-limit errors per 100 searches: {:.2}",
            self.sessions, self.bursts, self.max_in_2s, self.rate_limit_per_100
        );
        if !self.other_errors.is_empty() {
            let list: Vec<String> = self
                .other_errors
                .iter()
                .map(|(k, n)| format!("`{k}` {n}"))
                .collect();
            let _ = writeln!(o, "Other search errors: {}", list.join(", "));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_ignores_order_case_and_spacing_but_not_numbers() {
        let f = |q: &str| fingerprint(q, &[], &[]);
        assert_eq!(f("Rust  serde Default"), f("default serde rust"));
        assert_eq!(f("\"exact  phrase\" rust"), f("rust \"exact phrase\""));
        assert_ne!(f("\"exact phrase\" rust"), f("exact phrase rust"));
        assert_ne!(f("rust 2024 edition"), f("rust 2021 edition"));
        assert_ne!(
            fingerprint("tokio", &["docs.rs".into()], &[]),
            fingerprint("tokio", &[], &[])
        );
        assert_eq!(f("site:docs.rs -beta tokio"), f("tokio site:docs.rs -beta"));
    }

    fn ws(id: &str, q: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"s1","timestamp":"{ts}","message":{{"content":[{{"type":"tool_use","id":"{id}","name":"WebSearch","input":{{"query":"{q}"}}}}]}}}}"#
        )
    }
    fn res(id: &str, text: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"{id}","content":"{text}"}}]}}}}"#
        )
    }

    #[test]
    fn baseline_counts_duplicates_bursts_and_rate_limit_retries() {
        let t = [
            ws("a", "tokio select macro", "2026-11-05T10:00:00.000Z"),
            res("a", "Web search results for query: x"),
            ws("b", "select macro tokio", "2026-11-05T10:00:00.500Z"),
            res("b", "Web search results for query: x"),
            ws("c", "tokio select macro", "2026-11-05T10:00:01.000Z"),
            res(
                "c",
                "Web search results for query: x\\n\\nWeb search error: too_many_requests",
            ),
            ws("d", "tokio select! docs", "2026-11-05T10:00:01.500Z"),
            res("d", "Web search error: too_many_requests"),
            ws("e", "unrelated", "2026-11-05T11:00:00.000Z"),
            res("e", "Web search error: invalid_request"),
            ws("old", "tokio select macro", "2026-10-01T10:00:00.000Z"),
            res("old", "Web search results for query: x"),
        ]
        .join("\n");
        let c = calls(t.as_bytes(), Some("2026-11-01"));
        assert_eq!(c.len(), 5, "the call before --since is skipped");
        let b = Baseline::from_calls(c);
        assert_eq!(b.searches, 5);
        assert_eq!(b.exact_duplicates, 1); // c repeats a
        assert_eq!(b.reordered_duplicates, 1); // b reorders a
        assert_eq!(b.concurrent_duplicates, 2);
        assert_eq!((b.bursts, b.max_in_2s), (1, 4));
        assert_eq!(b.rate_limit_errors, 2);
        // After c: d (reworded, limited again). After d: nothing within 60 s.
        assert_eq!(b.retries_after_rate_limit, 1);
        assert_eq!(b.retries_reworded, 1);
        assert_eq!(b.retries_rate_limited_again, 1);
        assert_eq!(b.other_errors.get("invalid_request"), Some(&1));
        let mut md = String::new();
        b.write_markdown(&mut md);
        assert!(md.contains("| Searches | 5 |"));
        assert!(!md.contains("tokio"), "no query text in the report");
    }

    #[test]
    fn another_agents_search_is_not_a_retry() {
        let agent = |a: &str, line: String| {
            line.replace(r#""sessionId""#, &format!(r#""agentId":"{a}","sessionId""#))
        };
        let t = [
            agent("x", ws("a", "q one", "2026-11-05T10:00:00.000Z")),
            res("a", "Web search error: too_many_requests"),
            agent("y", ws("b", "q two", "2026-11-05T10:00:05.000Z")),
            res("b", "Web search results for query: x"),
            agent("x", ws("c", "q one", "2026-11-05T10:00:30.000Z")),
            res("c", "Web search results for query: x"),
        ]
        .join("\n");
        let b = Baseline::from_calls(calls(t.as_bytes(), None));
        assert_eq!(b.retries_after_rate_limit, 1, "only agent x's own search");
        assert_eq!(b.retries_same_query, 1);
    }
}
