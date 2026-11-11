//! `aaes mine`: offline transcript analysis (M-1).

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use aaes_transcripts::analyze::{Settings, Weights};
use aaes_transcripts::{
    Agent, Report, Session, analyze_sessions, claude, codex, discover, probe, tools, websearch,
};
use anyhow::{Context, Result, bail};
use clap::Args;
use rayon::prelude::*;

#[derive(Args)]
pub struct MineArgs {
    /// Claude Code projects dir [default: $CLAUDE_CONFIG_DIR/projects or ~/.claude/projects]
    #[arg(long)]
    claude_dir: Option<PathBuf>,
    /// Codex sessions dir [default: $CODEX_HOME/sessions or ~/.codex/sessions]
    #[arg(long)]
    codex_dir: Option<PathBuf>,
    /// Skip Claude Code transcripts.
    #[arg(long)]
    no_claude: bool,
    /// Skip Codex transcripts.
    #[arg(long)]
    no_codex: bool,
    /// Only sessions starting on/after this date (ISO-8601 prefix, e.g. 2026-09-01).
    #[arg(long)]
    since: Option<String>,
    /// Bytes per token for estimating tool-result tokens [default: calibrated from transcripts].
    #[arg(long)]
    bytes_per_token: Option<f64>,
    /// Go/no-go threshold for tool-result share of cost-weighted input.
    #[arg(long, default_value_t = 0.20)]
    gate: f64,
    /// Hash unknown program names (use before sharing a report).
    #[arg(long)]
    anonymize: bool,
    /// Rows in the top-families table.
    #[arg(long, default_value_t = 25)]
    top: usize,
    /// Write the full JSON report here.
    #[arg(long)]
    json: Option<PathBuf>,
    /// Write the Markdown report here (default: stdout).
    #[arg(long)]
    md: Option<PathBuf>,
    /// Print a structural fingerprint (line/block types only) instead of a report.
    #[arg(long)]
    probe: bool,
    /// Report the fixed request prefix instead (ADR-025): what the system prompt, tool
    /// definitions and listings cost, by MCP server and plugin, and which servers had no calls.
    /// Read-only; Claude Code 2.1.304 or later.
    #[arg(long)]
    tools: bool,
}

