# adaptive-agent-efficiency-engine

[![ci](https://github.com/ramanacr/adaptive-agent-efficiency-engine/actions/workflows/ci.yml/badge.svg)](https://github.com/ramanacr/adaptive-agent-efficiency-engine/actions/workflows/ci.yml)

**AAES (Adaptive Agent Efficiency Engine)** is a local, open-source sidecar that cuts the tokens coding agents (Claude Code, Codex, MCP clients) spend on tool output, without losing the facts the agent needs.

It runs the real tool unchanged, keeps the full output in an encrypted, session-private store, and gives the agent a compact, validated rendering in the tool's native format, plus a handle (`aaes show <handle>`) for retrieving the rest on demand. It is deterministic-first, written in Rust for minimal per-call overhead, and falls back to the raw output whenever unsure. It reports savings for both **subscription** (usage headroom) and **API** (dollars) sessions.

> **Status:** design v0.5 · M-1 done (GO) · M1 increment 1 working end-to-end with Claude Code (read and search shaping, [contract test](docs/spikes/M1-contract-test.md)). `aaes mine` measures where tool-output tokens go in your existing sessions. See the [roadmap](Implementation%20Docs/04_IMPLEMENTATION_ROADMAP.md).

## Quick start: measure your own sessions (M-1)
```bash
cargo install --path crates/aaes-cli   # or: cargo build --release
aaes mine                               # reads ~/.claude/projects and ~/.codex/sessions (read-only)
aaes mine --since 2026-09-01 --md report.md --json report.json
aaes mine --probe                       # structure check: line/block types only
aaes mine --anonymize                   # hash unknown program names before sharing
```
The report contains aggregate numbers and command-family names only, never transcript content. It ends with a go/no-go verdict: tool results ≥ 20% of cost-weighted input means GO.

## Try it: M1 preview (Claude Code)
```bash
cargo build --release            # binary: target/release/aaes(.exe)
aaes init                        # adds hooks to ~/.claude/settings.json (backup kept);
                                 # Windows: HTTP hooks to a resident `aaes serve`, started by SessionStart
aaes report                      # tokens saved, pass-through, rebound (local, content-free)
aaes disable | aaes enable       # kill switch
aaes uninstall                   # removes only AAES hooks and stops the server
```
What changes for the agent:
- **Large whole-file reads** (Read tool, `cat`, `Get-Content`) return the first 40 lines verbatim plus a structural outline with exact line ranges. Ranged reads pass through, and reading the same file again returns it in full.
- **Large search results** (Grep tool, `grep`/`rg`/`git grep`/`Select-String`) are grouped by file with exact totals; capped lines stay retrievable with `aaes show <handle>`.
- Everything else passes through unchanged, and any error fails open (the original output is used).

## Documents
| Doc | Contents |
|---|---|
| [01 Requirements](Implementation%20Docs/01_REQUIREMENTS.md) | Functional, security, metrics and self-improvement requirements; NFR targets; evidence contracts |
| [02 Architecture & ADRs](Implementation%20Docs/02_ARCHITECTURE_ADRS.md) | Reference architecture, data flow, ADR-001…020, contracts, isolation tiers, risks |
| [03 Benchmark plan](Implementation%20Docs/03_BENCHMARK_PLAN.md) | Phase 0 study, arms A/A′/B/C/D/E, public/private corpora, sample sizes, gates |
| [04 Implementation roadmap](Implementation%20Docs/04_IMPLEMENTATION_ROADMAP.md) | M-1…M6 with exit criteria, first vertical slice, repository layout |
| [05 Design review](Implementation%20Docs/05_DESIGN_REVIEW.md) | Review of v0.1 and the recommendations applied |
| [06 Threat model](Implementation%20Docs/06_THREAT_MODEL.md) | Adversaries, per-tier guarantees and non-guarantees, mandatory tests |
| [M1 hook spike](docs/spikes/M1-hook-spike.md) | Verified Claude Code hook capabilities (output replacement, sub-agents, Edit, latency) |
| [M1 contract test](docs/spikes/M1-contract-test.md) | AAES with real Claude Code: shaping results, latency, defects fixed |

## Contributing
See [CONTRIBUTING.md](CONTRIBUTING.md). Never commit private data: real transcripts, proprietary code or raw tool output belong in the git-ignored `bench-private/`. Security issues: [SECURITY.md](SECURITY.md).

## License
MIT. See [LICENSE](LICENSE).
