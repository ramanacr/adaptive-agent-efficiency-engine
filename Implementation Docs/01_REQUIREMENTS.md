# Versioned Requirements Specification — AAES v0.4

**AAES** = Adaptive Agent Efficiency Engine.
Status: Proposed. Priorities P0 essential; P1 next; P2 experimental. Owners to be assigned.
Requirement IDs are stable across versions. Changed priorities are marked, and new requirements get new IDs.

## 0. Change log
| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-11-03 | Initial draft. |
| 0.2 | 2026-11-03 | Applied design review (`05_DESIGN_REVIEW.md`). Native agent hooks are now P0 and the MCP proxy P1 (R1). Standard-tier isolation is scoped to accidental leakage, and principal-bound capabilities move to Hardened/P1 (R3). Added: compact model-facing rendering (R4), recovery UX and re-run metric (R5), compaction-aware state (R6), tokenizer estimates (R7), cache-aware economics (R8), realistic noninferiority margin (R9), command registry, user controls, Windows/PowerShell correctness (R13, R16), secrets at rest and retention (R14), transcript mining (R2). Learning automation moved to P2 behind gates (R11). |
| 0.3 | 2026-11-03 | Owner decisions: **public open-source (MIT)**; must serve **both subscription and API-credit** sessions; audience is **teams and the public**; runtime **Rust** (ADR-017 accepted, speed-critical); M-1 gate **≥20% cost-weighted**. Added FUN-023 (distribution), FUN-024 (team policy), SEC-015 (supply chain), SEC-016 (public-repo data hygiene), MET-013 (billing-mode reporting), MET-014 (initial-context overhead); SEC-010 tightened (no network telemetry by default); cold-start NFRs. FUN-022 implemented as `aaes mine`. Windows process-spawn measurement recorded (NFR split into own overhead vs end-to-end). |
| 0.4 | 2026-11-03 | **M1 re-targeted by M-1 evidence:** file-content reads dominate tool-output cost while build/test output is small. Added FUN-025 (large-file read shaping, P0), FUN-026 (search-result shaping, P0), MET-015 (read rebound), MET-016 (Edit failure rate). Build/test renderers moved to P1 (capture requirement FUN-003 unchanged). FUN-014 must cover sub-agent tool calls. |

## 1. Scope and boundaries
**In scope:** intercept eligible tool calls/results, **native agent hooks first** (Claude Code), then MCP; task-aware lossless/validated reduction; exact and semantic evidence retrieval; session-safe caching; authorized read-only micro-workflows; token and outcome metrics; offline analysis of existing agent transcripts; safe policy learning; offline execution; adapters to tested agent hook APIs and MCP.

**Target platforms:** Windows 10/11 (Git Bash and PowerShell tool shells) and Linux at P0; macOS at P1.

**Audience and licensing:** public open-source (MIT) for individual developers and teams. Must deliver value for **subscription** sessions (usage-limit headroom, fewer compactions) and **API-credit** sessions (dollars), and report both (MET-013).

**Out of scope for v1:** replacing the reasoning/coding model; mandatory cloud APIs; automatic live self-modifying executable code; uncontrolled writes; unrestricted agent-to-agent context sharing; claims of full interception for all products; **security isolation against a hostile agent running as the same OS user in the Standard tier** (see SEC-011 and `06_THREAT_MODEL.md`).

