//! Session-scoped raw artifacts behind unguessable handles (FUN-006, ADR-005) and retention (SEC-014).
//!
//! Standard tier (ADR-013): the 128-bit random handle is the capability. Files live in the
//! user-private AAES home, encrypted at rest (SEC-012, [`crypto`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::{crypto, paths};

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

/// Persist raw output for a session under `handle`, encrypted.
pub fn save(home: &Path, session_id: &str, handle: &str, raw: &str) -> std::io::Result<()> {
    let dir = paths::session_dir(home, session_id).join("artifacts");
    let file = dir.join(format!("{handle}.bin"));
    paths::write_private(&file, &crypto::seal(home, handle, raw.as_bytes())?)?;
    paths::write_private(&index_path(home, handle), file.to_string_lossy().as_bytes())
}

/// Load raw output by handle. `None` if unknown, invalid, expired or unreadable (key lost).
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
    let plain = crypto::open(home, handle, &std::fs::read(target).ok()?)?;
    String::from_utf8(plain).ok()
}

/// Delete sessions whose last activity is older than `retention`, plaintext `.txt` artifacts
/// written before SEC-012 (unreadable now anyway), and dangling handle entries. Returns the
/// number of sessions removed.
pub fn sweep(home: &Path, retention: Duration) -> usize {
    let now = SystemTime::now();
    let mut removed = 0;
    if let Ok(rd) = std::fs::read_dir(home.join("sessions")) {
        for e in rd.flatten() {
            let p = e.path();
            if let Ok(arts) = std::fs::read_dir(p.join("artifacts")) {
                for a in arts.flatten() {
                    if a.path().extension().is_some_and(|x| x == "txt") {
                        let _ = std::fs::remove_file(a.path());
                    }
                }
            }
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

    #[test]
    fn artifacts_are_encrypted_and_lost_key_expires_them() {
        let home = std::env::temp_dir().join(format!("aaes-store-enc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let h = new_handle();
        let canary = "AKIA_CANARY_7f3e9b1c secret=hunter2";
        save(&home, "sess-1", &h, canary).unwrap();
        let file = std::fs::read_to_string(index_path(&home, &h)).unwrap();
        let bytes = std::fs::read(file.trim()).unwrap();
        assert!(
            !bytes.windows(7).any(|w| w == b"hunter2"),
            "no plaintext on disk"
        );
        assert_eq!(load(&home, &h).as_deref(), Some(canary));
        // A pre-SEC-012 plaintext artifact is removed by the sweep; the sealed one stays.
        let legacy = paths::session_dir(&home, "sess-1")
            .join("artifacts")
            .join("h_old.txt");
        std::fs::write(&legacy, canary).unwrap();
        assert_eq!(sweep(&home, Duration::from_secs(3600)), 0);
        assert!(!legacy.exists());
        assert_eq!(load(&home, &h).as_deref(), Some(canary));
        // Key lost: a fresh process finds no key, creates a new one, and the old artifact reads
        // as expired instead of failing.
        std::fs::remove_file(home.join("artifact-key")).unwrap();
        crypto::forget(&home);
        assert_eq!(load(&home, &h), None);
        let _ = std::fs::remove_dir_all(&home);
    }
}
