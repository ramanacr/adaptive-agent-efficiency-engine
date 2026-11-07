use std::path::Path;

use aaes_core::shape::ShapeConfig;
use serde_json::Value;

pub const DEFAULT_PORT: u16 = 47_830;

#[derive(Debug, Clone)]
pub struct Config {
    pub shape: ShapeConfig,
    pub port: u16,
    /// Raw artifacts and session state are deleted this long after the session's last activity.
    pub retention_hours: u64,
    /// Diagnostic log level (overridden by the `AAES_LOG` env var).
    pub log_level: crate::log::Level,
    /// Inject a short "AAES is active" notice at session start (FUN-016).
    pub session_guidance: bool,
    /// A read of a shaped file counts as its follow-up (MET-015) only within this window.
    pub followup_window_secs: u64,
    /// Shape large whole-file reads (Read tool and shell `cat`-style reads). Off by default:
    /// the M1 dogfood trial measured a net token loss, because agents read the rest of the file.
    pub read_shaping: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shape: ShapeConfig::default(),
            port: DEFAULT_PORT,
            retention_hours: 24,
            log_level: crate::log::Level::Info,
            session_guidance: true,
            followup_window_secs: 300,
            read_shaping: false,
        }
    }
}

impl Config {
    /// Load `config.json` from the AAES home; missing keys keep their defaults.
    pub fn load(home: &Path) -> Self {
        let mut c = Self::default();
        let Ok(text) = std::fs::read_to_string(home.join("config.json")) else {
            return c;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return c;
        };
        let f = |k: &str| v[k].as_f64();
        let u = |k: &str| v[k].as_u64();
        if let Some(x) = f("bytes_per_token").filter(|x| *x > 0.5) {
            c.shape.bytes_per_token = x;
        }
        if let Some(x) = u("read_threshold_tokens") {
            c.shape.read_threshold_tokens = x as usize;
        }
        if let Some(x) = u("head_lines") {
            c.shape.head_lines = x as usize;
        }
        if let Some(x) = u("search_threshold_tokens") {
            c.shape.search_threshold_tokens = x as usize;
        }
        if let Some(x) = f("search_min_parsed").filter(|x| (0.5..=1.0).contains(x)) {
            c.shape.search_min_parsed = x;
        }
        if let Some(x) = u("search_max_lines_per_file") {
            c.shape.search_max_lines_per_file = (x as usize).max(1);
        }
        if let Some(x) = u("port").filter(|p| *p > 0 && *p < 65_536) {
            c.port = x as u16;
        }
        if let Some(x) = u("retention_hours") {
            c.retention_hours = x;
        }
        if let Some(x) = u("followup_window_secs") {
            c.followup_window_secs = x;
        }
        if let Some(b) = v["read_shaping"].as_bool() {
            c.read_shaping = b;
        }
        if let Some(b) = v["session_guidance"].as_bool() {
            c.session_guidance = b;
        }
        if let Some(l) = v["log_level"].as_str().and_then(crate::log::Level::parse) {
            c.log_level = l;
        }
        c
    }
}
