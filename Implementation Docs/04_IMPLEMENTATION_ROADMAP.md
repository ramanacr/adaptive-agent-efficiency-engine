# Implementation Roadmap — AAES v0.4

This is a build sequence, not a promise of timing. Each milestone has objective exit criteria.

## 0. Change log
| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-11-03 | Initial draft (M0–M6). |
| 0.2 | 2026-11-03 | Re-sequenced per design review (`05_DESIGN_REVIEW.md`): added M-1 Measure; the first vertical slice targets Claude Code native hooks instead of MCP; daemonless M1 (ADR-011); Standard-tier scope (ADR-013); MCP/Codex moved to M3; local model (M5) and policy learning + Hardened tier (M6) gated on evidence. Added a language spike (ADR-017) to M0. |
| 0.3 | 2026-11-03 | Rust accepted, so the M0 spike becomes a permanent cold-start CI benchmark. M-1 tool implemented (`aaes mine`). Release pipeline added to M1 (signed binaries) and team policy to M3. Public/private corpus split. Repository layout reflects the real workspace. |
| 0.4 | 2026-11-03 | **M1 re-targeted to read and search shaping** (ADR-022/023) on M-1 evidence. Build/test parsers moved to M3. Sub-agent hook coverage and rebound/Edit-failure metrics added to M1/M2. |

## M-1 — Measure before building  *(done — GO)*
- ✅ `aaes mine` (FUN-022): parallel, read-only analysis of Claude Code and Codex history with aggregate content-free output, `--probe` structure check, `--anonymize`, JSON + Markdown, and the go/no-go verdict at the 20% gate.
- ✅ Run on the maintainer's machine (2026-11-03): **verdict GO** with bytes/token calibrated from transcripts. Context-lever metrics added (unchanged re-reads, large whole-file reads, shell file reads, sub-agent split). Detailed numbers are kept privately (ADR-020).
- ✅ Calibration validated against the `count_tokens` API (`tools/calibration-check`, 60 samples across 5 models): the transcript-derived estimate is within ~5% of API counts and slightly over-counts tokens, so it's conservative for savings claims. Verdict unchanged (GO).
- ☐ Codex: re-run once Codex sessions exist locally (`--probe` first).
- Report: token share by tool/command family, top 20 commands, result-size distribution, occupancy, compactions, re-run rate, cache-write/read mix.

**Exit**: go/no-go decision recorded, plus a ranked list of target command families that seeds M0 fixtures and M1 parsers.

## M0 — Reproducible test harness, contracts and baselines
- Workspace and schema crate; CLI skeleton; telemetry/event schema; command registry schema (ADR-016); end-to-end fixture runner.
- Golden fixtures for the M-1 top command families (expected: shell, .NET, Angular, Git, search, file reads) under Git Bash **and** PowerShell, plus JSON MCP, errors and security attacks.
- Fix-commit-replay task corpus from your own .NET/Angular repos (`03_BENCHMARK_PLAN.md`).
- Baseline captures for arms **A, A′ and B**; register tokenizer and model versions; calibrate the Claude token estimator against `count_tokens`.
- **Cold-start benchmark (permanent, CI):** no-op hook handler p50/p95 on Windows and Linux. Report AAES's own overhead (delta vs a trivial native exe in the same folder; NFR ≤5 ms) separately from the platform spawn floor.
- **ADR-021 spike (Windows):** Claude Code `type: "http"` hook → `aaes serve` on 127.0.0.1 with token auth; measure end-to-end hook latency against the command-hook path.
- Public fixture corpus with `tests/fixtures/PROVENANCE.md`; private corpus wiring to `bench-private/`.

**Exit**: arms A/A′/B reproducible with measured tokens, cache usage and quality outcomes; cold-start benchmark green.

## M1 — First vertical slice: read and search shaping via Claude Code hooks (deterministic)
- **Spike first (1–2 days):**
  - (a) `PostToolUse.updatedToolOutput` replaces Read, Grep and Bash/PowerShell results for the pinned Claude Code version;
  - (b) hooks fire for **sub-agent** tool calls;
  - (c) a shaped Read still satisfies Claude Code's read-before-Edit check;
  - (d) end-to-end hook latency for command vs HTTP hooks on Windows (ADR-021).

  Record the results in the coverage matrix (FUN-012).
