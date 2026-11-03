# Design Review & Recommendations — AAES v0.1

Status: Review of docs 01–04 v0.1. **Applied in v0.2** of docs 01–04 (see their change logs), with the threat model split out into `06_THREAT_MODEL.md`. Owner decisions in v0.3: public OSS, subscription and API billing, team and public audience, ADR-017 accepted (Rust), M-1 gate at 20% cost-weighted.

## 1. Understanding (one paragraph)
AAES is a local sidecar that sits between coding agents (Claude Code, Codex, MCP clients) and their tools. It runs the real tool unchanged, keeps the raw result in a session-private store, and hands the agent a smaller, validated rendering plus a handle to the full output. It is deterministic-first, with an optional local model, caching, retrieval and a gated policy-learning loop. The docs put correctness and isolation first, measure carefully (observed vs. causal savings) and fail safe (raw fallback). Those foundations are good. Keep them.

## 2. Overall verdict
The main risk is sequencing. Gateway auth, capability crypto, isolation tiers and the improvement lab all come before the project has shown savings on the interception surface that actually carries the tokens. In Claude Code and Codex that surface is the native Bash/Read/Grep tools, not MCP. The recommendations below reorder the work so value gets proven first. They also tighten the threat model and fix several gaps that would quietly erode the savings.

---

## 3. Recommendations (priority order)

