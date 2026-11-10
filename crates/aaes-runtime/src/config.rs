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
    /// Shaping families in shadow mode (`read`, `search`): AAES computes the shaped output and
    /// logs its size, but the agent gets the raw output. For trying a renderer on real traffic
    /// without risk. Shadow mode wins over `read_shaping`.
    pub shadow: Vec<String>,
    /// Per-repo overrides (`repos` in `config.json`), longest root first.
    pub repos: Vec<RepoOverride>,
}

/// Shaping settings for one repo. Keys left out of the override keep the global value.
#[derive(Debug, Clone)]
pub struct RepoOverride {
    /// The key as written in `config.json`. Events carry it, so `aaes report` can group by repo.
    pub name: String,
    /// Normalised root folder (see [`norm_path`]).
    pub root: String,
    pub shape: ShapeConfig,
    pub read_shaping: bool,
    pub shadow: Vec<String>,
}

/// The settings that apply to one tool call: the global ones, or a repo override.
#[derive(Debug, Clone, Copy)]
pub struct Effective<'a> {
    pub shape: &'a ShapeConfig,
    pub read_shaping: bool,
    pub shadow: &'a [String],
    /// Name of the matching repo override.
    pub repo: Option<&'a str>,
}

impl Effective<'_> {
    pub fn shadow(&self, family: &str) -> bool {
        self.shadow.iter().any(|f| f == family)
    }
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
            shadow: Vec::new(),
            repos: Vec::new(),
        }
    }
}

/// Families that `shadow` accepts.
pub const SHADOW_FAMILIES: &[&str] = &["read", "search"];

/// Case- and separator-insensitive form of a folder path, without a trailing separator.
pub fn norm_path(p: &str) -> String {
    p.trim()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// Shaping keys that both the global config and a repo override accept.
fn apply_shape(s: &mut ShapeConfig, v: &Value) {
    let u = |k: &str| v[k].as_u64();
    if let Some(x) = u("read_threshold_tokens") {
        s.read_threshold_tokens = x as usize;
    }
    if let Some(x) = u("head_lines") {
        s.head_lines = x as usize;
    }
    if let Some(x) = u("search_threshold_tokens") {
        s.search_threshold_tokens = x as usize;
    }
    if let Some(x) = v["search_min_parsed"]
        .as_f64()
        .filter(|x| (0.5..=1.0).contains(x))
    {
        s.search_min_parsed = x;
    }
    if let Some(x) = u("search_max_lines_per_file") {
        s.search_max_lines_per_file = (x as usize).max(1);
    }
}

fn parse_shadow(v: &Value) -> Option<Vec<String>> {
    v.as_array().map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .filter(|s| SHADOW_FAMILIES.contains(s))
            .map(str::to_owned)
            .collect()
    })
}

impl Config {
    /// The shaping family is in shadow mode (global setting).
    pub fn shadow(&self, family: &str) -> bool {
        self.shadow.iter().any(|f| f == family)
    }

    /// Settings for a tool call in `cwd`: the override whose root is `cwd` or the closest folder
    /// above it, else the global settings.
    pub fn for_cwd(&self, cwd: Option<&str>) -> Effective<'_> {
        let hit = cwd.map(norm_path).and_then(|c| {
            self.repos.iter().find(|r| {
                c == r.root || (c.starts_with(&r.root) && c.as_bytes()[r.root.len()] == b'/')
            })
        });
        match hit {
            Some(r) => Effective {
                shape: &r.shape,
                read_shaping: r.read_shaping,
                shadow: &r.shadow,
                repo: Some(&r.name),
            },
            None => Effective {
                shape: &self.shape,
                read_shaping: self.read_shaping,
                shadow: &self.shadow,
                repo: None,
            },
        }
    }

    /// Load `config.json` from the AAES home; missing keys keep their defaults.
    pub fn load(home: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(home.join("config.json")) else {
            return Self::default();
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return Self::default();
        };
        Self::from_value(&v)
    }

    pub fn from_value(v: &Value) -> Self {
        let mut c = Self::default();
        let u = |k: &str| v[k].as_u64();
        if let Some(x) = v["bytes_per_token"].as_f64().filter(|x| *x > 0.5) {
            c.shape.bytes_per_token = x;
        }
        apply_shape(&mut c.shape, v);
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
        if let Some(s) = parse_shadow(&v["shadow"]) {
            c.shadow = s;
        }
        if let Some(b) = v["session_guidance"].as_bool() {
            c.session_guidance = b;
        }
        if let Some(l) = v["log_level"].as_str().and_then(crate::log::Level::parse) {
            c.log_level = l;
        }
        if let Some(m) = v["repos"].as_object() {
            for (name, o) in m.iter().filter(|(_, o)| o.is_object()) {
                let root = norm_path(name);
                if root.is_empty() {
                    continue;
                }
                let mut shape = c.shape.clone();
                apply_shape(&mut shape, o);
                c.repos.push(RepoOverride {
                    name: name.clone(),
                    root,
                    shape,
                    read_shaping: o["read_shaping"].as_bool().unwrap_or(c.read_shaping),
                    shadow: parse_shadow(&o["shadow"]).unwrap_or_else(|| c.shadow.clone()),
                });
            }
            c.repos.sort_by_key(|r| std::cmp::Reverse(r.root.len()));
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repo_override_matches_closest_root_and_inherits_globals() {
        let c = Config::from_value(&json!({
            "search_threshold_tokens": 1200,
            "shadow": ["search"],
            "repos": {
                "D:\\Work\\App\\": { "read_shaping": true, "read_threshold_tokens": 5000 },
                "D:/Work/App/web": { "shadow": ["read"], "search_threshold_tokens": 800 },
                "D:/Work/Other": {},
                "": { "read_shaping": true }
            }
        }));
        assert_eq!(c.repos.len(), 3);

        let g = c.for_cwd(Some("D:/Elsewhere"));
        assert_eq!((g.repo, g.read_shaping), (None, false));
        assert_eq!(g.shape.search_threshold_tokens, 1200);

        let app = c.for_cwd(Some("d:\\work\\app\\src"));
        assert_eq!(app.repo, Some("D:\\Work\\App\\"));
        assert!(app.read_shaping);
        assert_eq!(app.shape.read_threshold_tokens, 5000);
        assert_eq!(app.shape.search_threshold_tokens, 1200);
        assert!(app.shadow("search") && !app.shadow("read"));

        let web = c.for_cwd(Some("D:/Work/App/web"));
        assert_eq!(web.repo, Some("D:/Work/App/web"));
        assert!(!web.read_shaping);
        assert_eq!(web.shape.search_threshold_tokens, 800);
        assert!(web.shadow("read") && !web.shadow("search"));

        // A sibling folder that only shares the prefix is not inside the repo.
        assert_eq!(c.for_cwd(Some("D:/Work/Application")).repo, None);
        assert_eq!(c.for_cwd(None).repo, None);
        assert_eq!(c.for_cwd(Some("D:/Work/Other")).repo, Some("D:/Work/Other"));
    }
}
