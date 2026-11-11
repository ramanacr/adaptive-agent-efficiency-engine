//! Encryption at rest for raw artifacts (SEC-012, ADR-015).
//!
//! One random 256-bit key per AAES home, protected by the OS and referenced from `artifact-key`:
//! - Windows: the file holds the key wrapped by DPAPI (CurrentUser).
//! - macOS and Linux: the key is in the OS keystore (login Keychain; kernel user keyring) and the
//!   file holds only the entry id (`KS1:<id>`). Where no keystore is usable (containers that block
//!   `keyctl`, a locked or missing Keychain, other Unix systems) or `AAES_KEY_STORE=file` is set,
//!   the file holds the key itself: 0600 inside the 0700 home, the documented file-permission
//!   fallback. A plain key file is moved into the keystore when one becomes usable.
//!
//! Each artifact is sealed with ChaCha20-Poly1305 under a random 96-bit nonce, with the handle as
//! associated data so a file cannot be swapped between handles.
//!
//! Key loss (profile reset, key file copied to another machine or user, Linux keyring dropped at
//! reboot): a new key replaces it and older artifacts fail authentication. Their handles then read
//! as expired, and the retention sweep removes the files. A keystore that exists but cannot answer
//! now (for example a locked Keychain) is not key loss: the key is kept and sealing fails, so the
//! hook passes output through unchanged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde_json::json;

#[cfg(not(windows))]
use crate::keystore::{self, Lookup};
use crate::{log, paths};

const KEY_FILE: &str = "artifact-key";
const KEY_MAGIC: &[u8; 8] = b"AAESKEY1";
/// Key-file payload prefix for a key held in the OS keystore; 32 hex characters of entry id follow.
#[cfg(not(windows))]
const KS_TAG: &[u8] = b"KS1:";
const BLOB_MAGIC: &[u8; 4] = b"AAE1";
const NONCE_LEN: usize = 12;

type RawKey = [u8; 32];

/// What the key file gave.
enum Stored {
    /// The key; `plain` when the file holds it unwrapped (the non-Windows file fallback).
    Key { key: RawKey, plain: bool },
    /// The key cannot be recovered. `expected` when its keystore entry is simply gone (Linux
    /// keyring after a reboot): logged at info level, not as a warning.
    Lost { expected: bool },
    /// The keystore cannot answer now; the key may still exist and must not be replaced.
    #[cfg_attr(windows, allow(dead_code))]
    Unavailable(String),
}

/// Keys already unwrapped in this process, per home (the server unwraps once, not per call).
fn cache() -> &'static Mutex<HashMap<PathBuf, RawKey>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, RawKey>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Use the OS keystore unless `AAES_KEY_STORE=file`. Unit tests use the file, so they never add
/// entries to the developer's Keychain or keyring; the keystore tests opt in explicitly.
fn use_keystore() -> bool {
    !cfg!(test) && std::env::var("AAES_KEY_STORE").map_or(true, |v| v != "file")
}

fn key(home: &Path) -> std::io::Result<RawKey> {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(k) = c.get(home) {
        return Ok(*k);
    }
    let k = load_or_create(home, use_keystore())?;
    c.insert(home.to_path_buf(), k);
    Ok(k)
}