### R1 — Build native hook adapters first and MCP second (changes ADR-002, FUN-012, M6 → M1)
Most tool-output tokens come from native shell and file tools, so an MCP-first proxy intercepts the smallest share. Current hook surfaces (verify against pinned versions, as open investigation #1 says):

| Agent | Input rewrite | Output replace | Other relevant |
|---|---|---|---|
| Claude Code | `PreToolUse` → `hookSpecificOutput.updatedInput` | `PostToolUse` → `updatedToolOutput` ("replaces the tool's result"). **Confirm it covers built-ins (Bash/Read/Grep) and not only MCP.** | `PreCompact`/`PostCompact`, `PostToolBatch`, `SessionStart.source` (`compact`/`resume`), `transcript_path`, and a separate **PowerShell tool on Windows** |
| Codex CLI | Not supported: `PreToolUse` can only deny | Not supported: `updatedMCPToolOutput` is in the schema but rejected | Hooks fail open; only `Bash` is hookable |

Implications:
- **Claude Code, mode (b), preferred:** `PostToolUse` output replacement. The tool runs natively, so the host keeps exit-code, permission and protocol semantics, and AAES only transforms. This is architecturally cleaner than wrapping processes and automatically satisfies FUN-001, FUN-003 and FUN-007.
- **Claude Code, mode (a), fallback:** `PreToolUse` command rewrite (`dotnet test` → `aaes run -- dotnet test`), the RTK pattern. The matcher must cover **both `Bash` and `PowerShell`** on Windows.
- **Codex:** instruction-based prefixing in AGENTS.md, plus a `PreToolUse` deny-with-reason that redirects (`"use: aaes run -- <cmd>"`), or an MCP shell server. Expect lower coverage and report it as such (the benchmark plan already requires this).

### R2 — Add M-1, "measure before building": mine the transcripts you already have
Claude Code transcripts (`~/.claude/projects/**/*.jsonl`) record per-message usage, including cache-creation and cache-read fields. Codex keeps session rollouts under `~/.codex/sessions`. A small offline analyzer over real history answers Q1 and part of Q2 before you write any gateway code:
- share of input tokens that come from tool results, by tool and by command family (dotnet build/test, ng/tsc/eslint, git, rg, Read)
- top 20 commands by cumulative tokens, and the size distribution of results (p50/p95/max)
- compactions per session, and how often the agent re-ran a command it had already run
- **Go/no-go gate:** if tool results are a small share of billed input, re-scope the project (for example toward context-occupancy and compaction reduction) before investing in M1+.

These parsers come straight out of this data: build them for the top five command families first.

### R3 — Right-size the threat model (SEC-001/002/007, ADR-003)
In the Standard tier, every session runs as the same OS user, and the agent has Bash. A prompt-injected agent in session A can read session B's SQLite or artifact files directly, so gateway capability checks do not stop a hostile co-resident agent. The docs half-acknowledge this in SEC-007. State it plainly:
- **Standard tier protects against:** accidental cross-session leakage (cache-key or filter bugs), guessable handles, and content in telemetry. It is **not** a security boundary against a hostile same-user agent.
- **P0 for Standard:** physical per-session namespaces, session-keyed cache, 128-bit random handles, content-free telemetry, and a negative test suite.
- **Move to the Hardened tier (P1):** principal-bound signed capabilities and `token_binding`. They only mean something when sessions run under different OS principals or sandboxes.
- Add `THREAT_MODEL.md` with an adversary table (buggy code / injected tool output / hostile sibling agent / other local user / admin) and the boundary that defends against each.

This shrinks M1 considerably with no loss of real security.

### R4 — The model-facing output must be compact text, not the `OptimizedToolResult` envelope (doc 01 §7, doc 02 contracts)
A JSON envelope with schema_version, correlation_id, handles, validation_status and so on **costs tokens**, and for small outputs it makes them bigger. Models also read native tool formats better than JSON.
- Keep the envelope internal (store and metrics). Render to the agent in the tool's native style plus a single footer line:
  `[aaes: 1,842→63 lines · 3 errors, 0 warnings kept · exit 1 · full: aaes show h_7Kx2Qm]`
- Preserve native diagnostic shapes verbatim (`src/Foo.cs(42,17): error CS0246: …`, `src/app/x.ts(10,5): error TS2322: …`).
- **Minimum-size threshold:** pass outputs under roughly 300–500 tokens through untouched, and track the footer and envelope overhead as a metric.

### R5 — Recovery path and re-run behavior (FUN-006, MET-002)
- Define the retrieval UX: `aaes show <handle> [--lines a-b] [--grep pat] [--tail n]`, plus an equivalent MCP tool. Retrieval has to be progressive, not "dump everything".
- Teach it once per session: Claude Code `SessionStart.additionalContext`, Codex `AGENTS.md`.
- **New metric: re-run-instead-of-retrieve rate.** If the agent re-runs `dotnet test` instead of fetching the handle, you lose minutes and tokens and may trigger side effects. This is the most likely way a compressor ends up losing money.

### R6 — Make the "already-seen" logic compaction-aware (M4, FUN-008)
After compaction (or `/clear`, or a resume with an expired cache), content the agent "already saw" is gone from its context. Replying "unchanged since turn N" would then starve it of the information. Reset the seen-set on `PreCompact`/`PostCompact` and on `SessionStart` with `source ∈ {compact, clear, resume}`. Codex has no compaction hook, so disable seen-dedupe there or derive it from the rollout file.

### R7 — Tokenizer reality (MET-001, open investigation #2)
- **Claude:** there is no public local tokenizer, so runtime counts are **estimates**. In the lab, calibrate a per-content-class estimator against the `count_tokens` API, which keeps runtime offline, and publish the error band. Ground truth comes from the transcript usage fields (R2).
- **OpenAI/Codex:** `o200k_base` runs locally (tiktoken).
- Update MET-001's wording so "estimate" is the expected state for Claude, not an exception.

### R8 — A cache-aware economics model (MET-003/005, ADR-007)
A tool result is billed once as a cache write and then re-read on **every later turn** until compaction or the session ends:
`cost(T) ≈ T · (p_cache_write + k · p_cache_read)`, where *k* is the number of turns it stays in context.
- New metrics: **context occupancy (token-turns)** and **compactions per task**.
- Fewer compactions and less context rot are a quality benefit independent of dollars, and may be the stronger story.

### R9 — The 1 pp noninferiority margin is statistically out of reach (doc 01 §6, doc 03 gates)
At baseline success p≈0.70, one-sided α=0.05 and 80% power, an unpaired design needs roughly **26,000 tasks per arm** for a 1 pp margin, versus about **1,040 per arm for 5 pp**. A paired McNemar design lowers this, depending on how often outcomes differ, but not to the point where 1 pp becomes affordable for agent tasks that cost dollars and minutes each.
- **Primary gate:** fixture-level fidelity (deterministic, cheap, zero tolerance). It already exists; make it the release gate.
- **Secondary:** paired task-level noninferiority at **3–5 pp**, with 1 pp labeled aspirational.
- **Task corpus that's cheap and relevant to your stack:** take historical bug-fix commits from your own .NET/Angular repos, revert each fix, and ask the agent to fix it with the repo's tests as the oracle. That gives you real tasks that no model has seen in training.

### R10 — Add a "prompt-only" baseline arm, A′ (doc 03 arms)
A′ is the unmodified agent plus CLAUDE.md/AGENTS.md instructions to use quiet flags (`dotnet build -v:q -clp:ErrorsOnly`, `dotnet test --logger "console;verbosity=minimal"`, `ng test --watch=false --no-progress`, `git --no-pager log --oneline -n 20`). If A′ captures most of the gain, you need to know before building M3–M5. Also confirm RTK's license for arm B: the repo checked is MIT, so the "licensing/feature tier" caveat may not apply.

### R11 — Gate the local LLM (M3) and policy learning (M5) on evidence
- A 0.6–1.7B model on CPU over 2–4k-token inputs takes far longer than the 20 ms budget, and small models are weak at faithful extraction. Proceed only if M-1 and M2 show a large residual bucket of **unstructured, uncompressed, large** outputs that deterministic parsers can't handle.
- For M5, start with per-command-family thresholds in versioned config plus offline replay. Build the shadow, canary and auto-rollback infrastructure once there is a real installed base generating signal. The ADR-008 principles stay; the machinery is deferred.

### R12 — Process model: start without a daemon
Hook handlers and `aaes run` are short-lived processes anyway. For M1–M2, use a **daemonless** design: each invocation opens its session's SQLite (WAL mode) and content-addressed store, does its work, and exits. That removes the gateway service, IPC, crash-reattach and cancellation-routing work (much of SEC-008 and the torture suite) until a long-lived component (inference, M3) actually needs one. Measure cold start against the 20 ms budget from day one.

### R13 — Correctness checklist for rewrite mode (FUN-003, FUN-007)
- exact exit-code passthrough; signal and Ctrl+C forwarding
- stdout/stderr **ordering**: capture through a merged pipe with sequence numbers when order matters
- **long-running or interactive detection** (`ng serve`, `dotnet watch`, `docker compose up`, `npm start`): always pass through, never buffer
- ANSI, progress-bar and spinner stripping; CRLF; UTF-16 console output from some Windows tools; code pages
- quoting differences between Git Bash, PowerShell and cmd.exe on Windows

### R14 — Secrets and retention (ADR-005, open investigation #6)
Raw artifacts will contain secrets: env dumps, connection strings, tokens in logs.
- Encrypt at rest with the OS keystore: DPAPI (CurrentUser) on Windows, Keychain on macOS, libsecret on Linux.
- **Default retention:** session end + 24 h, configurable, with a per-session disk quota and LRU eviction.
- **Redact before FTS indexing:** index the redacted text and keep the raw only encrypted. An FTS index full of plaintext secrets widens exposure.

### R15 — Platform and stack ADR (ADR-002)
The docs never name target operating systems. Make it explicit (likely Windows and Linux first, given the team). Rust is well justified: millisecond cold start, a single static binary, `tiktoken-rs`/`tokenizers`, llama.cpp bindings, and RTK as precedent. For a .NET-heavy team, **.NET 9/10 Native AOT** is a credible alternative: `Microsoft.Data.Sqlite` (bundled SQLite includes FTS5), `Microsoft.ML.Tokenizers` (tiktoken), ONNX Runtime GenAI. Its cold start is typically tens of ms, so the 20 ms budget is tighter; measure it. Record the trade-off as maintainability and team skills versus hot-path cost.

### R16 — Doc hygiene and missing pieces
- Define "AAES" and assign owners (doc 01 header).
- **Command classification registry** as data: `pure | read-only | mutating | long-running | interactive`, defaulting to non-cacheable and pass-through. This is what ADR-006 and data-flow step 3 depend on.
- **User controls:** `AAES_RAW=1`, per-command deny-list, `--raw`, a global kill switch, and `aaes init`/`aaes uninstall` that back up and restore agent settings.
- Distribution (single binary; winget/cargo/npm shim), SQLite schema migrations and versioning.

---

## 4. Proposed revised roadmap
| Milestone | Content | Exit |
|---|---|---|
| **M-1 Measure** | Transcript miner (Claude Code + Codex); token share by tool/command; compaction and re-run rates | Go/no-go with a quantified target list |
| **M0 Harness** | Fixtures for the top command families from M-1; arms A, A′, B; fix-commit-replay task corpus | Reproducible baselines |
| **M1 Vertical slice** | Claude Code PostToolUse (+ PreToolUse fallback, Bash + PowerShell); daemonless; deterministic parsers for top 5 families; raw store + `aaes show`; compaction-aware; Standard-tier namespacing; Windows + Linux | Fidelity goldens pass; measured reduction; no tool regressions |
| **M2 Evaluate** | Arm C vs A/A′/B; cache-aware economics; re-run-rate; occupancy | Net-positive on representative workload |
| **M3 Coverage** | Codex adapter (instructions + deny-redirect); MCP proxy | Coverage matrix measured |
| **M4 Memory** | FTS5, already-seen, exact cache for declared-pure commands | Fewer repeat calls, no stale or foreign evidence |
| **M5 Local model** *(gated)* | Only if residual unstructured bucket justifies it | Beats deterministic baseline within latency budget |
| **M6 Learning + Hardened tier** *(gated)* | Policy lab, shadow/canary; principal-bound capabilities; process isolation | Per existing gates |

## Sources
- Claude Code hooks reference: https://code.claude.com/docs/en/hooks
- Codex CLI hooks guide (third-party, Apr 2026): https://codex.danielvaughan.com/2026/04/15/codex-cli-hooks-complete-guide-events-policy-patterns/
- RTK (Claude Code PreToolUse rewrite pattern, MIT): https://github.com/spullara/rtk
