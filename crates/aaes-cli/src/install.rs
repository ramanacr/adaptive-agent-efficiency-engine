//! `aaes init` / `aaes uninstall`: add or remove AAES hooks in a Claude Code settings file
//! (FUN-020). The original file is backed up first. Our entries carry a marker
//! (`--by-aaes-init` in commands, `/aaes/v1/` in URLs), so uninstall removes only what we added.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub const MARK: &str = "--by-aaes-init";
pub const TOKEN_VAR: &str = "AAES_HOOK_TOKEN";
/// Only the tools AAES shapes pay the hook cost (M1 spike decision).
pub const MATCHER: &str = "Read|Grep|Bash|PowerShell";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `type: "http"` hooks to the resident `aaes serve` (ADR-021; default on Windows).
    Http,
    /// A process per hook call (`aaes hook`).
    Command,
}

impl Mode {
    pub fn default_for_platform() -> Self {
        if cfg!(windows) {
            Mode::Http
        } else {
            Mode::Command
        }
    }
}

pub fn default_settings_path() -> PathBuf {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(d).join("settings.json");
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".claude").join("settings.json")
}

/// Executable path as it should appear in a hook command (forward slashes, quoted if needed).
pub fn exe_for_command(exe: &Path) -> String {
    let s = exe.to_string_lossy().replace('\\', "/");
    if s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s
    }
}

/// Whether `dir` is one of the entries of a `PATH`-style value (case-insensitive on Windows,
/// ignoring trailing separators).
pub fn dir_on_path(dir: &Path, path_var: Option<&std::ffi::OsStr>) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy();
        let s = s.trim_end_matches(['/', '\\']);
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s.to_owned()
        }
    };
    let want = norm(dir);
    path_var.is_some_and(|v| std::env::split_paths(v).any(|p| norm(&p) == want))
}

fn command_hook(exe: &str, args: &str, timeout: u64) -> Value {
    json!({ "type": "command", "command": format!("{exe} {args} {MARK}"), "timeout": timeout })
}

fn http_hook(port: u16) -> Value {
    json!({
        "type": "http",
        "url": format!("http://127.0.0.1:{port}/aaes/v1/hook"),
        "timeout": 5,
        "headers": { "Authorization": format!("Bearer ${TOKEN_VAR}") },
        "allowedEnvVars": [TOKEN_VAR],
    })
}

/// Our hook groups per event.
pub fn our_hooks(mode: Mode, exe: &str, port: u16) -> Vec<(&'static str, Value)> {
    match mode {
        Mode::Http => vec![
            (
                "SessionStart",
                json!({ "hooks": [command_hook(exe, &format!("serve --ensure --port {port}"), 10)] }),
            ),
            (
                "PostToolUse",
                json!({ "matcher": MATCHER, "hooks": [http_hook(port)] }),
            ),
            ("PreCompact", json!({ "hooks": [http_hook(port)] })),
            ("PostCompact", json!({ "hooks": [http_hook(port)] })),
        ],
        Mode::Command => vec![
            (
                "SessionStart",
                json!({ "hooks": [command_hook(exe, "hook", 10)] }),
            ),
            (
                "PostToolUse",
                json!({ "matcher": MATCHER, "hooks": [command_hook(exe, "hook", 10)] }),
            ),
            (
                "PreCompact",
                json!({ "hooks": [command_hook(exe, "hook", 10)] }),
            ),
            (
                "PostCompact",
                json!({ "hooks": [command_hook(exe, "hook", 10)] }),
            ),
        ],
    }
}

pub fn is_ours(hook: &Value) -> bool {
    hook["command"].as_str().is_some_and(|c| c.contains(MARK))
        || hook["url"]
            .as_str()
            .is_some_and(|u| u.contains("/aaes/v1/"))
}

/// Remove every AAES hook (and our env var), leaving the user's own hooks untouched.
/// Returns the number of hook entries removed.
pub fn strip_ours(settings: &mut Value) -> usize {
    let mut removed = 0;
    if let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) {
        for groups in hooks.values_mut() {
            let Some(arr) = groups.as_array_mut() else {
                continue;
            };
            for g in arr.iter_mut() {
                if let Some(hs) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                    let before = hs.len();
                    hs.retain(|h| !is_ours(h));
                    removed += before - hs.len();
                }
            }
            arr.retain(|g| g["hooks"].as_array().is_none_or(|h| !h.is_empty()));
        }
        hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    }
    if settings["hooks"].as_object().is_some_and(|h| h.is_empty()) {
        settings.as_object_mut().map(|o| o.remove("hooks"));
    }
    if let Some(env) = settings.get_mut("env").and_then(Value::as_object_mut) {
        env.remove(TOKEN_VAR);
        if env.is_empty() {
            settings.as_object_mut().map(|o| o.remove("env"));
        }
    }
    removed
}