pub fn run(a: MineArgs) -> Result<()> {
    if a.bytes_per_token.is_some_and(|b| b.is_nan() || b <= 0.0) {
        bail!("--bytes-per-token must be > 0");
    }
    if !(0.0..=1.0).contains(&a.gate) {
        bail!("--gate must be between 0 and 1");
    }

    let mut roots: Vec<(Agent, PathBuf)> = Vec::new();
    if !a.no_claude {
        if let Some(p) = a.claude_dir.clone().or_else(discover::default_claude_root) {
            roots.push((Agent::ClaudeCode, p));
        }
    }
    if !a.no_codex {
        if let Some(p) = a.codex_dir.clone().or_else(discover::default_codex_root) {
            roots.push((Agent::Codex, p));
        }
    }

    let mut files: Vec<(Agent, PathBuf)> = Vec::new();
    for (agent, root) in &roots {
        if !root.is_dir() {
            eprintln!(
                "aaes: {} transcripts not found at {}",
                agent.label(),
                root.display()
            );
            continue;
        }
        let found = discover::jsonl_files(root);
        eprintln!(
            "aaes: {} - {} transcript files under {}",
            agent.label(),
            found.len(),
            root.display()
        );
        files.extend(found.into_iter().map(|f| (*agent, f)));
    }
    if files.is_empty() {
        bail!("no transcript files found");
    }

    if a.probe {
        for (agent, _) in &roots {
            let list: Vec<PathBuf> = files
                .iter()
                .filter(|(ag, _)| ag == agent)
                .map(|(_, p)| p.clone())
                .collect();
            println!("## {} (latest {} files)", agent.label(), list.len().min(50));
            for (sig, n) in probe::probe(*agent, &list, 50) {
                println!("{n:>8}  {sig}");
            }
        }
        return Ok(());
    }

    if a.tools {
        return tools_report(&a, &files);
    }

    let sessions: Vec<Session> = files
        .par_iter()
        .flat_map_iter(|(agent, path)| -> Vec<Session> {
            let Ok(f) = File::open(path) else {
                return Vec::new();
            };
            let r = BufReader::with_capacity(1 << 16, f);
            let mut out = match agent {
                Agent::ClaudeCode => claude::parse_reader(r),
                Agent::Codex => vec![codex::parse_reader(r)],
            };
            // Claude Code stores sub-agent transcripts under `<session>/subagents/`.
            if path.components().any(|c| c.as_os_str() == "subagents") {
                for s in &mut out {
                    s.sidechain = true;
                }
            }
            out
        })
        .collect();

    let settings = Settings {
        bytes_per_token: a.bytes_per_token,
        weights: Weights::default(),
        gate: a.gate,
        anonymize: a.anonymize,
        since: a.since,
    };
    // MET-017 web-search baseline (ADR-024 entry gate): a second, prefiltered pass over the
    // Claude Code files, which are in the OS file cache by now.
    let since = settings.since.clone();
    let web_calls: Vec<websearch::Call> = files
        .par_iter()
        .filter(|(agent, _)| *agent == Agent::ClaudeCode)
        .flat_map_iter(|(_, path)| match File::open(path) {
            Ok(f) => websearch::calls(BufReader::with_capacity(1 << 16, f), since.as_deref()),
            Err(_) => Vec::new(),
        })
        .collect();

    let accs = analyze_sessions(&sessions, &settings);
    let mut report = Report::build(accs, settings);
    report.web_search = Some(websearch::Baseline::from_calls(web_calls));

    if let Some(p) = &a.json {
        let f = File::create(p).with_context(|| format!("creating {}", p.display()))?;
        serde_json::to_writer_pretty(f, &report)?;
        eprintln!("aaes: JSON report written to {}", p.display());
    }
    let md = report.to_markdown(a.top);
    match &a.md {
        Some(p) => {
            std::fs::write(p, md).with_context(|| format!("writing {}", p.display()))?;
            eprintln!("aaes: Markdown report written to {}", p.display());
        }
        None => print!("{md}"),
    }
    Ok(())
}

/// `aaes mine --tools`: the fixed request prefix of Claude Code sessions (ADR-025).
fn tools_report(a: &MineArgs, files: &[(Agent, PathBuf)]) -> Result<()> {
    let since = a.since.as_deref();
    let per_file: Vec<tools::FileTools> = files
        .par_iter()
        .filter(|(agent, _)| *agent == Agent::ClaudeCode)
        .filter_map(|(_, path)| {
            let f = File::open(path).ok()?;
            let t = tools::FileTools::from_reader(BufReader::with_capacity(1 << 16, f));
            let recent = since.is_none_or(|s| t.first_ts.as_deref().is_some_and(|ts| ts >= s));
            (t.has_prefix() && recent).then_some(t)
        })
        .collect();
    if per_file.is_empty() {
        bail!(
            "no sessions with request shapes found (Claude Code 2.1.304 or later writes them){}",
            since.map(|s| format!(" since {s}")).unwrap_or_default()
        );
    }
    let (bpt, samples) = tools::bytes_per_token(&per_file, a.bytes_per_token);
    let s = tools::summarize(&per_file, &Weights::default(), bpt, samples, a.anonymize);

    if let Some(p) = &a.json {
        let groups: Vec<serde_json::Value> = s
            .groups
            .iter()
            .map(|g| {
                serde_json::json!({
                    "kind": g.kind.label(), "name": g.name, "tokens": g.tokens.round(),
                    "tools_loaded": g.tools_loaded, "tools_deferred": g.tools_deferred,
                    "units": g.units.round(), "share": if s.input_units > 0.0 { g.units / s.input_units } else { 0.0 },
                    "sessions": g.sessions, "calls": g.calls, "last_used": g.last_used,
                })
            })
            .collect();
        let v = serde_json::json!({
            "sessions": s.sessions, "requests": s.requests, "since": a.since,
            "input_units": s.input_units.round(), "prefix_units": s.prefix_units.round(),
            "prefix_share": s.prefix_share(), "prefix_rewrites": s.prefix_rewrites,
            "median_prefix_tokens": s.median_prefix_tokens.round(),
            "bytes_per_token": s.bytes_per_token, "calibration_samples": s.calibration_samples,
            "groups": groups,
        });
        let f = File::create(p).with_context(|| format!("creating {}", p.display()))?;
        serde_json::to_writer_pretty(f, &v)?;
        eprintln!("aaes: JSON report written to {}", p.display());
    }
    let md = tools_markdown(&s, a.since.as_deref(), a.top);
    match &a.md {
        Some(p) => {
            std::fs::write(p, md).with_context(|| format!("writing {}", p.display()))?;
            eprintln!("aaes: Markdown report written to {}", p.display());
        }
        None => print!("{md}"),
    }
    Ok(())
}