/// Delete the artifact key of `home`: its OS keystore entry and the key file. Sealed artifacts of
/// that home become unreadable.
pub fn remove_key(home: &Path) -> std::io::Result<()> {
    let path = home.join(KEY_FILE);
    #[cfg(not(windows))]
    if let Some(id) = std::fs::read(&path)
        .ok()
        .as_deref()
        .and_then(|b| b.strip_prefix(KEY_MAGIC))
        .and_then(keystore_id)
    {
        keystore::delete(id);
    }
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(home);
    let _ = std::fs::remove_file(path.with_extension("nostore"));
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Drop the cached key for `home` (tests simulate a fresh process after key loss).
#[cfg(test)]
pub(crate) fn forget(home: &Path) {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(home);
}

fn read_key(path: &Path) -> std::io::Result<Stored> {
    let bytes = std::fs::read(path)?;
    Ok(match bytes.strip_prefix(KEY_MAGIC) {
        Some(w) => unwrap_key(w),
        None => Stored::Lost { expected: false },
    })
}

fn load_or_create(home: &Path, keystore: bool) -> std::io::Result<RawKey> {
    let path = home.join(KEY_FILE);
    match read_key(&path) {
        Ok(Stored::Key { key, plain }) => {
            if plain && keystore {
                move_to_keystore(&path, &key);
            }
            return Ok(key);
        }
        Ok(Stored::Unavailable(why)) => {
            return Err(std::io::Error::other(format!(
                "artifact key is in the OS keystore, which cannot be read now: {why}"
            )));
        }
        Ok(Stored::Lost { expected }) => {
            let level = if expected {
                log::Level::Info
            } else {
                log::Level::Warn
            };
            log::log(
                level,
                "store",
                "artifact key cannot be recovered; replacing it (older artifacts become unreadable)",
                json!({}),
            );
            std::fs::remove_file(&path)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut k: RawKey = [0; 32];
    getrandom::fill(&mut k).map_err(|e| std::io::Error::other(e.to_string()))?;
    let (wrapped, entry) = wrap_key(&k, keystore)?;
    let mut file = KEY_MAGIC.to_vec();
    file.extend(wrapped);
    // Publish without replacing: if another process (server and `aaes show`) created the key
    // first, the hard link fails and we use theirs.
    let tmp = path.with_extension(format!("new{}", std::process::id()));
    let linked = paths::write_private(&tmp, &file).and_then(|()| std::fs::hard_link(&tmp, &path));
    let _ = std::fs::remove_file(&tmp);
    if linked.is_err() {
        drop_entry(entry.as_deref());
    }
    match linked {
        Ok(()) => {
            if keystore && entry.is_none() {
                note_no_keystore(&path);
            }
            log::info(
                "store",
                "artifact key created",
                json!({"key_store": store_name(entry.as_deref())}),
            );
            Ok(k)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match read_key(&path)? {
            Stored::Key { key, .. } => Ok(key),
            _ => Err(std::io::Error::other(
                "artifact key written by another process is unreadable",
            )),
        },
        Err(e) => Err(e),
    }
}

/// Encrypt `plain` for `handle`: `AAE1 || nonce || ciphertext+tag`.
pub fn seal(home: &Path, handle: &str, plain: &[u8]) -> std::io::Result<Vec<u8>> {
    let k = key(home)?;
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|e| std::io::Error::other(e.to_string()))?;
    let ct = ChaCha20Poly1305::new(Key::from_slice(&k))
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plain,
                aad: handle.as_bytes(),
            },
        )
        .map_err(|_| std::io::Error::other("artifact encryption failed"))?;
    let mut out = Vec::with_capacity(BLOB_MAGIC.len() + NONCE_LEN + ct.len());
    out.extend_from_slice(BLOB_MAGIC);
    out.extend_from_slice(&nonce);
    out.extend(ct);
    Ok(out)
}

/// Decrypt a sealed artifact. `None` for a wrong key, a wrong handle, tampering or an unsealed
/// (pre-SEC-012) file.
pub fn open(home: &Path, handle: &str, blob: &[u8]) -> Option<Vec<u8>> {
    let rest = blob.strip_prefix(BLOB_MAGIC)?;
    if rest.len() < NONCE_LEN {
        return None;
    }
    let (nonce, ct) = rest.split_at(NONCE_LEN);
    let k = key(home).ok()?;
    ChaCha20Poly1305::new(Key::from_slice(&k))
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ct,
                aad: handle.as_bytes(),
            },
        )
        .ok()
}

#[cfg(windows)]
fn wrap_key(k: &RawKey, _keystore: bool) -> std::io::Result<(Vec<u8>, Option<String>)> {
    dpapi(k, true)
        .map(|w| (w, None))
        .ok_or_else(|| std::io::Error::other("DPAPI CryptProtectData failed"))
}

