//! `--probe`: structural fingerprint of transcripts (line/block *types* only, never content).
//! Used to check parser assumptions against a user's local agent versions.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use serde_json::Value;

use crate::model::Agent;

pub fn probe(agent: Agent, files: &[PathBuf], max_files: usize) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for path in files.iter().rev().take(max_files) {
        let Ok(f) = File::open(path) else { continue };
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                *counts.entry("<invalid json>".to_owned()).or_default() += 1;
                continue;
            };
            for sig in signatures(agent, &v) {
                *counts.entry(sig).or_default() += 1;
            }
        }
    }
    counts
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("-")
}

fn signatures(agent: Agent, v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match agent {
        Agent::ClaudeCode => {
            let base = format!("type={} subtype={}", s(&v["type"]), s(&v["subtype"]));
            out.push(base.clone());
            if let Some(blocks) = v["message"]["content"].as_array() {
                for b in blocks {
                    out.push(format!("{base} block={}", s(&b["type"])));
                }
            }
            if v["message"]["usage"].is_object() {
                out.push(format!("{base} has_usage"));
            }
        }
        Agent::Codex => {
            out.push(format!(
                "type={} payload.type={}",
                s(&v["type"]),
                s(&v["payload"]["type"])
            ));
        }
    }
    out
}
