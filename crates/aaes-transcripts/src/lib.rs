//! Offline, read-only analysis of coding-agent transcripts (requirement FUN-022, milestone M-1).
//!
//! Parses Claude Code (`~/.claude/projects/**/*.jsonl`) and Codex (`~/.codex/sessions/**/*.jsonl`)
//! transcripts into a normalised event stream, then reports where tool-output tokens go.
//! Reports contain **aggregate numbers and command-family names only**, never transcript content.

pub mod analyze;
pub mod claude;
pub mod codex;
pub mod discover;
pub mod edits;
pub mod model;
pub mod probe;
pub mod report;

/// Command-family parsing lives in `aaes-core`; re-exported for existing callers.
pub use aaes_core::command;

pub use analyze::{Settings, analyze_sessions};
pub use model::{Agent, Event, Session, Usage};
pub use report::Report;