## 2. Functional requirements
| ID | Priority | Requirement | Acceptance evidence |
|---|---|---|---|
| FUN-001 | **P1** (was P0) | Intercept MCP-mediated calls without changing input/output schemas, errors, or tool permissions. | Contract tests with protocol fixtures and representative MCP servers. |
| FUN-002 | P0 | Support pass-through, deterministic extraction, compact structured result, and full-result retrieval. | Byte-level original artifacts and validated summaries. |
| FUN-003 | P0 | Capture command exit code, stderr, relevant stdout, location, counts, diagnostic IDs and ordering when material. | Golden tests for build/test/git/file/search outputs. |
| FUN-004 | P0 | Associate every call with agent instance, session, and tool invocation IDs (authenticated per the active isolation tier). | Concurrent correlation tests; no orphan response delivered to wrong session. |
| FUN-005 | P0 | Session-private exact cache; no automatic cross-session cache hits; versioned invalidation. | Same input in distinct sessions yields independent cache entries. |
| FUN-006 | P0 | Full original results available by unguessable (≥128-bit random), session-scoped handle until retention expiration. | Authorized retrieval succeeds; foreign-session retrieval denied. |
| FUN-007 | P0 | Bypass unsupported/unsafe transforms without failing authorized tool execution. | Fault injection and compatibility tests. |
| FUN-008 | P1 | Maintain task-state, evidence provenance, dependency and freshness records; "already-seen" state obeys FUN-019. | Invalidation replay with source mutation, concurrent modifications and compaction. |
| FUN-009 | P1 | Context-aware optimization based on current task, tool, cost, and risk. | Policy selection trace and replay. |
| FUN-010 | P1 | Authorized bounded, read-only micro-workflows and independent tool-call batching. | No permission escalation; complete provenance of subcalls. |
| FUN-011 | P1 | SQLite FTS5 hybrid retrieval, optional semantic retrieval provider. | Retrieval benchmark; vector feature can be disabled. |
| FUN-012 | P1 | Adapter coverage matrix for Claude Code, Codex, MCP and other agents. | Versioned integration and feature-detection tests; explicit unsupported paths. |
| FUN-013 | P2 | Learned policy ranking, bounded prefetch, semantic cache candidate checks. | Offline evaluation and safe promotion gates. |
| FUN-014 | P0 | **Claude Code native adapter.** Primary: a `PostToolUse` hook replaces the result via `updatedToolOutput` for built-in tools (Bash, PowerShell, Read, Grep, Glob) where the pinned version supports it. Fallback: a `PreToolUse` hook rewrites the command via `updatedInput` (`<cmd>` → `aaes run -- <cmd>`) for Bash **and** PowerShell. Feature-detect per pinned version. **Must also cover tool calls made inside sub-agents**, which carry ~45% of input in M-1 data. | Version-pinned integration tests per tool, main and sub-agent contexts; coverage recorded in FUN-012 matrix. |
| FUN-015 | P0 (generic) / **P1 (build/test renderers)** | **Model-facing rendering.** The agent receives compact text in the tool's native diagnostic format (e.g. `file(l,c): error CS0246: …`, `file(l,c): error TS2322: …`) plus one footer line with counts, exit code and handle. The `OptimizedToolResult` envelope stays internal. Results under a configurable minimum (default ~400 tokens) pass through untouched. | Rendering goldens; measured footer overhead; zero cases where delivered tokens exceed raw tokens. |
| FUN-016 | P0 | **Progressive recovery.** `aaes show <handle> [--lines a-b] [--grep pat] [--tail n] [--head n]`, plus an equivalent MCP tool (P1). Recovery guidance is injected once per session (Claude Code `SessionStart.additionalContext`; Codex `AGENTS.md`). | Recovery tests; guidance-presence test; re-run rate (MET-010). |
| FUN-017 | P0 | **Command classification registry** stored as versioned data: `pure | read-only | mutating | long-running | interactive | unknown`. Default `unknown` means non-cacheable and pass-through. | Registry schema tests; unknown commands are never transformed or cached. |
| FUN-018 | P0 | **Execution fidelity in rewrite mode:** exact exit-code passthrough; signal/Ctrl+C forwarding; stdout/stderr order preserved (sequence-numbered capture); long-running/interactive commands (`ng serve`, `dotnet watch`, `docker compose up`, `npm start`) always streamed through unbuffered; ANSI/progress stripping; CRLF and UTF-16/code-page handling on Windows; correct quoting under Git Bash, PowerShell and cmd. | Cross-platform golden and fault-injection tests. |
| FUN-019 | P0 | **Compaction-aware state.** Reset the per-session "already-seen" and dedupe state on `PreCompact`/`PostCompact` and on `SessionStart` with `source ∈ {compact, clear, resume}`. Where no compaction signal exists (Codex), disable seen-based dedupe or derive it from the rollout file. | Compaction replay tests: no "unchanged since…" reply after context loss. |
| FUN-020 | P0 | **User controls:** `AAES_RAW=1` env bypass, per-command deny-list, global kill switch, and `aaes init` / `aaes uninstall` that back up and restore agent settings files. | Install/uninstall round-trip tests; bypass tests. |
| FUN-021 | P1 | **Codex adapter.** Codex hooks can deny but not rewrite input or replace output, so use `AGENTS.md` prefix instructions plus a `PreToolUse` deny-with-reason redirect to `aaes run -- <cmd>`, or an MCP shell server. Report coverage honestly. | Coverage measurement per pinned Codex version. |
| FUN-022 | P0 ✅ M-1 | **Transcript miner** (`aaes mine`) (offline, read-only) over Claude Code `~/.claude/projects/**/*.jsonl` and Codex `~/.codex/sessions`. Reports token share by tool/command family, result-size distribution, compactions per session and command re-run rate. | Reproducible report on a pinned transcript snapshot; no raw content in output. |
| FUN-025 | P0 | **Large-file read shaping** (ADR-022). For a whole-file read (Read tool without `offset`/`limit`, or shell `cat`/`type`/`Get-Content` of one file) whose result exceeds a threshold (default 2,000 est. tokens), deliver a header (path, total lines, est. tokens, what is shown), a **structural outline** (symbols/sections with line ranges), the **first N lines verbatim** in the host's line-numbered format, and a footer explaining exact retrieval: `Read(offset, limit)` or `aaes show <handle> --lines a-b`. **Ask twice, get full:** a second whole-file read of the same unchanged file in the same context returns full content. Ranged reads, small files, binaries, images and notebooks pass through. | Goldens per language (C#, TS/JS, HTML, SCSS/CSS, JSON/YAML, Markdown, generic); rebound (MET-015) and Edit-failure (MET-016) gates in M2. |
| FUN-026 | P0 | **Search-result shaping.** For Grep tool / `grep` / `rg` / `git grep` / `Select-String` content output above a threshold: group matches by file, print each path once, keep line numbers and match text verbatim, cap matches per file with an explicit `+N more` count and handle, and keep total match/file counts exact. `files_with_matches`/`count` modes pass through unless above threshold. | Goldens incl. Windows paths; exact totals; recovery of capped matches. |
| FUN-023 | P0 | **Distribution:** prebuilt, signed single binaries for win-x64, win-arm64, linux-x64/arm64 (static/musl), macOS universal via GitHub Releases + `cargo install`/`cargo binstall`. winget, Homebrew and Scoop at P1. `aaes init` works without admin rights. | Release pipeline produces verified artifacts; clean-machine install tests per OS. |
| FUN-024 | P1 | **Team policy:** an organization policy file (registry overrides, deny-lists, retention, telemetry off/on, isolation tier) layered *under* user settings where allowed, deployable alongside Claude Code managed settings. | Precedence tests; policy-locked keys can't be overridden by users. |