#[cfg(windows)]
fn unwrap_key(w: &[u8]) -> Stored {
    match dpapi(w, false).and_then(|k| RawKey::try_from(k.as_slice()).ok()) {
        Some(key) => Stored::Key { key, plain: false },
        None => Stored::Lost { expected: false },
    }
}

#[cfg(windows)]
fn move_to_keystore(_path: &Path, _k: &RawKey) {}

#[cfg(windows)]
fn drop_entry(_entry: Option<&str>) {}

#[cfg(windows)]
fn store_name(_entry: Option<&str>) -> &'static str {
    "dpapi"
}

/// DPAPI (CurrentUser) protect/unprotect. The second documented `unsafe` exception (see
/// `aaes-cli/src/ops.rs` for the first): the Win32 API has no safe wrapper in `windows-sys`.
#[cfg(windows)]
#[allow(unsafe_code)]
fn dpapi(data: &[u8], protect: bool) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    const ENTROPY: &[u8] = b"aaes-artifact-key-v1";
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).ok()?,
        pbData: data.as_ptr().cast_mut(),
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr().cast_mut(),
    };
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: `input` and `entropy` point to live, correctly sized buffers that DPAPI only reads.
    // On success DPAPI allocates `out.pbData` with LocalAlloc (`out.cbData` bytes); we copy it and
    // release it with LocalFree exactly once. No prompt struct, no description, UI forbidden.
    unsafe {
        let ok = if protect {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 || out.pbData.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData.cast());
        Some(v)
    }
}

/// Key-file payload: the keystore reference when the keystore takes the key, else the key itself
/// (the 0600 file fallback, `paths::write_private`). Also returns the new entry id, for clean-up.
#[cfg(not(windows))]
fn wrap_key(k: &RawKey, keystore: bool) -> std::io::Result<(Vec<u8>, Option<String>)> {
    if keystore {
        if let Some(id) = keystore_put(k) {
            let mut w = KS_TAG.to_vec();
            w.extend(id.as_bytes());
            return Ok((w, Some(id)));
        }
    }
    Ok((k.to_vec(), None))
}

/// Store `k` under a new random id and read it back. `None`, with nothing left behind, when the
/// keystore is not usable here.
#[cfg(not(windows))]
fn keystore_put(k: &RawKey) -> Option<String> {
    let mut raw_id = [0u8; 16];
    getrandom::fill(&mut raw_id).ok()?;
    let id = hex(&raw_id);
    let secret = hex(k);
    let ok = match keystore::put(&id, secret.as_bytes()) {
        Ok(()) => matches!(keystore::get(&id), Lookup::Found(v) if v == secret.as_bytes()),
        Err(e) => {
            log::debug(
                "store",
                "OS keystore not usable; the key stays in the 0600 file",
                json!({"key_store": keystore::NAME, "error": e}),
            );
            false
        }
    };
    if ok {
        Some(id)
    } else {
        keystore::delete(&id);
        None
    }
}

#[cfg(not(windows))]
fn unwrap_key(w: &[u8]) -> Stored {
    if let Ok(key) = RawKey::try_from(w) {
        return Stored::Key { key, plain: true };
    }
    let Some(id) = keystore_id(w) else {
        return Stored::Lost { expected: false };
    };
    match keystore::get(id) {
        Lookup::Found(v) => match unhex(&v).and_then(|b| RawKey::try_from(b.as_slice()).ok()) {
            Some(key) => Stored::Key { key, plain: false },
            None => {
                keystore::delete(id);
                Stored::Lost { expected: false }
            }
        },
        Lookup::Missing => Stored::Lost { expected: true },
        Lookup::Unavailable(why) => Stored::Unavailable(why),
    }
}

/// The entry id of a `KS1:` payload: exactly 32 lowercase hex characters, so a damaged or edited
/// key file can never inject text into a keystore command.
#[cfg(not(windows))]
fn keystore_id(w: &[u8]) -> Option<&str> {
    let id = w.strip_prefix(KS_TAG)?;
    if id.len() == 32 && id.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        std::str::from_utf8(id).ok()
    } else {
        None
    }
}

