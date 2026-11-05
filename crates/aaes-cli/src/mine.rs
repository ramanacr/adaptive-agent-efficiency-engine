//! `aaes mine`: offline transcript analysis (M-1).

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use aaes_transcripts::analyze::{Settings, Weights};
use aaes_transcripts::{
    Agent, Report, Session, analyze_sessions, claude, codex, discover, probe, websearch,
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