## 3. Security and isolation
Isolation claims are tier-specific; see `06_THREAT_MODEL.md` and the isolation tiers in `02_ARCHITECTURE_ADRS.md`.

| ID | Priority | Requirement | Acceptance evidence |
|---|---|---|---|
| SEC-001 | P0 (Standard) / **P1 (Hardened)** | **Standard:** session access via unguessable session-scoped handles and physical namespaces; a session ID alone never authorizes access. **Hardened:** capability bound to an OS principal (`token_binding`) and verified on every operation. | Spoofed/guessed IDs rejected (both tiers); principal-mismatch rejected (Hardened). |
| SEC-002 | P0 | Keep context, tool output, cache, index, embeddings, raw artifacts, and policy feedback private per session. | Foreign session cannot list/query/read any private objects through AAES interfaces. |
| SEC-003 | P0 | Bind tool permissions and workspace permissions to original authority, including delegated calls. In hook mode the host agent enforces permissions, and AAES must never widen them. | Denied tool remains denied through proxy/delegation/hook. |
| SEC-004 | P0 | Deny cross-session artifact references even if path/hash/task/repository matches. | Cross-session negative tests. |
| SEC-005 | P0 | Treat tool output, repository text and model outputs as untrusted data, not authorization instructions. | Prompt-injection tests; no policy/permission change. |
| SEC-006 | P0 *(when local inference is enabled)* | Keep inference request payloads, KV states and response routing isolated; disable cross-session prompt-prefix caching unless formally proven safe. | Runtime-level stress and state-reuse tests. |
| SEC-007 | P0 | Protect session-private files with OS permissions where possible; document that unrestricted host access is out of scope of sidecar-only guarantees. | Multi-user filesystem/permissions tests. |
| SEC-008 | P0 | Expire/revoke session capabilities; cancel and discard late replies (applies to any long-lived component). | Crash/reconnect/termination/replay tests. |
| SEC-009 | P1 | Use separate physical stores or processes for elevated threat models. | Configurable isolation tier and test suite. |
| SEC-010 | P0 | Telemetry never contains raw source, secrets or prompt text. **No network telemetry by default.** Metrics stay local; any remote export is explicit opt-in (user or team policy) and content-free. | Telemetry inspection and redaction tests; no outbound connections in default config. |
| SEC-011 | P0 | **Published threat model** listing adversaries, the boundary that defends against each, and what each tier explicitly does *not* protect against. The Standard tier must not be described as a security boundary against a same-user hostile agent. | `06_THREAT_MODEL.md` reviewed; docs/UI wording audit. |
| SEC-012 | P0 | **Encryption at rest** for raw artifacts using an OS-protected key: DPAPI (CurrentUser) on Windows, Keychain on macOS, libsecret/kernel keyring on Linux, with a file-permission fallback documented. | Artifact files unreadable without the key; key-loss behavior tested. |
| SEC-013 | P0 | **Secret redaction before indexing.** FTS/vector indexes hold redacted text only; raw stays encrypted (SEC-012). | Canary-secret tests: secrets never appear in indexes, telemetry or footers. |
| SEC-014 | P0 | **Retention and quota defaults:** raw artifacts kept until session end + 24 h (configurable), with a per-session disk quota and LRU eviction. An expired handle returns a clear "expired" status. | Retention/eviction tests. |
| SEC-015 | P0 | **Supply chain:** minimal dependency tree for hot-path binaries; `cargo-deny`/`cargo-audit` in CI; locked builds; signed releases (Sigstore/cosign provenance, Authenticode for Windows, notarization for macOS when available); SBOM per release. | CI gates; signature/provenance verification in install docs and `aaes doctor`. |
| SEC-016 | P0 | **Public-repo data hygiene:** no proprietary code, real transcripts, raw tool output from non-public repos, or secrets in the repository. Fixtures are synthetic or from public OSS repos with license noted; private corpora live in git-ignored `bench-private/`; secret scan on every push. | CI secret scan; fixture provenance file; contributor guide. |