/// A plain key file found while a keystore is usable: move the key into the keystore and rewrite
/// the file atomically. A process that read the old file holds the same key, so this is safe while
/// other AAES processes run. Best effort: on failure the file stays as it was. A failed attempt
/// is not repeated for a day, because a keystore that is not there can cost seconds to find out
/// (`security` timeouts) and command mode starts one process per tool call.
#[cfg(not(windows))]
fn move_to_keystore(path: &Path, k: &RawKey) {
    let marker = no_keystore_marker(path);
    let recent = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < NO_KEYSTORE_RETRY);
    if recent {
        return;
    }
    let Some(id) = keystore_put(k) else {
        note_no_keystore(path);
        return;
    };
    let mut file = KEY_MAGIC.to_vec();
    file.extend(KS_TAG);
    file.extend(id.as_bytes());
    match paths::write_private(path, &file) {
        Ok(()) => {
            let _ = std::fs::remove_file(&marker);
            log::info(
                "store",
                "artifact key moved into the OS keystore",
                json!({"key_store": keystore::NAME}),
            );
        }
        Err(_) => keystore::delete(&id),
    }
}

/// How long a failed keystore attempt holds off the next one.
#[cfg(not(windows))]
const NO_KEYSTORE_RETRY: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

#[cfg(not(windows))]
fn no_keystore_marker(path: &Path) -> PathBuf {
    path.with_extension("nostore")
}

/// Record that the keystore was not usable (the marker's modification time is the attempt time).
#[cfg(not(windows))]
fn note_no_keystore(path: &Path) {
    let _ = paths::write_private(&no_keystore_marker(path), b"");
}

#[cfg(windows)]
fn note_no_keystore(_path: &Path) {}

#[cfg(not(windows))]
fn drop_entry(entry: Option<&str>) {
    if let Some(id) = entry {
        keystore::delete(id);
    }
}

#[cfg(not(windows))]
fn store_name(entry: Option<&str>) -> &'static str {
    if entry.is_some() {
        keystore::NAME
    } else {
        "file"
    }
}

