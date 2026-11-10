//! Request timeline of one Claude Code context, for cache-aware cost of single tool results.
//!
//! A tool result that enters the context at time `t` is part of the prompt of every later model
//! request until the next compaction (or the end of the session). With prompt caching, the first
//! of those requests writes it to the cache and every later one reads it, so `T` tokens cost
//! `T · (w_first + (k − 1) · w_read)` cost-weighted units, where `k` is the number of those requests
//! (same model as [`crate::analyze`]). This module finds `k` from timestamps only, so callers that
//! know when a result was produced (the AAES hook event log) need no tool-call ids.

use std::collections::HashSet;
use std::io::BufRead;

use serde_json::Value;

use crate::analyze::Weights;
use crate::edits::parse_iso_ms;

/// Model requests and compaction boundaries of one transcript file, in file order.
#[derive(Debug, Default, Clone)]
pub struct Timeline {
    /// Timestamps (ms) of distinct model requests, sorted.
    requests: Vec<u64>,
    /// Timestamps (ms) of compaction boundaries, sorted.
    compactions: Vec<u64>,
    /// Observed cost weight of a cache write in this context (5-minute and 1-hour mix).
    pub w_first: f64,
    /// Cost weight of a cache read.
    pub w_read: f64,
}

impl Timeline {
    /// Parses one transcript file. Lines that cannot be a request or a compaction are skipped
    /// before JSON parsing, because transcripts can be hundreds of MB.
    pub fn from_reader<R: BufRead>(reader: R, w: &Weights) -> Self {
        let mut t = Timeline {
            w_read: w.cache_read,
            ..Default::default()
        };
        let mut seen: HashSet<String> = HashSet::new();
        let (mut w5, mut w1h) = (0u64, 0u64);
        for line in reader.lines().map_while(Result::ok) {
            let compact = line.contains("compact_boundary") || line.contains("isCompactSummary");
            if !compact && !line.contains(r#""usage""#) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(ts) = v["timestamp"].as_str().and_then(parse_iso_ms) else {
                continue;
            };
            let is_boundary = (v["type"] == "system" && v["subtype"] == "compact_boundary")
                || v["isCompactSummary"].as_bool() == Some(true);
            if is_boundary {
                // A compact summary follows its boundary line; one boundary is enough.
                if t.compactions
                    .last()
                    .is_none_or(|&c| ts.saturating_sub(c) > 60_000)
                {
                    t.compactions.push(ts);
                }
                continue;
            }
            let m = &v["message"];
            if v["type"] != "assistant" || !m["usage"].is_object() {
                continue;
            }
            // One API response is split over several lines that repeat the same usage.
            let key = m["id"].as_str().or_else(|| v["requestId"].as_str());
            if key.is_some_and(|k| !seen.insert(k.to_owned())) {
                continue;
            }
            let u = &m["usage"];
            let n = |k: &str| u[k].as_u64().unwrap_or(0);
            let cc = n("cache_creation_input_tokens");
            let (a, b) = (
                u["cache_creation"]["ephemeral_5m_input_tokens"].as_u64(),
                u["cache_creation"]["ephemeral_1h_input_tokens"].as_u64(),
            );
            match (a, b) {
                (Some(a), Some(b)) if a + b == cc => {
                    w5 += a;
                    w1h += b;
                }
                _ => w5 += cc,
            }
            t.requests.push(ts);
        }
        t.requests.sort_unstable();
        t.compactions.sort_unstable();
        t.w_first = if w5 + w1h > 0 {
            (w5 as f64 * w.cache_write_5m + w1h as f64 * w.cache_write_1h) / (w5 + w1h) as f64
        } else {
            w.cache_write_5m
        };
        t
    }

    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    /// Requests after `ts` and before the first compaction after `ts`: how many prompts carry a
    /// tool result produced at `ts`.
    pub fn later_requests(&self, ts: u64) -> u64 {
        let end = self
            .compactions
            .iter()
            .find(|&&c| c > ts)
            .copied()
            .unwrap_or(u64::MAX);
        let from = self.requests.partition_point(|&r| r <= ts);
        let to = self.requests.partition_point(|&r| r < end);
        to.saturating_sub(from) as u64
    }

    /// Cost weight of one token that entered the context at `ts`
    /// (`w_first + (k − 1) · w_read`, or 0 when no request followed).
    pub fn weight_at(&self, ts: u64) -> f64 {
        match self.later_requests(ts) {
            0 => 0.0,
            k => self.w_first + (k - 1) as f64 * self.w_read,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(ts: &str, body: &str) -> String {
        format!(r#"{{"timestamp":"{ts}",{body}}}"#)
    }

    fn req(ts: &str, id: &str, w5: u64, w1h: u64) -> String {
        line(
            ts,
            &format!(
                r#""type":"assistant","message":{{"id":"{id}","usage":{{"input_tokens":1,"cache_creation_input_tokens":{},"cache_creation":{{"ephemeral_5m_input_tokens":{w5},"ephemeral_1h_input_tokens":{w1h}}},"cache_read_input_tokens":5,"output_tokens":1}},"content":[]}}"#,
                w5 + w1h
            ),
        )
    }

    fn timeline(lines: &[String]) -> Timeline {
        Timeline::from_reader(lines.join("\n").as_bytes(), &Weights::default())
    }

    #[test]
    fn counts_requests_until_compaction() {
        let t = timeline(&[
            req("2026-11-10T10:00:00.000Z", "m1", 100, 0),
            // Same response split over two lines: one request.
            req("2026-11-10T10:00:00.000Z", "m1", 100, 0),
            req("2026-11-10T10:00:10.000Z", "m2", 10, 0),
            req("2026-11-10T10:00:20.000Z", "m3", 10, 0),
            line(
                "2026-11-10T10:00:30.000Z",
                r#""type":"system","subtype":"compact_boundary""#,
            ),
            line(
                "2026-11-10T10:00:30.500Z",
                r#""type":"user","isCompactSummary":true,"message":{"content":"x"}"#,
            ),
            req("2026-11-10T10:00:40.000Z", "m4", 10, 0),
        ]);
        let at = |s| parse_iso_ms(s).unwrap();
        // A result at 10:00:05 is in the prompts of m2 and m3, then compaction drops it.
        assert_eq!(t.later_requests(at("2026-11-10T10:00:05.000Z")), 2);
        assert_eq!(t.later_requests(at("2026-11-10T10:00:35.000Z")), 1);
        assert_eq!(t.later_requests(at("2026-11-10T10:00:50.000Z")), 0);
        assert_eq!(
            t.compactions.len(),
            1,
            "summary line is not a second boundary"
        );
        let w = t.weight_at(at("2026-11-10T10:00:05.000Z"));
        assert!((w - (1.25 + 0.1)).abs() < 1e-9, "{w}");
        assert_eq!(t.weight_at(at("2026-11-10T10:00:50.000Z")), 0.0);
    }

    #[test]
    fn first_write_weight_mixes_cache_lifetimes() {
        let t = timeline(&[
            req("2026-11-10T10:00:00.000Z", "a", 100, 0),
            req("2026-11-10T10:00:01.000Z", "b", 0, 100),
        ]);
        assert!((t.w_first - (1.25 + 2.0) / 2.0).abs() < 1e-9);
        assert!(timeline(&[]).is_empty());
    }
}
