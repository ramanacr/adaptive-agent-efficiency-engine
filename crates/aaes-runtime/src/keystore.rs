//! OS keystores that hold the artifact key on macOS and Linux (SEC-012). Windows uses DPAPI
//! instead (see `crypto.rs`), so this module is not built there.
//!
//! - macOS: the login Keychain. The item is created by `/usr/bin/security` with an "any
//!   application" access list (`-A`), so a new `aaes` binary after an upgrade reads it without a
//!   prompt. Reads use the native Security framework (no subprocess) with user interaction
//!   switched off; if the native read is refused, one `security find-generic-password` call with a
//!   deadline is the fallback.
//! - Linux: the kernel user keyring (`keyctl`). No D-Bus or desktop service is needed, so it works
//!   on servers and in WSL 2. The kernel drops the keyring at reboot (or when the user's last
//!   session ends); the key is then lost and older artifacts read as expired.
//!
//! The protection is against a copy of the disk or a backup of the home folder, not against code
//! that runs as the same user (ADR-015). Each entry is named by a random id that the key file
//! stores, so several AAES homes never share an entry.

/// What a keystore lookup found.
pub(crate) enum Lookup {
    Found(Vec<u8>),
    /// The entry does not exist (deleted, or the keyring was dropped at reboot): the key is lost.
    Missing,
    /// The keystore could not answer (locked, refused, not supported here). The key may still
    /// exist, so the caller must not replace it.
    Unavailable(String),
}

#[cfg(target_os = "macos")]
pub(crate) use macos::{NAME, delete, get, put};

#[cfg(target_os = "linux")]
pub(crate) use linux::{NAME, delete, get, put};

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) use none::{NAME, delete, get, put};

#[cfg(target_os = "macos")]
mod macos {
    use super::Lookup;
    use security_framework::os::macos::keychain::SecKeychain;
    use security_framework::passwords::{delete_generic_password, get_generic_password};
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    pub(crate) const NAME: &str = "macos-keychain";
    const SERVICE: &str = "aaes";
    const SECURITY: &str = "/usr/bin/security";
    /// `errSecItemNotFound`; `security` exits with 44 for the same condition.
    const NOT_FOUND: i32 = -25300;
    const CLI_NOT_FOUND: i32 = 44;

    fn account(id: &str) -> String {
        format!("artifact-key-{id}")
    }

