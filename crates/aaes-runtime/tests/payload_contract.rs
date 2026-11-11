//! Offline payload contract (FUN-014): hook inputs in the shapes Claude Code sends
//! (`tests/contract/payloads.json`, field sets observed in real versions) must each lead to the
//! expected decision. A schema change in Claude Code that AAES does not understand makes the hook
//! pass the output through without an event; this test then fails for that case, in CI, without
//! a Claude Code login. The live check is `scripts/contract-test.ps1`.

use std::path::Path;

use aaes_runtime::{Runtime, hook, paths, store};
use serde_json::{Value, json};

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

fn big_grep() -> String {
    let mut s = String::new();
    for f in 0..30 {
        for l in 1..=40 {
            s.push_str(&format!(
                "src/module{f}/file{f}.ts:{l}:  const value{l} = compute(input, {l});\n"
            ));
        }
    }
    s
}

/// Replaces placeholder strings anywhere in the value.
fn fill(v: &mut Value, big: &str, grep: &str) {
    match v {
        Value::String(s) => match s.as_str() {
            "{{BIG_TS}}" => *s = big.to_owned(),
            "{{GREP}}" => *s = grep.to_owned(),
            "{{SMALL}}" => *s = "On branch main\nnothing to commit\n".to_owned(),
            _ => {}
        },
        Value::Array(a) => a.iter_mut().for_each(|x| fill(x, big, grep)),
        Value::Object(m) => m.values_mut().for_each(|x| fill(x, big, grep)),
        _ => {}
    }
}

#[test]
fn observed_payload_shapes_get_the_expected_decision() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/contract/payloads.json");
    let spec: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let home = std::env::temp_dir().join(format!("aaes-contract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    // Read shaping is off by default; the contract covers its payloads too.
    std::fs::write(home.join("config.json"), r#"{"read_shaping": true}"#).unwrap();
    let rt = Runtime::new(home.clone());
    let (big, grep) = (big_ts(), big_grep());

    let cases = spec["cases"].as_array().unwrap();
    assert!(cases.len() >= 10);
    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut event = case["event"].clone();
        fill(&mut event, &big, &grep);
        // The host's line counts, where the fixture leaves them to the content.
        if event["tool_response"]["file"]["numLines"] == json!(0) {
            let n = big.lines().count();
            event["tool_response"]["file"]["numLines"] = json!(n);
            event["tool_response"]["file"]["totalLines"] = json!(n);
        }
        let out = hook::handle(&rt, &event);
        let exp = &case["expect"];

        let replaced = out["hookSpecificOutput"]["updatedToolOutput"].is_object();
        if replaced != exp["replaced"].as_bool().unwrap() {
            failures.push(format!("{name}: replaced = {replaced}"));
        }
        if exp["context"].as_bool() == Some(true)
            && !out["hookSpecificOutput"]["additionalContext"].is_string()
        {
            failures.push(format!("{name}: no additionalContext"));
        }
        if replaced {
            // The replacement keeps the tool's own fields (Claude Code ignores a mismatched shape).
            let (orig, new) = (
                &event["tool_response"],
                &out["hookSpecificOutput"]["updatedToolOutput"],
            );
            for k in orig.as_object().unwrap().keys() {
                if new.get(k).is_none() {
                    failures.push(format!("{name}: replacement lost field `{k}`"));
                }
            }
        }

        let session = event["session_id"].as_str().unwrap();
        let log = paths::session_dir(&home, session).join("events.jsonl");
        let last: Option<Value> = std::fs::read_to_string(&log)
            .ok()
            .and_then(|t| t.lines().last().map(|l| serde_json::from_str(l).unwrap()));
        match (&exp["event"], last) {
            (Value::Null, None) => {}
            (Value::Null, Some(e)) => failures.push(format!("{name}: unexpected event {e}")),
            (want, None) => failures.push(format!("{name}: no event, expected {want}")),
            (want, Some(e)) => {
                for k in ["kind", "action", "reason"] {
                    if e[k] != want[k] {
                        failures.push(format!("{name}: {k} = {}, expected {}", e[k], want[k]));
                    }
                }
            }
        }
    }
    // Also removes the OS keystore entry on macOS and Linux.
    let _ = store::remove_key(&home);
    let _ = std::fs::remove_dir_all(&home);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
