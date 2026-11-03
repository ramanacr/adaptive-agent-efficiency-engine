//! Agent-neutral event model.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    ClaudeCode,
    Codex,
}

impl Agent {
    pub fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
        }
    }
}

/// Token usage of one model request (one API call), already de-duplicated.
///
/// `input` is *uncached* input. For Codex, `cache_read` is the cached subset of input, and
/// there are no billed cache writes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub input: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub output: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.cache_write_5m += o.cache_write_5m;
        self.cache_write_1h += o.cache_write_1h;
        self.cache_read += o.cache_read;
        self.output += o.output;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// One model request. Every tool result recorded before it is part of its prompt.
    Request(Usage),
    ToolCall {
        id: String,
        tool: String,
        /// Shell command text for shell-like tools (Bash, PowerShell, Codex shell).
        command: Option<String>,
        /// The call edits workspace files (Edit/Write/apply_patch…), which starts a new mutation epoch.
        mutates: bool,
        /// File the call reads or writes (Read/Edit/Write/MultiEdit/NotebookEdit), used in-memory only.
        path: Option<String>,
        /// Read range (`offset`/`limit`) if the read was partial; `None` means a whole-file read.
        range: Option<String>,
    },
    ToolResult {
        id: String,
        /// UTF-8 bytes of the text the model received (images excluded).
        bytes: u64,
        images: u32,
        is_error: bool,
        /// The host already replaced a large output with a preview (Claude Code `<persisted-output>`).
        persisted: bool,
    },
    /// Context compaction boundary: content before it is no longer in the model's context.
    Compaction,
    /// Other content entering the context (user text, attachments, injected reminders). Used to
    /// exclude turns from bytes/token calibration.
    Noise,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ParseStats {
    pub lines: u64,
    pub bad_lines: u64,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub agent: Agent,
    pub events: Vec<Event>,
    pub stats: ParseStats,
    /// First ISO-8601 timestamp seen, used for `--since` filtering.
    pub first_ts: Option<String>,
    /// Sub-agent context (sidechain), separate from the main conversation.
    pub sidechain: bool,
}

impl Session {
    pub fn new(agent: Agent) -> Self {
        Self {
            agent,
            events: Vec::new(),
            stats: ParseStats::default(),
            first_ts: None,
            sidechain: false,
        }
    }

    pub fn note_ts(&mut self, ts: Option<&str>) {
        if self.first_ts.is_none() {
            if let Some(t) = ts {
                self.first_ts = Some(t.to_owned());
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}
