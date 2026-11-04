//! Session-scoped raw artifacts behind unguessable handles (FUN-006, ADR-005) and retention (SEC-014).
//!
//! Standard tier (ADR-013): the 128-bit random handle is the capability. Files live in the
//! user-private AAES home. Encryption at rest (SEC-012) is a planned M1 follow-up.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::paths;

const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// `h_` + 26 base32 characters (128 random bits).
pub fn new_handle() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS random source");
    let mut out = String::from("h_");
    let mut buf: u32 = 0;
    let mut bits = 0;
    for b in bytes {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn valid_handle(h: &str) -> bool {
    h.len() == 28 && h.starts_with("h_") && h[2..].bytes().all(|c| ALPHABET.contains(&c))
}

fn index_path(home: &Path, handle: &str) -> PathBuf {
    home.join("handles").join(handle)
}

/// Persist raw output for a session under `handle`.
pub fn save(home: &Path, session_id: &str, handle: &str, raw: &str) -> std::io::Result<()> {
    let dir = paths::session_dir(home, session_id).join("artifacts");
    let file = dir.join(format!("{handle}.txt"));
    paths::write_private(&file, raw.as_bytes())?;
    paths::write_private(&index_path(home, handle), file.to_string_lossy().as_bytes())
}

/// Load raw output by handle. `None` if unknown, invalid or expired.
pub fn load(home: &Path, handle: &str) -> Option<String> {
    if !valid_handle(handle) {
        return None;
    }
    let target = std::fs::read_to_string(index_path(home, handle)).ok()?;
    let target = PathBuf::from(target.trim());
    // The index must point inside our sessions folder.
    if !target.starts_with(home.join("sessions")) {
        return None;
    }
    std::fs::read_to_string(target).ok()
}

/// Delete sessions whose last activity is older than `retention`, and dangling handle entries.
/// Returns the number of sessions removed.
pub fn sweep(home: &Path, retention: Duration) -> usize {
    let now = SystemTime::now();
    let mut removed = 0;
    if let Ok(rd) = std::fs::read_dir(home.join("sessions")) {
        for e in rd.flatten() {
            let p = e.path();
            let last = std::fs::metadata(p.join("state.json"))
                .or_else(|_| std::fs::metadata(&p))
                .and_then(|m| m.modified())
                .unwrap_or(now);
            if now.duration_since(last).unwrap_or_default() > retention
                && std::fs::remove_dir_all(&p).is_ok()
            {
                removed += 1;
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(home.join("handles")) {
        for e in rd.flatten() {
            let ok = std::fs::read_to_string(e.path())
                .map(|t| Path::new(t.trim()).exists())
                .unwrap_or(false);
            if !ok {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_random_and_valid() {
        let a = new_handle();
        let b = new_handle();
        assert_ne!(a, b);
        assert!(valid_handle(&a), "{a}");
        assert!(!valid_handle("h_../../etc"));
    }

    #[test]
    fn save_load_and_sweep() {
        let home = std::env::temp_dir().join(format!("aaes-store-{}", std::process::id()));
        let h = new_handle();
        save(&home, "sess-1", &h, "raw text").unwrap();
        assert_eq!(load(&home, &h).as_deref(), Some("raw text"));
        assert_eq!(load(&home, &new_handle()), None);
        assert_eq!(sweep(&home, Duration::from_secs(3600)), 0);
        assert_eq!(sweep(&home, Duration::ZERO), 1);
        assert_eq!(load(&home, &h), None);
        let _ = std::fs::remove_dir_all(&home);
    }
}
