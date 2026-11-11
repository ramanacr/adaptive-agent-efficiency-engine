//! AAES runtime: everything with side effects.
//!
//! - [`hook`]: Claude Code hook protocol (PostToolUse shaping, compaction/session resets).
//! - [`state`]: per-session, per-context state (ask-twice counters, epochs).
//! - [`store`]: session-scoped raw artifacts behind unguessable handles, plus retention.
//! - [`events`]: content-free per-session event log (MET-012/015).
//! - [`server`]: resident HTTP hook server (ADR-021).
//!
//! Data lives under [`paths::aaes_home`] (`AAES_HOME`, else `%USERPROFILE%\.aaes` on Windows,
//! `$XDG_STATE_HOME/aaes` or `~/.local/state/aaes` elsewhere).

pub mod config;
mod crypto;
pub mod events;
pub mod hook;
#[cfg(not(windows))]
mod keystore;
pub mod log;
pub mod paths;
pub mod server;
pub mod state;
pub mod store;

use std::path::PathBuf;
use std::sync::Mutex;

pub use config::Config;

pub struct Runtime {
    pub home: PathBuf,
    pub cfg: Config,
    /// Serialises state read-modify-write inside one process (the server handles requests
    /// on several threads).
    pub(crate) lock: Mutex<()>,
}

impl Runtime {
    pub fn new(home: PathBuf) -> Self {
        let cfg = Config::load(&home);
        let level = std::env::var("AAES_LOG")
            .ok()
            .and_then(|v| log::Level::parse(&v))
            .unwrap_or(cfg.log_level);
        log::init(&home, level);
        Self {
            home,
            cfg,
            lock: Mutex::new(()),
        }
    }

    pub fn from_env() -> Self {
        Self::new(paths::aaes_home())
    }

    /// Global kill switch: `AAES_RAW=1` (command mode) or a `disabled` file in the home folder.
    pub fn disabled(&self) -> bool {
        std::env::var("AAES_RAW").is_ok_and(|v| v == "1") || self.home.join("disabled").exists()
    }
}
