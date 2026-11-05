//! Diagnostic log: `AAES_HOME/logs/aaes-YYYY-MM-DD.jsonl` (UTC dates), one JSON object per line.
//!
//! Records server lifecycle, every hook decision with its duration, auth/parse failures,
//! unexpected tool-output shapes (Claude Code schema drift) and caught panics. It is
//! content-free: no file contents or command text; paths only as short hashes (except at `debug`).
//!
//! Level: `AAES_LOG` env var or `log_level` in `config.json` (`error`, `warn`, `info` (default),
//! `debug`). Files older than 14 days are removed by [`prune`]; one day's file stops growing at
//! 50 MB, after which only errors are written.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl Level {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Level::Error),
            "warn" | "warning" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" | "trace" => Some(Level::Debug),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
        }
    }
}

const MAX_DAY_BYTES: u64 = 50 * 1024 * 1024;
const KEEP_DAYS: u64 = 14;

struct Sink {
    dir: PathBuf,
    level: Level,
    open: Mutex<Option<(String, File)>>,
}

static SINK: OnceLock<Sink> = OnceLock::new();

/// Initialise once per process. Later calls are ignored.
pub fn init(home: &Path, level: Level) {
    let _ = SINK.set(Sink {
        dir: home.join("logs"),
        level,
        open: Mutex::new(None),
    });
}

/// Log folder of this process's sink (tests scan it for leaked content).
#[cfg(test)]
pub(crate) fn dir() -> Option<PathBuf> {
    SINK.get().map(|s| s.dir.clone())
}

pub fn enabled(level: Level) -> bool {
    SINK.get().is_some_and(|s| level <= s.level)
}

/// `YYYY-MM-DD` and `YYYY-MM-DDTHH:MM:SS.mmmZ` for a UNIX time (UTC).
pub fn utc_parts(t: SystemTime) -> (String, String) {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = d.as_secs();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    let date = format!("{year:04}-{month:02}-{day:02}");
    let ts = format!(
        "{date}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        d.subsec_millis()
    );
    (date, ts)
}

/// Write one record. `fields` must be a JSON object (anything else is stored under `data`).
pub fn log(level: Level, component: &str, msg: &str, fields: Value) {
    let Some(sink) = SINK.get() else { return };
    if level > sink.level {
        return;
    }
    let (date, ts) = utc_parts(SystemTime::now());
    let mut rec = Map::new();
    rec.insert("ts".into(), json!(ts));
    rec.insert("lvl".into(), json!(level.as_str()));
    rec.insert("pid".into(), json!(std::process::id()));
    rec.insert("comp".into(), json!(component));
    rec.insert("msg".into(), json!(msg));
    match fields {
        Value::Object(m) => rec.extend(m),
        Value::Null => {}
        other => {
            rec.insert("data".into(), other);
        }
    }
    let line = format!("{}\n", Value::Object(rec));

    let mut guard = sink.open.lock().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref().is_none_or(|(d, _)| *d != date) {
        if crate::paths::create_private_dir(&sink.dir).is_err() {
            return;
        }
        let path = sink.dir.join(format!("aaes-{date}.jsonl"));
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => *guard = Some((date.clone(), f)),
            Err(_) => return,
        }
    }
    if let Some((_, f)) = guard.as_mut() {
        let big = f
            .metadata()
            .map(|m| m.len() > MAX_DAY_BYTES)
            .unwrap_or(false);
        if !big || level == Level::Error {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

pub fn error(c: &str, m: &str, f: Value) {
    log(Level::Error, c, m, f)
}
pub fn warn(c: &str, m: &str, f: Value) {
    log(Level::Warn, c, m, f)
}
pub fn info(c: &str, m: &str, f: Value) {
    log(Level::Info, c, m, f)
}
pub fn debug(c: &str, m: &str, f: Value) {
    log(Level::Debug, c, m, f)
}

/// Delete log files older than 14 days. Returns how many were removed.
pub fn prune(home: &Path) -> usize {
    let cutoff = SystemTime::now() - Duration::from_secs(KEEP_DAYS * 86_400);
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(home.join("logs")) {
        for e in rd.flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t < cutoff);
            if old && std::fs::remove_file(e.path()).is_ok() {
                n += 1;
            }
        }
    }
    n
}

/// Log files, newest last.
pub fn files(home: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(home.join("logs"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Short, stable, non-reversible id for paths/sessions in logs.
pub fn short_hash(s: &str) -> String {
    crate::events::fnv_hex(s)[..10].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formatting() {
        let t = UNIX_EPOCH + Duration::from_millis(1_793_779_200_123); // 2026-11-04T08:00:00.123Z
        let (d, ts) = utc_parts(t);
        assert_eq!(d, "2026-11-04");
        assert_eq!(ts, "2026-11-04T08:00:00.123Z");
        assert_eq!(utc_parts(UNIX_EPOCH).1, "1970-01-01T00:00:00.000Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(utc_parts(leap).0, "2000-02-29");
    }

    #[test]
    fn levels() {
        assert_eq!(Level::parse("WARN"), Some(Level::Warn));
        assert!(Level::Error < Level::Debug);
        assert_eq!(Level::parse("nope"), None);
    }
}