#[cfg(not(windows))]
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(not(windows))]
fn unhex(s: &[u8]) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    s.chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(tag: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("aaes-crypto-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        h
    }

    #[test]
    fn seal_open_round_trip_and_binding() {
        let h = home("rt");
        let blob = seal(&h, "h_a", b"password=hunter2").unwrap();
        assert!(!blob.windows(7).any(|w| w == b"hunter2"));
        assert_eq!(
            open(&h, "h_a", &blob).as_deref(),
            Some(&b"password=hunter2"[..])
        );
        assert_eq!(open(&h, "h_b", &blob), None, "bound to its handle");
        let mut bad = blob.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(open(&h, "h_a", &bad), None, "tamper detected");
        assert_eq!(open(&h, "h_a", b"plain legacy text"), None);
        let file = std::fs::read(h.join(KEY_FILE)).unwrap();
        assert!(file.starts_with(KEY_MAGIC));
        let _ = std::fs::remove_dir_all(&h);
    }

    fn stored(h: &Path) -> Option<RawKey> {
        match read_key(&h.join(KEY_FILE)).unwrap() {
            Stored::Key { key, .. } => Some(key),
            _ => None,
        }
    }

    #[test]
    fn key_persists_and_unreadable_key_is_replaced() {
        let h = home("loss");
        let k1 = load_or_create(&h, false).unwrap();
        assert_eq!(load_or_create(&h, false).unwrap(), k1, "same key on reload");
        // Simulate a key that cannot be unwrapped (another user/machine, corrupted file).
        std::fs::write(h.join(KEY_FILE), b"AAESKEY1garbage").unwrap();
        let k2 = load_or_create(&h, false).unwrap();
        assert_ne!(k1, k2);
        assert_eq!(stored(&h), Some(k2));
        remove_key(&h).unwrap();
        assert!(!h.join(KEY_FILE).exists());
        let _ = std::fs::remove_dir_all(&h);
    }

    #[cfg(not(windows))]
    #[test]
    fn keystore_ids_are_strict_and_hex_round_trips() {
        assert_eq!(
            keystore_id(b"KS1:0123456789abcdef0123456789abcdef"),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(keystore_id(b"KS1:0123456789abcdef0123456789abcde"), None);
        assert_eq!(keystore_id(b"KS1:0123456789abcdef0123456789abcdeF"), None);
        assert_eq!(keystore_id(b"KS1:0123456789abcdef 123456789abcdef"), None);
        assert_eq!(
            unhex(hex(&[0, 1, 0xfe, 0xff]).as_bytes()),
            Some(vec![0, 1, 0xfe, 0xff])
        );
        assert_eq!(unhex(b"0g"), None);
        assert_eq!(unhex(b"abc"), None);
    }

    /// A recent failed keystore attempt stops the next process from trying again.
    #[cfg(not(windows))]
    #[test]
    fn failed_keystore_attempt_is_not_repeated_at_once() {
        let h = home("nostore");
        let k = [7u8; 32];
        let mut plain = KEY_MAGIC.to_vec();
        plain.extend(k);
        paths::write_private(&h.join(KEY_FILE), &plain).unwrap();
        note_no_keystore(&h.join(KEY_FILE));
        assert_eq!(load_or_create(&h, true).unwrap(), k);
        assert_eq!(
            std::fs::read(h.join(KEY_FILE)).unwrap(),
            plain,
            "no attempt"
        );
        remove_key(&h).unwrap();
        assert!(!h.join("artifact-key.nostore").exists());
        let _ = std::fs::remove_dir_all(&h);
    }

    /// The real OS keystore (login Keychain, kernel user keyring). It adds and removes entries, so
    /// it runs only with `AAES_TEST_KEYSTORE=1` (set in CI); there it must not fall back to the file.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn os_keystore_holds_the_key_and_takes_over_a_plain_file() {
        if std::env::var("AAES_TEST_KEYSTORE").as_deref() != Ok("1") {
            return;
        }
        let h = home("ks");
        let k1 = load_or_create(&h, true).unwrap();
        let file = std::fs::read(h.join(KEY_FILE)).unwrap();
        let payload = file.strip_prefix(KEY_MAGIC).unwrap();
        let Some(id) = keystore_id(payload).map(str::to_string) else {
            // Say why the keystore was not used.
            let probe = "00000000000000000000000000000000";
            let put = keystore::put(probe, b"probe");
            let get = match keystore::get(probe) {
                Lookup::Found(_) => "found".to_string(),
                Lookup::Missing => "missing".to_string(),
                Lookup::Unavailable(e) => format!("unavailable: {e}"),
            };
            keystore::delete(probe);
            panic!("key file holds no keystore reference; put: {put:?}; get: {get}");
        };
        assert!(!file.windows(32).any(|w| w == k1), "key not in the file");
        assert_eq!(load_or_create(&h, true).unwrap(), k1, "same key on reload");

        // Entry gone (keyring dropped at reboot): expected loss, new key.
        keystore::delete(&id);
        assert!(matches!(
            read_key(&h.join(KEY_FILE)).unwrap(),
            Stored::Lost { expected: true }
        ));
        let k2 = load_or_create(&h, true).unwrap();
        assert_ne!(k1, k2);
        remove_key(&h).unwrap();

        // A plain (fallback or pre-keystore) key file moves into the keystore, same key.
        let mut plain = KEY_MAGIC.to_vec();
        plain.extend(k1);
        paths::write_private(&h.join(KEY_FILE), &plain).unwrap();
        assert_eq!(load_or_create(&h, true).unwrap(), k1);
        let file = std::fs::read(h.join(KEY_FILE)).unwrap();
        assert!(keystore_id(file.strip_prefix(KEY_MAGIC).unwrap()).is_some());
        assert_eq!(stored(&h), Some(k1));
        remove_key(&h).unwrap();
        let _ = std::fs::remove_dir_all(&h);
    }
}
