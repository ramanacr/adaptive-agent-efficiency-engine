//! Encryption at rest for raw artifacts (SEC-012, ADR-015).
//!
//! One random 256-bit key per AAES home, stored in `artifact-key` wrapped by the OS: DPAPI
//! (CurrentUser) on Windows; elsewhere, until Keychain/libsecret support lands, a 0600 file in the
//! 0700 home (the documented file-permission fallback). Each artifact is sealed with
//! ChaCha20-Poly1305 under a random 96-bit nonce, with the handle as associated data so a file
//! cannot be swapped between handles.
//!
//! Key loss (profile reset, key file copied to another machine or user): the key cannot be
//! unwrapped, so a new one replaces it and older artifacts fail authentication. Their handles then
//! read as expired, and the retention sweep removes the files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde_json::json;

use crate::{log, paths};

const KEY_FILE: &str = "artifact-key";
const KEY_MAGIC: &[u8; 8] = b"AAESKEY1";
const BLOB_MAGIC: &[u8; 4] = b"AAE1";
const NONCE_LEN: usize = 12;

type RawKey = [u8; 32];

/// Keys already unwrapped in this process, per home (the server unwraps once, not per call).
fn cache() -> &'static Mutex<HashMap<PathBuf, RawKey>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, RawKey>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn key(home: &Path) -> std::io::Result<RawKey> {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(k) = c.get(home) {
        return Ok(*k);
    }
    let k = load_or_create(home)?;
    c.insert(home.to_path_buf(), k);
    Ok(k)
}

/// Drop the cached key for `home` (tests simulate a fresh process after key loss).
#[cfg(test)]
pub(crate) fn forget(home: &Path) {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(home);
}

fn read_key(path: &Path) -> std::io::Result<Option<RawKey>> {
    let bytes = std::fs::read(path)?;
    Ok(bytes
        .strip_prefix(KEY_MAGIC)
        .and_then(unwrap_key)
        .and_then(|k| RawKey::try_from(k.as_slice()).ok()))
}

fn load_or_create(home: &Path) -> std::io::Result<RawKey> {
    let path = home.join(KEY_FILE);
    match read_key(&path) {
        Ok(Some(k)) => return Ok(k),
        Ok(None) => {
            log::warn(
                "store",
                "artifact key cannot be unwrapped; replacing it (older artifacts become unreadable)",
                json!({}),
            );
            std::fs::remove_file(&path)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut k: RawKey = [0; 32];
    getrandom::fill(&mut k).map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut file = KEY_MAGIC.to_vec();
    file.extend(wrap_key(&k)?);
    // Publish without replacing: if another process (server and `aaes show`) created the key
    // first, the hard link fails and we use theirs.
    let tmp = path.with_extension(format!("new{}", std::process::id()));
    paths::write_private(&tmp, &file)?;
    let linked = std::fs::hard_link(&tmp, &path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(k),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            read_key(&path)?.ok_or_else(|| {
                std::io::Error::other("artifact key written by another process is unreadable")
            })
        }
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
fn wrap_key(k: &[u8]) -> std::io::Result<Vec<u8>> {
    dpapi(k, true).ok_or_else(|| std::io::Error::other("DPAPI CryptProtectData failed"))
}

#[cfg(windows)]
fn unwrap_key(w: &[u8]) -> Option<Vec<u8>> {
    dpapi(w, false)
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

/// File-permission fallback: the key file is 0600 inside the 0700 home (`paths::write_private`).
#[cfg(not(windows))]
fn wrap_key(k: &[u8]) -> std::io::Result<Vec<u8>> {
    Ok(k.to_vec())
}

#[cfg(not(windows))]
fn unwrap_key(w: &[u8]) -> Option<Vec<u8>> {
    Some(w.to_vec())
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

    #[test]
    fn key_persists_and_unreadable_key_is_replaced() {
        let h = home("loss");
        let k1 = load_or_create(&h).unwrap();
        assert_eq!(load_or_create(&h).unwrap(), k1, "same key on reload");
        // Simulate a key that cannot be unwrapped (another user/machine, corrupted file).
        std::fs::write(h.join(KEY_FILE), b"AAESKEY1garbage").unwrap();
        let k2 = load_or_create(&h).unwrap();
        assert_ne!(k1, k2);
        assert_eq!(read_key(&h.join(KEY_FILE)).unwrap(), Some(k2));
        let _ = std::fs::remove_dir_all(&h);
    }
}