## 4. Token economics and metrics
| ID | Priority | Requirement | Acceptance evidence |
|---|---|---|---|
| MET-001 | P0 | Count raw and delivered tool-result tokens. **Claude:** no public local tokenizer, so counts are *estimates* from a per-content-class estimator calibrated offline against the `count_tokens` API, with the error band published. **OpenAI/Codex:** local `o200k_base`. Every count carries `estimate_kind`. | Calibration report; parity checks for exact tokenizers. |
| MET-002 | P0 | Measure tool request count, original/optimized bytes, elapsed time, cache hit, policy ID, tool exit status, validation status and recoveries. | Structured per-session event log. |
| MET-003 | P0 | Report direct tool output reduction separately from total agent/API token reduction. | Dashboard labels and metric tests. |
| MET-004 | P0 | Preserve an unoptimized baseline capture and paired experimental protocol for attributed claims. | Reproducible evaluation runs. |
| MET-005 | P1 | Ingest actual provider usage (input/output/cache-write/cache-read), using agent transcripts where API records aren't accessible, and compute bills with versioned pricing. | Reconciliation to provider records/transcripts. |
| MET-006 | P1 | Per-session drilldowns and authorized installation-wide content-free aggregates. | RBAC tests. |
| MET-007 | P1 | Resource overhead: RAM, CPU, inference time, disk, cold start and p50/p95/p99 added latency. | Load test dashboard. |
| MET-008 | P1 | Compare overall task quality, reruns and recovery calls. | Paired task-score datasets. |
| MET-009 | P0 | Transcript-derived baselines (FUN-022) are the first measured evidence and go/no-go input for M1+. | M-1 report. |
| MET-010 | P0 | **Re-run-instead-of-retrieve rate:** how often the agent re-executes a command whose compacted result had a valid handle, instead of using `aaes show`. | Per-session event; reported per arm. |
| MET-011 | P1 | **Context occupancy** (token-turns a tool result stays in context) and **compactions per task**. | Transcript-derived; reported per arm. |
| MET-012 | P0 | Footer/rendering overhead and pass-through rate (results under the FUN-015 threshold). | Per-session event. |
| MET-013 | P0 | **Billing-mode-aware reporting.** Every economics view shows (a) **cost-weighted token units** (relative API price weights; also the stated *proxy* for subscription usage-limit consumption, since plans don't publish token limits) and (b) **dollars** when billing mode = API with a versioned price table. Mode is `api | subscription | unknown` (config; default unknown → show both). | Report tests for each mode; proxy labelled as such. |
| MET-015 | P0 | **Read rebound:** tokens of follow-up ranged reads, `aaes show` calls and "ask twice" full reads that follow a shaped read of the same file in the same context. Net saving = raw − delivered − rebound, per family. | Per-session events; M2 gate: net saving > 0 per shaped family. |
| MET-016 | P0 | **Edit failure rate:** share of Edit/MultiEdit calls failing (e.g. `old_string` not found) with shaping on vs off. Primary fidelity signal for read shaping. | Paired M2 runs; gate: no significant increase. |
| MET-014 | P1 | **Initial-context overhead:** prompt size at each session's first request (system prompt + tool schemas + memory files), tracked as a separate token sink that tool-output reduction doesn't address. | `aaes mine` metric; trend per agent version. |

**Definitions:**
- Direct tool tokens avoided = tokenizer(raw result) − tokenizer(delivered result).
- Net observed API reduction = baseline total API tokens − optimized total API tokens for comparable tasks.
- Dollar savings = baseline provider cost − optimized provider cost − measured sidecar compute/operations cost.
- **Cache-aware cost of a tool result** of T tokens kept in context for k later turns ≈ `T · (p_cache_write + k · p_cache_read)` (or `T · p_input · (1 + k)` without caching). Savings scale with k, so occupancy (MET-011) matters as much as size.

Don't infer billed savings solely from output reduction. Prompt caching, retries, conversation history and provider token accounting complicate attribution.

## 5. Self-improvement requirements
| ID | Priority | Requirement | Acceptance evidence |
|---|---|---|---|
| IMP-001 | P0 | Record per-strategy results and categorized failure outcomes, per authorized scope. | Reproducible measurement events. |
| IMP-002 | P0 | Maintain immutable policies (versioned config per command family), versions, and rollback path. | Rollback replay. |
| IMP-003 | P0 | Evaluate candidate policies against fixed security/fidelity suites and representative workloads (offline replay). | Test reports and hard gates. |
| IMP-004 | **P2** (was P1) | Run challenger policies in shadow mode before any active rollout. *Gated on an installed base that produces enough signal.* | Main-agent output unchanged during shadow runs. |
| IMP-005 | **P2** (was P1) | Auto-adjust bounded, low-risk numeric thresholds only after passing evaluation gates. *Same gate as IMP-004.* | Versioned signed policy transition log. |
| IMP-006 | **P2** (was P1) | Automatically disable/roll back policy on measured regression thresholds. *Same gate as IMP-004.* | Triggered rollback experiments. |
| IMP-007 | P2 | Propose new parsers, adapters or code changes, but require review to integrate. | No unreviewed executable modification. |
| IMP-008 | P0 | No raw private session data used across sessions for policy learning by default; aggregate numerical statistics must not permit private data disclosure. | Learning-scope authorization tests. |

## 6. Nonfunctional targets (proposed, not observed)
- Baseline (LLM disabled): functional on CPU-only systems; measure binary, RSS, **cold start per invocation** (hook handlers are short-lived processes) and disk use; no hard universal hardware guarantee.
- Main-path no-op/pass-through overhead: target p95 <= 20 ms excluding underlying tool and I/O, **including process cold start**, on reference desktop hardware (Windows and Linux); benchmark, revise if unmet.
- **AAES's own start-up overhead**, measured as the delta over a trivial native executable in the same folder: target p95 ≤ 5 ms on every OS. No async runtime, no network and no heavy initialisation on this path. *Measured 2026-11-03 (Windows, managed laptop): `aaes --version` 90.7 ms vs a copied `hostname.exe` 97.1 ms in the same folder, so the delta is ≈ 0.*
- **End-to-end hook overhead** (the agent's view): target p95 ≤ 20 ms. On managed Windows machines, *process creation alone* for an unsigned executable outside System32 was measured at **50–100 ms** (endpoint security), so per-call process designs cannot meet this target there; see ADR-021.
- Cross-session unauthorized retrievals in mandatory negative tests: 0 successful.
- Critical diagnostic loss in qualification corpus: 0 tolerated. **This fixture-level fidelity gate is the primary release gate.** Real-world quality is measured independently.
- Task-level noninferiority on success with a predeclared margin of **3–5 percentage points** using a paired design. A 1 pp margin stays aspirational: at ~70% baseline success it needs on the order of 26k tasks per arm unpaired (see `03_BENCHMARK_PLAN.md`). If underpowered, report inconclusive.
- Achieve candidate goal >= 30% tool-result token reduction on the compressible workload subset, reporting all-task total results separately **and** the gap versus the prompt-only arm (A′).
- Delivered tokens never exceed raw tokens for any result (FUN-015).
- No additional outbound network required for the deterministic/local configuration.

## 7. Evidence contracts
**Internal envelope (store, metrics, recovery):** every optimized response records status, original tool identity, correlation ID, exact status/exit code where applicable, content class, normalized findings or exact excerpts, referenced source locations, original artifact handle, validation status, and explicit indicators for omission, truncation or stale evidence. Schema versions must be supported.

**Model-facing rendering (what the agent sees):** native-format text with exact diagnostics kept verbatim, plus a single footer, e.g.
`[aaes: 1,842→63 lines · 3 errors, 0 warnings kept · exit 1 · full: aaes show h_7Kx2Qm]`.
Omission/truncation/staleness indicators appear in the footer, never silently. The JSON envelope is never sent to the model by default.
