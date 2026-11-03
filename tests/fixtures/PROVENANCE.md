# Fixture provenance (ADR-020)

Every fixture in this repository must be **synthetic** or derived from a **public OSS repository**, and must be listed here.

| Path | Source | Commit / version | License | Notes |
|---|---|---|---|---|
| `crates/aaes-transcripts/src/*.rs` (inline test JSONL) | Synthetic, hand-written | — | MIT (this repo) | Mimics Claude Code / Codex transcript structure; no real session data |

Private corpora (team repositories, real transcripts, raw tool output) live in the git-ignored `bench-private/` folder and must never be added here.