    /// Store `secret` (ASCII) under `id`. The command goes to `security -i` on stdin, so the secret
    /// is never in a process argument list. The caller verifies the result with `get`.
    pub(crate) fn put(id: &str, secret: &[u8]) -> Result<(), String> {
        let secret = std::str::from_utf8(secret).map_err(|_| "secret is not ASCII".to_string())?;
        let mut child = Command::new(SECURITY)
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start {SECURITY}: {e}"))?;
        let cmd = format!(
            "add-generic-password -a {} -s {SERVICE} -l aaes-artifact-key -A -w {secret}\n",
            account(id)
        );
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(cmd.as_bytes())
                .map_err(|e| format!("cannot write to {SECURITY}: {e}"))?;
        }
        wait(&mut child, Duration::from_secs(10)).map(|_| ())
    }

    pub(crate) fn get(id: &str) -> Lookup {
        let acct = account(id);
        // A refused read must fail at once, never open a dialog inside a hook call.
        let _no_ui = SecKeychain::disable_user_interaction();
        match get_generic_password(SERVICE, &acct) {
            Ok(v) => Lookup::Found(v),
            Err(e) if e.code() == NOT_FOUND => Lookup::Missing,
            Err(e) => cli_get(&acct, e.code()),
        }
    }

    /// Fallback read by the tool that created the item (it is on the item's access list).
    fn cli_get(acct: &str, native: i32) -> Lookup {
        let child = Command::new(SECURITY)
            .args(["find-generic-password", "-s", SERVICE, "-a", acct, "-w"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => return Lookup::Unavailable(format!("native {native}; {e}")),
        };
        let mut out = Vec::new();
        if let Some(mut so) = child.stdout.take() {
            let _ = so.read_to_end(&mut out);
        }
        match wait(&mut child, Duration::from_secs(3)) {
            Ok(0) => {
                while out.last().is_some_and(|b| b.is_ascii_whitespace()) {
                    out.pop();
                }
                Lookup::Found(out)
            }
            Ok(CLI_NOT_FOUND) => Lookup::Missing,
            Ok(code) => Lookup::Unavailable(format!("native {native}; security exit {code}")),
            Err(e) => Lookup::Unavailable(format!("native {native}; {e}")),
        }
    }

    pub(crate) fn delete(id: &str) {
        let _no_ui = SecKeychain::disable_user_interaction();
        let _ = delete_generic_password(SERVICE, &account(id));
    }

    /// Wait for `child` with a deadline; kill it when the deadline passes.
    fn wait(child: &mut std::process::Child, limit: Duration) -> Result<i32, String> {
        let end = Instant::now() + limit;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status.code().unwrap_or(-1)),
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(5)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{SECURITY} timed out"));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::Lookup;
    use linux_keyutils::{KeyError, KeyPermissionsBuilder, KeyRing, KeyRingIdentifier, Permission};

    pub(crate) const NAME: &str = "linux-keyring";

    fn desc(id: &str) -> String {
        format!("aaes:artifact-key:{id}")
    }

    /// The user keyring, linked into this process's own keyring first. The kernel lets a process
    /// read a key only when it "possesses" it (reaches it from its thread, process or session
    /// keyring). A desktop login session links the user keyring into the session keyring, but a
    /// systemd service or CI runner often has a private session keyring that does not. The process
    /// keyring is private to this process and ends with it, so the link changes nothing for others.
    fn ring() -> Result<KeyRing, KeyError> {
        static LINK: std::sync::Once = std::sync::Once::new();
        LINK.call_once(|| {
            if let Ok(p) = KeyRing::from_special_id(KeyRingIdentifier::Process, true) {
                let _ = p.link_keyring_id(KeyRingIdentifier::User);
            }
        });
        KeyRing::from_special_id(KeyRingIdentifier::User, true)
    }

    fn lost(e: KeyError) -> bool {
        matches!(
            e,
            KeyError::KeyDoesNotExist | KeyError::KeyExpired | KeyError::KeyRevoked
        )
    }

    pub(crate) fn put(id: &str, secret: &[u8]) -> Result<(), String> {
        let key = ring()
            .and_then(|r| r.add_key(&desc(id), secret))
            .map_err(|e| format!("{e:?}"))?;
        // The default lets only a "possessor" read the key. Let every process of this user read
        // it, also one whose session keyring does not link the user keyring. Best effort: the
        // caller's read-back decides whether the keyring is usable here.
        let perms = KeyPermissionsBuilder::builder()
            .posessor(Permission::ALL)
            .user(Permission::VIEW | Permission::READ | Permission::SEARCH)
            .build();
        let _ = key.set_perms(perms);
        Ok(())
    }

    pub(crate) fn get(id: &str) -> Lookup {
        let found = ring().and_then(|r| r.search(&desc(id)));
        match found.and_then(|k| k.read_to_vec()) {
            Ok(v) => Lookup::Found(v),
            Err(e) if lost(e) => Lookup::Missing,
            Err(e) => Lookup::Unavailable(format!("{e:?}")),
        }
    }

    pub(crate) fn delete(id: &str) {
        if let Ok(k) = ring().and_then(|r| r.search(&desc(id))) {
            let _ = k.invalidate();
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod none {
    use super::Lookup;

    pub(crate) const NAME: &str = "none";

    pub(crate) fn put(_id: &str, _secret: &[u8]) -> Result<(), String> {
        Err("no OS keystore on this platform".into())
    }

    pub(crate) fn get(_id: &str) -> Lookup {
        Lookup::Unavailable("no OS keystore on this platform".into())
    }

    pub(crate) fn delete(_id: &str) {}
}
