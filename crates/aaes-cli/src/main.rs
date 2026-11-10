mod install;
mod mine;
mod ops;
mod replay;

use std::path::PathBuf;

use aaes_runtime::{Runtime, server};
use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

/// AAES — Adaptive Agent Efficiency Engine: shapes large coding-agent tool output without losing facts.
#[derive(Parser)]
#[command(name = "aaes", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Http,
    Command,
}

#[derive(Subcommand)]
enum Cmd {
    /// Analyse local Claude Code / Codex transcripts (read-only) and report where tool-output
    /// tokens go (M-1). Output contains aggregate numbers and command-family names only.
    Mine(mine::MineArgs),
    /// Run the current shapers over past Claude Code tool results (read-only) and report what
    /// each family would have saved, cost-weighted. An upper bound: follow-ups are not counted.
    Replay(replay::ReplayArgs),
    /// Install AAES hooks into Claude Code settings (backs the file up first).
    Init {
        /// `http` (resident server; default on Windows) or `command` (process per call).
        #[arg(long, value_enum)]
        mode: Option<ModeArg>,
        /// Settings file [default: $CLAUDE_CONFIG_DIR/settings.json or ~/.claude/settings.json].
        #[arg(long)]
        settings: Option<PathBuf>,
        /// Server port for http mode [default: from config, 47830].
        #[arg(long)]
        port: Option<u16>,
        /// Print the resulting settings instead of writing them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove AAES hooks from Claude Code settings and stop the server.
    Uninstall {
        #[arg(long)]
        settings: Option<PathBuf>,
    },
    /// Hook handler (command mode): event JSON on stdin, response on stdout.
    Hook {
        #[arg(long = "by-aaes-init", hide = true)]
        by_init: bool,
    },
    /// Run the resident HTTP hook server.
    Serve {
        #[arg(long)]
        port: Option<u16>,
        /// Start the server in the background if it isn't running (used by the SessionStart hook).
        #[arg(long)]
        ensure: bool,
        /// Stop a running server.
        #[arg(long)]
        stop: bool,
        #[arg(long = "by-aaes-init", hide = true)]
        by_init: bool,
    },
    /// Print a stored raw output (handles appear in shaped search results).
    Show {
        handle: String,
        /// Line range, e.g. 120-180.
        #[arg(long)]
        lines: Option<String>,
        /// Case-insensitive substring filter.
        #[arg(long)]
        grep: Option<String>,
        #[arg(long)]
        head: Option<usize>,
        #[arg(long)]
        tail: Option<usize>,
    },
    /// Summarise local shaping events (savings, pass-through, rebound).
    Report {
        #[arg(long)]
        json: bool,
    },
    /// Show the diagnostic log (AAES_HOME/logs).
    Logs {
        /// Number of most recent lines.
        #[arg(long, default_value_t = 50)]
        tail: usize,
        /// Only warnings and errors.
        #[arg(long)]
        errors: bool,
        /// Read all log files, not just the newest.
        #[arg(long)]
        all: bool,
        /// Print raw JSON lines.
        #[arg(long)]
        raw: bool,
    },
    /// Turn shaping off (kill switch).
    Disable,
    /// Turn shaping back on.
    Enable,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Mine(a) => mine::run(a),
        Cmd::Replay(a) => replay::run(a),
        Cmd::Init {
            mode,
            settings,
            port,
            dry_run,
        } => init(mode, settings, port, dry_run),
        Cmd::Uninstall { settings } => uninstall(settings),
        Cmd::Hook { .. } => ops::hook_cmd(),
        Cmd::Serve {
            port, ensure, stop, ..
        } => ops::serve_cmd(port, ensure, stop),
        Cmd::Show {
            handle,
            lines,
            grep,
            head,
            tail,
        } => ops::show_cmd(&handle, lines.as_deref(), grep.as_deref(), head, tail),
        Cmd::Report { json } => ops::report_cmd(json),
        Cmd::Logs {
            tail,
            errors,
            all,
            raw,
        } => ops::logs_cmd(tail, errors, all, raw),
        Cmd::Disable => ops::set_enabled(false),
        Cmd::Enable => ops::set_enabled(true),
    }
}

fn init(
    mode: Option<ModeArg>,
    settings: Option<PathBuf>,
    port: Option<u16>,
    dry_run: bool,
) -> Result<()> {
    let rt = Runtime::from_env();
    let mode = match mode {
        Some(ModeArg::Http) => install::Mode::Http,
        Some(ModeArg::Command) => install::Mode::Command,
        None => install::Mode::default_for_platform(),
    };
    let port = port.unwrap_or(rt.cfg.port);
    let path = settings.unwrap_or_else(install::default_settings_path);
    let exe = install::exe_for_command(&std::env::current_exe()?);
    let token = server::token(&rt.home)?;
    let mut s = install::read_settings(&path)?;
    install::apply(&mut s, mode, &exe, port, &token);
    if dry_run {
        println!("{}", serde_json::to_string_pretty(&s)?);
        return Ok(());
    }
    let backup = install::write_settings(&path, &s)?;
    aaes_runtime::log::info(
        "init",
        "hooks installed",
        serde_json::json!({ "mode": if mode == install::Mode::Http { "http" } else { "command" }, "port": port, "backup": backup.is_some() }),
    );
    eprintln!(
        "aaes: hooks installed in {} ({} mode)",
        path.display(),
        if mode == install::Mode::Http {
            "http"
        } else {
            "command"
        }
    );
    if let Some(b) = backup {
        eprintln!("aaes: previous settings backed up to {}", b.display());
    }
    if mode == install::Mode::Http {
        eprintln!(
            "aaes: the server starts automatically with each Claude Code session (port {port})."
        );
    }
    eprintln!(
        "aaes: data folder {} · undo with `aaes uninstall`",
        rt.home.display()
    );
    if let Some(dir) = std::env::current_exe()?.parent() {
        if !install::dir_on_path(dir, std::env::var_os("PATH").as_deref()) {
            eprintln!(
                "aaes: note: {} is not on PATH, so `aaes report`/`aaes logs` need the full path. Add it with:",
                dir.display()
            );
            if cfg!(windows) {
                eprintln!(
                    "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path','User') + ';{}', 'User')",
                    dir.display()
                );
            } else {
                eprintln!(
                    "  echo 'export PATH=\"$PATH:{}\"' >> ~/.profile",
                    dir.display()
                );
            }
        }
    }
    Ok(())
}

fn uninstall(settings: Option<PathBuf>) -> Result<()> {
    let rt = Runtime::from_env();
    let path = settings.unwrap_or_else(install::default_settings_path);
    let mut s = install::read_settings(&path)?;
    let removed = install::strip_ours(&mut s);
    if removed > 0 {
        let backup = install::write_settings(&path, &s)?;
        aaes_runtime::log::info(
            "uninstall",
            "hooks removed",
            serde_json::json!({ "entries": removed }),
        );
        eprintln!(
            "aaes: removed {removed} hook entries from {}",
            path.display()
        );
        if let Some(b) = backup {
            eprintln!("aaes: previous settings backed up to {}", b.display());
        }
    } else {
        eprintln!("aaes: no AAES hooks found in {}", path.display());
    }
    if let Ok(t) = server::token(&rt.home) {
        if server::shutdown(rt.cfg.port, &t) {
            eprintln!("aaes: server stopped");
        }
    }
    Ok(())
}
