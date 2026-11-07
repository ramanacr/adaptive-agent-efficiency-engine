# adaptive-agent-efficiency-engine

[![ci](https://github.com/ramanacr/adaptive-agent-efficiency-engine/actions/workflows/ci.yml/badge.svg)](https://github.com/ramanacr/adaptive-agent-efficiency-engine/actions/workflows/ci.yml)

**AAES (Adaptive Agent Efficiency Engine)** is a local, open-source sidecar that cuts the tokens coding agents (Claude Code, Codex, MCP clients) spend on tool output, without losing the facts the agent needs.

It runs the real tool unchanged, keeps the full output in an encrypted, session-private store, and gives the agent a compact, validated rendering in the tool's native format, plus a handle (`aaes show <handle>`) for retrieving the rest on demand. It is deterministic-first, written in Rust for minimal per-call overhead, and falls back to the raw output whenever unsure. It reports savings for both **subscription** (usage headroom) and **API** (dollars) sessions.

> **Status:** design v0.5 · M-1 done (GO) · M1 preview working end-to-end with Claude Code: search shaping (read shaping built, off by default after the trial), encrypted output store, savings and Edit-failure reports ([contract test](docs/spikes/M1-contract-test.md)). In dogfood trial. `aaes mine` measures where tool-output tokens go in your existing sessions. See the [roadmap](Implementation%20Docs/04_IMPLEMENTATION_ROADMAP.md).
>
> **New here?** Read the [user guide](docs/USER_GUIDE.md): install, what changes for the agent, `aaes show`, how to read `aaes report`, data and privacy, configuration, troubleshooting.

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
aaes report                      # tokens saved, pass-through, follow-ups, MET-015 per family, Edit failures (MET-016)
aaes show <handle> [--grep X]    # lines a shaped search left out (handle is in the result)
aaes logs [--errors] [--tail N]  # diagnostic log: decisions + timings, auth/parse errors, schema drift
aaes disable | aaes enable       # kill switch
aaes uninstall                   # removes only AAES hooks and stops the server
```
Optional `config.json` in the data folder (`%USERPROFILE%\.aaes` on Windows, `~/.local/state/aaes` elsewhere, or `$AAES_HOME`):
`{"log_level": "debug", "session_guidance": true, "read_shaping": false, "read_threshold_tokens": 2000, "search_max_lines_per_file": 20, "search_min_parsed": 0.7, "retention_hours": 24}`.
On Windows the data folder is deliberately outside `AppData`: apps installed as MSIX packages (such as the Claude desktop app) have their `AppData` writes redirected to a private copy, which would otherwise split data, logs and the hook token between a server started from the app and a CLI run from a terminal. Put `aaes` itself somewhere on your `PATH` (`aaes init` warns if it isn't).
Each session starts with a short notice (~150 tokens) telling the agent how shaped output works; set `session_guidance` to `false` to turn it off.
What changes for the agent:
- **Large search results** (Grep tool, `grep`/`rg`/`git grep`/`Select-String`) are grouped by file with exact totals; capped lines stay retrievable with `aaes show <handle>`.
- **Large whole-file reads** pass through by default. Read shaping (first 40 lines plus an outline with exact line ranges) is off, because in the dogfood trial agents read the rest of the file anyway; set `"read_shaping": true` to try it.
- Everything else passes through unchanged, and any error fails open (the original output is used).

Stored outputs are encrypted at rest and deleted 24 h after the session's last activity. Full details: [user guide](docs/USER_GUIDE.md).

## Documents
| Doc | Contents |
|---|---|
| [User guide](docs/USER_GUIDE.md) | Install, agent behavior, `aaes show`, reading `aaes report`, data and privacy, configuration, troubleshooting |
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
