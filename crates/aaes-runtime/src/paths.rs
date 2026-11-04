use std::path::{Path, PathBuf};

/// AAES data folder (user-private).
///
/// On Windows this is `%USERPROFILE%\.aaes`, deliberately outside `AppData`: processes started by
/// MSIX-packaged apps (e.g. the Claude desktop app and every agent/hook it spawns) have their
/// `AppData` writes silently redirected to a per-package copy, so a server started from such a
/// process and a CLI started from a normal terminal would otherwise see different data folders,
/// different hook tokens, and split logs.
pub fn aaes_home() -> PathBuf {
    if let Some(h) = std::env::var_os("AAES_HOME") {
        return PathBuf::from(h);
    }
    if cfg!(windows) {
        if let Some(u) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(u).join(".aaes");
        }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(l).join("aaes");
        }
    }
    if let Some(x) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(x).join("aaes");
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".local").join("state").join("aaes")
}

/// Restrict an id to `[A-Za-z0-9_-]`, max 64 chars, so it is safe as a path component.
pub fn sanitize_id(id: &str) -> String {
    let s: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();
    if s.is_empty() { "unknown".into() } else { s }
}

pub fn session_dir(home: &Path, session_id: &str) -> PathBuf {
    home.join("sessions").join(sanitize_id(session_id))
}

/// Create a directory (and parents) readable only by the current user where the OS allows.
pub fn create_private_dir(p: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(p)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Write a file atomically (temp file + rename), user-private on Unix.
pub fn write_private(p: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = p.parent() {
        create_private_dir(dir)?;
    }
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_path_safe() {
        assert_eq!(sanitize_id("abc-123_X"), "abc-123_X");
        assert_eq!(sanitize_id("../../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize_id(""), "unknown");
    }
}