fn tools_markdown(s: &tools::Summary, since: Option<&str>, top: usize) -> String {
    use std::fmt::Write;
    let pct = |u: f64| {
        if s.input_units > 0.0 {
            100.0 * u / s.input_units
        } else {
            0.0
        }
    };
    let mut o = String::new();
    let _ = writeln!(o, "# AAES fixed request prefix (ADR-025)\n");
    let _ = writeln!(
        o,
        "{} sessions, {} requests{}. Median prefix: {:.0} tokens ({:.2} bytes/token, {}).\n",
        s.sessions,
        s.requests,
        since.map(|v| format!(" since {v}")).unwrap_or_default(),
        s.median_prefix_tokens,
        s.bytes_per_token,
        if s.calibration_samples >= 5 {
            format!("calibrated from {} fresh sessions", s.calibration_samples)
        } else {
            "default".to_string()
        }
    );
    let _ = writeln!(
        o,
        "**The prefix is {:.1}% of cost-weighted input** ({:.0} of {:.0} units; the cache was rewritten on {} requests).\n",
        100.0 * s.prefix_share(),
        s.prefix_units,
        s.input_units,
        s.prefix_rewrites
    );
    let _ = writeln!(
        o,
        "| Group | Kind | Tokens/session | Tools (loaded/deferred) | Share of input | Calls | Last used |"
    );
    let _ = writeln!(o, "|---|---|---:|---:|---:|---:|---|");
    for g in s.groups.iter().take(top) {
        let tools = if g.tools_loaded + g.tools_deferred > 0 {
            format!("{}/{}", g.tools_loaded, g.tools_deferred)
        } else {
            "-".to_string()
        };
        let calls = if matches!(g.kind, tools::Kind::System | tools::Kind::Listing) {
            "-".to_string()
        } else {
            g.calls.to_string()
        };
        let last = g
            .last_used
            .as_deref()
            .map_or("-", |t| t.get(..10).unwrap_or(t));
        let _ = writeln!(
            o,
            "| {} | {} | {:.0} | {} | {:.1}% | {} | {} |",
            g.name,
            g.kind.label(),
            g.tokens,
            tools,
            pct(g.units),
            calls,
            last
        );
    }
    if s.groups.len() > top {
        let _ = writeln!(
            o,
            "\n({} more groups; raise `--top` to see them.)",
            s.groups.len() - top
        );
    }
    let unused: Vec<&tools::Group> = s.unused().collect();
    if unused.is_empty() {
        let _ = writeln!(
            o,
            "\nEvery MCP server and plugin in the prefix had calls in this window."
        );
    } else {
        let u: f64 = unused.iter().map(|g| g.units).sum();
        let _ = writeln!(
            o,
            "\n## No calls in this window\n\nThese servers are in the prefix but had no calls ({:.1}% of cost-weighted input together):\n",
            pct(u)
        );
        for g in &unused {
            let _ = writeln!(
                o,
                "- {} ({:.0} tokens/session, {:.1}%)",
                g.name,
                g.tokens,
                pct(g.units)
            );
        }
        let _ = writeln!(
            o,
            "\nIf you do not need one, you can turn it off in Claude Code for the projects where it is unused \
             (`/mcp` for MCP servers, `/plugin` for plugins). A server that is rarely used but needed shows here too. \
             AAES changes no settings."
        );
    }
    o
}