/// Install our hooks (replacing any previous AAES entries).
pub fn apply(settings: &mut Value, mode: Mode, exe: &str, port: u16, token: &str) {
    strip_ours(settings);
    if !settings.is_object() {
        *settings = json!({});
    }
    let obj = settings.as_object_mut().expect("object");
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    for (event, group) in our_hooks(mode, exe, port) {
        let arr = hooks
            .as_object_mut()
            .expect("object")
            .entry(event)
            .or_insert_with(|| json!([]));
        if !arr.is_array() {
            *arr = json!([]);
        }
        arr.as_array_mut().expect("array").push(group);
    }
    if mode == Mode::Http {
        let env = obj.entry("env").or_insert_with(|| json!({}));
        if !env.is_object() {
            *env = json!({});
        }
        env[TOKEN_VAR] = json!(token);
    }
}

/// Read settings (missing file = `{}`); refuses to proceed on invalid JSON.
pub fn read_settings(path: &Path) -> anyhow::Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(t) if t.trim().is_empty() => Ok(json!({})),
        Ok(t) => serde_json::from_str(&t).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid JSON ({e}); not modifying it",
                path.display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e.into()),
    }
}

/// Back up (if the file exists) and write settings atomically. Returns the backup path.
pub fn write_settings(path: &Path, settings: &Value) -> anyhow::Result<Option<PathBuf>> {
    let backup = if path.exists() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let b = path.with_extension(format!("json.aaes-backup-{ts}"));
        std::fs::copy(path, &b)?;
        Some(b)
    } else {
        None
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.aaes-tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(settings)? + "\n")?;
    std::fs::rename(&tmp, path)?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_on_path_matches_entries() {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let v = std::ffi::OsString::from(format!("/usr/bin{sep}/opt/aaes/{sep}/x"));
        assert!(dir_on_path(Path::new("/opt/aaes"), Some(&v)));
        assert!(dir_on_path(Path::new("/usr/bin"), Some(&v)));
        assert!(!dir_on_path(Path::new("/opt"), Some(&v)));
        assert!(!dir_on_path(Path::new("/opt/aaes"), None));
    }

    fn user_settings() -> Value {
        json!({
            "model": "opus",
            "hooks": {
                "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "~/.claude/hooks/rtk-rewrite.sh" }] }],
                "PostToolUse": [{ "matcher": "Edit", "hooks": [{ "type": "command", "command": "prettier --write" }] }]
            },
            "env": { "FOO": "1" }
        })
    }

    #[test]
    fn apply_http_keeps_user_hooks_and_order() {
        let mut s = user_settings();
        apply(&mut s, Mode::Http, "C:/tools/aaes.exe", 47830, "tok123");
        let keys: Vec<&String> = s.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["model", "hooks", "env"], "key order preserved");
        let post = s["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 2);
        assert_eq!(post[0]["hooks"][0]["command"], "prettier --write");
        assert_eq!(post[1]["matcher"], MATCHER);
        assert_eq!(
            post[1]["hooks"][0]["url"],
            "http://127.0.0.1:47830/aaes/v1/hook"
        );
        assert_eq!(
            post[1]["hooks"][0]["headers"]["Authorization"],
            "Bearer $AAES_HOOK_TOKEN"
        );
        assert_eq!(
            s["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "C:/tools/aaes.exe serve --ensure --port 47830 --by-aaes-init"
        );
        assert_eq!(s["env"]["AAES_HOOK_TOKEN"], "tok123");
        assert_eq!(
            s["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "~/.claude/hooks/rtk-rewrite.sh"
        );
    }

    #[test]
    fn reapply_is_idempotent_and_uninstall_restores() {
        let original = user_settings();
        let mut s = original.clone();
        apply(&mut s, Mode::Http, "aaes", 1, "t");
        apply(&mut s, Mode::Command, "aaes", 1, "t");
        assert_eq!(
            s["hooks"]["PostToolUse"].as_array().unwrap().len(),
            2,
            "no duplicates"
        );
        assert!(
            s["env"].get(TOKEN_VAR).is_none(),
            "command mode has no token"
        );
        let removed = strip_ours(&mut s);
        assert_eq!(removed, 4);
        assert_eq!(s, original);
    }

    #[test]
    fn empty_settings_round_trip() {
        let mut s = json!({});
        apply(
            &mut s,
            Mode::Http,
            "\"C:/Program Files/aaes/aaes.exe\"",
            2,
            "t",
        );
        assert!(s["hooks"]["PreCompact"].is_array());
        strip_ours(&mut s);
        assert_eq!(s, json!({}));
    }

    #[test]
    fn exe_paths_are_shell_safe() {
        assert_eq!(
            exe_for_command(Path::new(r"C:\Claude_Builds\aaes.exe")),
            "C:/Claude_Builds/aaes.exe"
        );
        assert_eq!(
            exe_for_command(Path::new(r"C:\Program Files\aaes\aaes.exe")),
            "\"C:/Program Files/aaes/aaes.exe\""
        );
    }
}