- `aaes init` / `aaes uninstall` that install and remove hooks with settings backup (FUN-020).
- **Read shaping** (FUN-025, ADR-022): threshold, outline extractors (C#, TS/JS, HTML, SCSS/CSS, JSON/YAML, Markdown, generic), head lines, footer, "ask twice, get full". Covers the Read tool plus shell `cat`/`type`/`Get-Content` single-file reads.
- **Search shaping** (FUN-026, ADR-023): Grep tool content mode, `grep`/`rg`/`git grep`/`Select-String`.
- Command registry with pass-through defaults; long-running/interactive streaming; Windows execution fidelity (FUN-017, FUN-018).
- Per-session SQLite (WAL) + encrypted content-addressed raw store; 128-bit handles; retention/quota; redaction (SEC-012..014).
- `aaes show <handle> [--lines|--grep|--tail|--head]`; once-per-session guidance via `SessionStart.additionalContext` (FUN-016).
- Compaction-aware state reset (FUN-019).
- Standard-tier namespacing and negative tests (SEC-001 Standard, SEC-002, SEC-004).
- Events for rebound (MET-015) and Edit failures (MET-016). Per-session CLI savings report with estimate labels, in both billing views (MET-013).
- Release pipeline v0: signed GitHub Release binaries for Windows/Linux/macOS, SBOM, `cargo-deny` gate (FUN-023, SEC-015).

**Exit**: read/search fidelity goldens pass (exact retrieval of every omitted range; exact match totals); no tool regressions; delivered ≤ raw for every result; hook latency measured; two concurrent sessions isolated; measurable direct reduction on shaped families.

## M2 — Evaluate
- Arm C vs A / A′ / B on the fix-commit-replay corpus and workload strata, paired, powered for a 3–5 pp margin.
- Cache-aware economics; re-run rate; occupancy; compactions; footer overhead.
- Tune per-family policies (versioned config) via offline replay.

**Exit**: net-positive on the representative workload *and* a material gain over A′, or a documented re-scope.

## M3 — Coverage expansion
- Codex adapter: `AGENTS.md` instructions + `PreToolUse` deny-redirect, or an MCP shell server (FUN-021).
- MCP proxy with original response persistence and schema/error fidelity (FUN-001).
- MCP recovery tool equivalent to `aaes show`.
- Agent compatibility/coverage matrix (FUN-012).
- Build/test renderers (`dotnet build/test`, `ng`/`tsc`, `git diff`) and MCP output shaping (e.g. codegraph, ~10k tokens per call), prioritised by the latest `aaes mine` data.
- Team policy file and managed deployment notes (FUN-024); winget/Homebrew/Scoop packages.

**Exit**: coverage measured per pinned agent version; no unsupported universal-compatibility claims.

## M4 — Memory and hybrid retrieval
- Evidence graph, FTS5 over redacted text, freshness/invalidation, compaction-aware already-seen tracking.
- Exact cache only for registry-declared pure commands.
- Optional embeddings/vector plugin behind trait; dedicated index per session.
- Benchmark relevance vs memory and latency before default enablement.

**Exit**: demonstrated fewer repeat calls without stale/foreign evidence; model-off remains usable.

## M5 — Adaptive semantic controller *(gated, ADR-010)*
**Entry gate**: M-1/M2 show a material residual bucket of large unstructured outputs that deterministic parsers can't reduce.
- Small local model abstraction with lazy loading and fallback; off the default latency path.
- Introduce the long-lived daemon (ADR-011) for warm inference: named pipe / UDS; reattach, cancellation and late-reply discard (SEC-008).
- Per-session inference routing; KV/slot isolation tests (SEC-006); CPU quota and fair queue; benchmark 0.6B vs 1.7B-class candidates.

**Exit**: quality and efficiency compare favorably to deterministic baseline on qualifying workloads, with transparent fallback rates.

## M6 — Improvement laboratory, Hardened tier, bounded orchestration *(gated)*
**Entry gate**: an installed base generating enough signal for shadow evaluation.
- Replay engine, immutable policy registry, shadow challenger, evaluators; bounded auto-tuning; versioned promotion and rollback (IMP-004..006).
- Hardened tier: per-principal processes, principal-bound capabilities (SEC-001 Hardened), restrictive ACLs, isolated workers.
- Delegation limited to scoped read-only micro-workflows with per-request work/time/token budgets (ADR-009).
- Installation dashboard.

**Exit**: approved candidate beats baseline without violating mandatory gates; rollback works in induced regression; Hardened-tier hostile-session tests pass.

## Repository organization
```
Cargo.toml                 # workspace (Rust, edition 2024)
crates/
  aaes-cli/                # `aaes` binary: mine (M-1) · later init/uninstall/hook/run/show/report/doctor
  aaes-transcripts/        # ✅ transcript parsing + analysis (Claude Code, Codex)
  # planned, added when first needed (no empty crates):
  aaes-core/               # session model, registry, policy, routing (sync, hot path)
  aaes-adapter-claude/     # Claude Code hook handlers (PostToolUse/PreToolUse/SessionStart/Compact)
  aaes-runner/             # `aaes run`: process exec, streaming, signals, encodings
  aaes-parsers/            # deterministic extracts per command family
  aaes-render/             # native-format renderers + footer
  aaes-evidence/           # encrypted artifact storage / provenance / retention
  aaes-memory/             # SQLite, FTS5, optional vector trait (M4)
  aaes-adapter-codex/, aaes-mcp/, aaes-inference/, aaes-improvement/   # M3–M6
registry/commands.toml
tests/fixtures/{bash,powershell}/ + PROVENANCE.md
bench-private/             # git-ignored private corpus
Implementation Docs/
```

## First vertical slice (minimum useful prototype)
Input: a Claude Code `PostToolUse` event for a **Read** tool call without `offset`/`limit` on a file above the threshold, in a main or sub-agent context. Keep the raw content encrypted. Build the outline with the language's extractor and deliver header + outline + first H lines (host line-number format) + footer with a scoped handle. A second whole-file read of the same unchanged file in the same context returns full content. `aaes show <handle> --lines a-b` returns exact lines for the owning session only. Count raw/delivered tokens (calibrated estimates) and emit rebound events. Then add Grep-tool shaping. Tests:
- wrong handle; outline extractor failure, which falls back to pass-through;
- small-file and ranged-read pass-through; "ask twice";
- compaction reset; CRLF/UTF-8/UTF-16 files; Windows paths.

No LLM, no vectors, no daemon (unless ADR-021 is accepted after the spike).

## Decision log / open technical investigations
1. Which agent hooks reliably cover shell/file/MCP calls for pinned Claude Code and Codex versions? **Partial findings (v0.2):** Claude Code exposes `PreToolUse.updatedInput`, `PostToolUse.updatedToolOutput`, `PreCompact`/`PostCompact`, `SessionStart.source`; confirm `updatedToolOutput` coverage for built-in tools. Codex hooks are deny-only, cannot replace output, cover Bash only, and fail open. Re-verify per pinned version.
2. Which tokenizer is authoritative per provider/model and which input usage fields are available? **Partial findings (v0.2):** no public local Claude tokenizer, so use a calibrated estimator; transcripts carry usage fields including cache-write/read.
3. What local model performs best for relevance and decision classification under CPU-only 4/8/16GB scenarios? *(Only if the M5 gate is met.)*
4. Is semantic retrieval gain worth an embedding model and index under session workloads?
5. Which OS sandbox primitives are required for the Hardened/High-assurance tiers, beyond session checks? See `06_THREAT_MODEL.md`.
6. ~~What data retention and disk quotas…~~ **Decided (ADR-015):** session end + 24 h default, per-session quota with LRU, encryption at rest, redacted indexes. Revisit with user feedback.
7. ~~Rust vs .NET Native AOT~~ **Decided (ADR-017): Rust.**
8. Codex rollout format: validate parser assumptions with `aaes mine --probe` on real rollouts before trusting Codex numbers.
9. Initial-context overhead (MET-014): if it dominates, is tool-schema/context trimming in scope? Decide after M-1.
10. **Windows process-spawn floor (measured 2026-11-03):** 50–100 ms for unsigned executables outside System32 on a managed laptop. AAES's own overhead ≈ 0. Investigate ADR-021 (HTTP hooks + resident `aaes serve`), the effect of signing and install location, and include the spawn floor in the M0 cold-start benchmark as a separate line.

## Stop conditions
Stop optimization and fall back to raw tool result on failed validation, uncertain critical information, unknown structured schema, unknown command class, interactive/long-running command, result below size threshold, stale cache dependency, unavailable inference, unsupported hook version, or budget overrun. Deny execution (not pass through) when authorization fails. Never silently suppress command errors, authorization failures or agent-visible tool protocol errors.
