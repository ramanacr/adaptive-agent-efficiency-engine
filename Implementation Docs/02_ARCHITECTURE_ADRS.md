# Architecture and Architecture Decision Records — AAES v0.6

## 0. Change log
| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-11-03 | Initial draft. |
| 0.2 | 2026-11-03 | Applied design review (`05_DESIGN_REVIEW.md`). Reference architecture is now adapter-first: Claude Code hooks are primary and MCP is secondary (ADR-002 amended). Added ADR-011 (daemonless process model for M1–M2), ADR-012 (model-facing rendering), ADR-013 (tier-honest threat model, which amends ADR-003), ADR-014 (compaction-aware state), ADR-015 (secrets at rest and retention), ADR-016 (command classification registry), ADR-017 (runtime language re-confirmation, PROPOSED). ADR-008 and ADR-010 are now gated on evidence. |
| 0.3 | 2026-11-03 | Windows process-spawn measurement: ADR-011 amended, ADR-021 (resident HTTP-hook server on Windows) PROPOSED. **ADR-017 accepted: Rust.** ADR-002 clarified: hot path is synchronous (no Tokio), and Tokio is allowed only in the M5 daemon. Added ADR-018 (distribution and supply chain), ADR-019 (billing-mode-aware economics), ADR-020 (public-repo data hygiene). |
| 0.4 | 2026-11-03 | Added ADR-022 (large-file read shaping) and ADR-023 (search-result shaping) as the M1 targets, following M-1 evidence. ADR-001 parser priority now follows measured cost. |
| 0.5 | 2026-11-04 | M1 spike (`docs/spikes/M1-hook-spike.md`): **ADR-021 accepted** (HTTP hooks + resident `aaes serve` on Windows). ADR-002 hook table confirmed for Claude Code 2.1.233, with output shapes recorded. |
| 0.6 | 2026-11-04 | ADR-011 amended (M1 implementation): session state is JSON + files, with SQLite deferred to M4. ADR-021 implementation notes: a Windows handle-inheritance fix and `--port` in the ensure command. |

## Reference architecture
```
Claude Code session                 Codex session               MCP client
  | PostToolUse (updatedToolOutput)   | AGENTS.md prefix +          | MCP proxy (P1)
  | PreToolUse  (updatedInput →       | PreToolUse deny-redirect    |
  |   `aaes run -- <cmd>`) fallback   |  → `aaes run -- <cmd>`      |
  | SessionStart / Pre|PostCompact    |                             |
  v                                   v                             v
aaes hook / aaes run  (short-lived process per call — ADR-011; daemon only when M5 inference needs one)
  identity (tier-appropriate, ADR-013) -> correlation -> command registry lookup (ADR-016) -> mode selection
   |                                    |
   |                                    +--> metrics event writer (content-free)
   v
Adaptive policy controller (deterministic-first, versioned config per command family)
   |-- Pass-through (unknown / interactive / long-running / under size threshold)
   |-- Format parsers and invariant extractors
   |-- Session-private exact cache / version-aware invalidator (declared-pure only)
   |-- Local LLM semantic summarizer (optional, sandboxed, gated — ADR-010)
   |-- Session-private evidence retrieval: SQLite + FTS5 (redacted) + optional vectors
   |-- Validator -> original-response fallback
   |-- Renderer: native-format text + footer (ADR-012)
   v
Original tool execution: by the host agent (PostToolUse mode) or by `aaes run` (rewrite mode)
   |
   +--> immutable original evidence (per-session, encrypted at rest — ADR-015)

Recovery: `aaes show <handle> [--lines|--grep|--tail|--head]`   (+ MCP tool, P1)
Offline:  transcript miner (M-1) · improvement lab -> replay -> policy gate -> versioned release
```

## Data flow
1. The adapter identifies the session (tier-appropriate, ADR-013) and creates an invocation correlation ID. The tool request cannot choose arbitrary alternate session owners.
2. Authorization: in hook mode the host agent has already authorized the call, and AAES never widens permissions. In proxy/delegation modes the gateway authorizes before policy inference or caching. Unauthorized calls never reach the tool.
3. The command registry (ADR-016) classifies the call. Pre-execution optimization may deduplicate only registry-declared pure, fresh, read-only calls. Mutations are never replayed from cache. Interactive and long-running calls are streamed through untouched.
4. The original authorized tool executes (natively by the host, or by `aaes run`). Capture original response, status, provenance and byte/token metadata.
5. Classify content. Skip transformation below the size threshold. Apply the deterministic parser first. Invoke the optional local small model only when semantic benefit outweighs overhead and risk (gated, ADR-010).
6. Validate required invariants. If violated or uncertain, pass original tool output and record failure.
7. Render native-format compact text + footer with a session-scoped handle (ADR-012), never another session's artifact.
8. Log content-free measurement; raw evidence retained encrypted under the retention policy (ADR-015).

## ADR-001 — Deterministic first, model second (ACCEPT)
**Decision**: use parsers, schemas, command fingerprints and exact extractors before a model. Model is for relevance/routing and unstructured content under verification. **Why**: predictable correctness and latency. **Tradeoff**: more format-specific code. **v0.2 note**: parser priority comes from the M-1 transcript analysis (top command families by token volume), not from guesswork. **v0.4:** M-1 shows file reads and search dominate (ADR-022/023), while build/test output is ≈ 1%, so those renderers follow later.

## ADR-002 — Rust core and adapter boundary; native hooks first (ACCEPT, amended v0.2)
**Decision**: Rust binaries with a transport-agnostic core. **Hot-path binaries (`aaes hook`, `aaes run`) are synchronous**: no async runtime, minimal dependencies, lazy initialisation. Tokio is allowed only in the long-lived daemon (M5) and the MCP proxy. **Adapter order (amended):** (1) Claude Code native hooks, with `PostToolUse` output replacement preferred and `PreToolUse` command rewrite as fallback, covering both the Bash and PowerShell tools; (2) Codex via instructions + deny-redirect; (3) MCP proxy. **Why**: most tool-output tokens flow through native Bash/Read/Grep, not MCP, so MCP-first intercepts the smallest share. Output replacement keeps exit-code and permission semantics in the host. **Tradeoff**: hook APIs drift between agent versions, so pin versions, feature-detect, and fall back to pass-through. MCP interception does not imply interception of native agent tool calls, and vice versa. Language choice is re-confirmed in ADR-017.

Observed hook surfaces (confirmed for Claude Code 2.1.233 by the M1 spike, `docs/spikes/M1-hook-spike.md`; re-verify per pinned version):
| Agent | Rewrite input | Replace output | Lifecycle signals |
|---|---|---|---|
| Claude Code | `PreToolUse` → `updatedInput` | `PostToolUse` → `updatedToolOutput`: **confirmed for Read (`file.content`), Grep (`content`), Bash/PowerShell (`stdout`)**, including inside sub-agents. A value that doesn't match the shape is silently ignored | `SessionStart.source`, `PreCompact`/`PostCompact`, `PostToolBatch`, `transcript_path` |
| Codex CLI | No (deny only) | No (`updatedMCPToolOutput` rejected) | Bash only; hooks fail open |

## ADR-003 — Session physical namespaces (ACCEPT, scope clarified by ADR-013)
**Decision**: session-specific SQLite database and content-addressed artifact store for private evidence. Every lookup checks the session handle. Use separate OS permissions for distinct user principals and separate worker processes for hardened deployments. Shared model weights and executable code are allowed. Shared prompts, KV caches, embeddings and content-derived state are disallowed. **Why**: reduces accidental leakage from missing filters. **Tradeoff**: more file handles, migrations, disk overhead.

## ADR-004 — Embedded hybrid retrieval, vector optional (ACCEPT)
**Decision**: SQLite + FTS5 at launch, indexing **redacted** text only (ADR-015). Add a vector provider behind a trait only if paired retrieval tests show improvement in correct reuse per latency and memory. Do not allow a global cross-session ANN index. **Why**: exact names/paths and diagnostics favor lexical retrieval. **Tradeoff**: some semantic rephrases may be missed initially.

## ADR-005 — Raw evidence preservation and retrieval handles (ACCEPT)
**Decision**: persist raw response with retention policy and return an opaque, ≥128-bit random, session-scoped handle. Keep exact lines and source metadata when needed. Retrieval is progressive (`--lines/--grep/--tail/--head`), never "dump all" by default. **Why**: progressive disclosure and auditability. **Tradeoff**: disk and secret-handling responsibilities (see ADR-015).

## ADR-006 — Per-session cache, invalidation first (ACCEPT)
**Decision**: key by session + tool name/version + normalized args + environment/workspace fingerprint + relevant file/repo versions + authorization scope. Only commands the registry (ADR-016) declares pure/read-only are eligible. Time-varying external APIs default to noncacheable unless declared. **Why**: prevent stale and foreign hits. **Tradeoff**: lower initial hit rate. In active coding loops the workspace changes constantly, so expect low hit rates outside `git log`/unchanged reads, and don't build the roadmap on cache wins.

## ADR-007 — Metrics claims with evidence levels (ACCEPT)
**Decision**: three separate labels: (a) directly measured raw-vs-delivered tokens (estimated for Claude, exact for OpenAI tokenizers, always labeled), (b) observed provider/transcript usage, (c) causally estimated savings from paired baseline runs. Economics use the cache-aware model in `01_REQUIREMENTS.md` §4. **Why**: avoids misleading savings claims.

## ADR-008 — Self-improvement via controlled policies (ACCEPT; machinery gated v0.2)
**Decision**: the live engine does not mutate its binaries. Policies are versioned config per command family, evaluated by offline replay against frozen benchmarks. Shadow, canary and auto-rollback machinery is **deferred** until an installed base generates enough signal (IMP-004..006 are now P2). New source changes need human review. **Why**: limits regressions and security exposure without building unused infrastructure.

## ADR-009 — Permission-preserving bounded delegation (DEFER TO P1)
**Decision**: add later for narrowly scoped, read-only multi-tool work, with signed delegation context, max calls, max output, timeouts and explicit provenance. Do not silently execute write operations. **Why**: valuable but raises latency and safety risk.

## ADR-010 — Local model selection is empirical (ACCEPT; gated v0.2)
**Decision**: support no-model mode and compare 0.6B/1.7B-class permissively licensed models by actual routing, extraction fidelity, CPU latency and memory. Do not hardcode model size as a proxy for capability. Runtime must isolate inference states; verify cache/slot behavior. **Gate (v0.2):** start only if M-1/M2 show a material residual bucket of large, unstructured outputs that deterministic parsers can't reduce. CPU inference over 2–4k-token inputs far exceeds the 20 ms main-path budget, so the model must be off the default path or async. **Why**: machine diversity and uncertainty; avoid latency and fidelity costs without evidence of benefit.

## ADR-011 — Daemonless process model for M1–M2 (ACCEPT)
**Decision**: `aaes hook` and `aaes run` are short-lived processes. Each one opens its session's SQLite database (WAL mode) and content-addressed store, does its work and exits. A long-lived daemon (local IPC: named pipe on Windows, UDS on Unix) is introduced only when a component needs warm state, e.g. M5 inference. **Why**: hook handlers are per-call processes anyway, and this removes service lifecycle, IPC, reattach and cancellation-routing complexity until it's needed. **Tradeoff**: per-call cold start and SQLite open cost, which must be measured against the 20 ms p95 budget from day one. Per-session databases keep write contention negligible.
**Measured (2026-11-03):** on a managed Windows laptop, starting *any* unsigned executable outside System32 costs 50–100 ms (endpoint security), whatever the language. Daemonless therefore stays the default on Linux/macOS and the fallback on Windows. The preferred Windows path is ADR-021.
**Amended v0.6 (M1 implementation):** session state is a small per-session `state.json` (ask-twice counters and context epochs per main/sub-agent context) plus files for raw artifacts and `events.jsonl`. SQLite is deferred to M4, when full-text search needs it. In the resident server, state updates are serialised by a process mutex. In command mode, concurrent hooks for parallel tool calls can race on `state.json`; the worst case is an extra full read, never data loss of tool output.

## ADR-012 — Model-facing rendering is native text, not JSON (ACCEPT)
**Decision**: the `OptimizedToolResult` envelope is internal. The agent sees compact text in the tool's own diagnostic format with exact diagnostics verbatim, plus one footer line (counts, exit code, omissions, handle). Results below a minimum size (default ~400 tokens) pass through untouched. Recovery guidance is injected once per session (`SessionStart.additionalContext` / `AGENTS.md`), not repeated in every result. **Why**: a JSON envelope costs tokens, can make small outputs larger, and models read native tool formats better. **Tradeoff**: per-tool renderers to maintain.

## ADR-013 — Tier-honest threat model (ACCEPT; amends ADR-003 claims)
**Decision**: in the Standard tier all sessions run as the same OS user, and the agent has a shell, so a prompt-injected agent can read another session's store directly. The Standard tier therefore defends against **accidental** cross-session leakage (cache-key/filter bugs, guessable handles, content in telemetry). It is **not** a security boundary against a hostile same-user agent. Principal-bound signed capabilities (`token_binding`) belong to the Hardened tier, where sessions run under distinct OS principals or sandboxes, because only there are they enforceable. See `06_THREAT_MODEL.md`. **Why**: prevents overclaiming and keeps M1 small without losing real protection.

## ADR-014 — Compaction-aware session state (ACCEPT)
**Decision**: "already-seen" tracking and dedupe replies ("unchanged since …") are valid only while the content is still in the agent's context. Reset that state on `PreCompact`/`PostCompact` and on `SessionStart` with `source ∈ {compact, clear, resume}`. With no compaction signal (Codex), disable seen-based dedupe or derive it from the rollout file. **Why**: otherwise the agent is told it has content it no longer has.

## ADR-015 — Secrets at rest and retention (ACCEPT)
**Decision**: encrypt raw artifacts with an OS-protected key (DPAPI CurrentUser, Keychain, libsecret; documented file-permission fallback). Redact secret patterns before FTS/vector indexing; indexes never hold raw secrets. Default retention: session end + 24 h, configurable, with a per-session disk quota and LRU eviction. **Why**: raw tool output routinely contains env dumps, connection strings and tokens. **Tradeoff**: key management and an "expired handle" UX.

## ADR-016 — Command classification registry (ACCEPT)
**Decision**: a versioned data file maps command patterns to `pure | read-only | mutating | long-running | interactive | unknown`, plus the parser/renderer ID and size threshold. `unknown` means pass-through and non-cacheable. **Why**: cache eligibility, streaming decisions and parser routing all depend on it. **Tradeoff**: needs curation, and M-1 data decides which entries come first.

## ADR-017 — Runtime language: Rust (ACCEPTED v0.3)
**Context**: ADR-002 chose Rust. The team is primarily .NET/Angular.
**Options**: (a) **Rust**: ms-level cold start, single static binary, `tiktoken-rs`/`tokenizers`, llama.cpp bindings, RTK precedent. Costs: team ramp-up and maintenance. (b) **.NET 9/10 Native AOT**: team familiarity, `Microsoft.Data.Sqlite` (bundled SQLite with FTS5), `Microsoft.ML.Tokenizers` (tiktoken), ONNX Runtime GenAI. Costs: cold start typically tens of ms, which strains the 20 ms budget, and AOT trimming constraints.
**Decision (v0.3)**: **Rust.** The product requirement is "superfast": a handler process starts on every agent tool call, and the project is public and team-distributed. Rust gives the lowest cold start and memory with no runtime to install, static single-file binaries for every target, and the ecosystem the peer tool (RTK) already uses. .NET Native AOT was viable but carries a structurally higher start-up cost and AOT/trimming constraints for no product benefit.
**Consequences**: contributors need Rust. The cold-start harness becomes a permanent CI benchmark rather than a one-off spike (NFR: ≤5 ms Linux / ≤10 ms Windows p95). First code: `aaes mine` (M-1), 1 MB release binary.

## ADR-018 — Distribution and supply chain (ACCEPT v0.3)
**Decision**: ship signed, single-file binaries via GitHub Releases for win-x64/arm64, linux-x64/arm64 (musl static) and macOS universal, plus `cargo install`/`cargo binstall`; winget, Homebrew and Scoop follow at P1. Builds are locked and reproducible where feasible, with Sigstore provenance, Authenticode (Windows) and notarization (macOS) when certificates are available, and an SBOM per release. CI enforces `cargo-deny`/`cargo-audit` and a secret scan. `aaes init` needs no admin rights and backs up agent settings. **Why**: the binary runs inside every agent tool call on developer and team machines, so users must be able to verify it, and teams must be able to deploy it. **Tradeoff**: release-engineering effort and certificate costs.

## ADR-019 — Billing-mode-aware economics (ACCEPT v0.3)
**Decision**: the primary unit is **cost-weighted input units** (tokens × relative API price: uncached 1, 5m write 1.25, 1h write 2, read 0.1; versioned). For API sessions, units convert to dollars with a versioned price table. For subscription sessions, the same units are reported as a **labelled proxy** for usage-limit consumption, alongside occupancy and compactions, because plans don't publish limits in tokens. Billing mode is `api | subscription | unknown` (config). **Why**: one engine and one metric pipeline serve both audiences without implying dollar savings to subscribers. **Tradeoff**: the subscription proxy is an assumption and is labelled everywhere.

## ADR-020 — Public-repo data hygiene (ACCEPT v0.3)
**Decision**: the repository contains only synthetic fixtures or fixtures from public OSS repositories with provenance and license recorded (`tests/fixtures/PROVENANCE.md`). Real transcripts, proprietary code and raw tool output stay in git-ignored `bench-private/`. Reports from `aaes mine` are aggregate-only and support `--anonymize` before sharing. CI runs a secret scan on every push. **Why**: the project is public, while the most valuable benchmark data (team repos, transcripts) is private. **Tradeoff**: published benchmark results must rely on the public corpus; private results can be summarised but not reproduced publicly.

## ADR-021 — Resident hook server for Windows (ACCEPTED v0.5)
**Context**: ADR-011 assumed per-call process cost is small. Measurement shows a 50–100 ms floor per process start on managed Windows machines, caused by endpoint security rather than AAES. Rewrite mode (`aaes run -- <cmd>`) pays it on top of the tool's own process.
**Proposal**: on Windows, run a lightweight per-user resident process (`aaes serve`: synchronous, a small HTTP/1.1 server, no Tokio) bound to `127.0.0.1`, and register Claude Code hooks as **`type: "http"`** so no process is spawned per call. Requirements:
- authentication with a per-user random token in a request header, so other local users can't use the port;
- started on demand by `aaes init` or at login, with health checks;
- automatic fallback to command hooks when it isn't running.

Prefer `PostToolUse` output replacement over rewrite mode, since that avoids the extra process entirely.
**Verified (M1 spike, 2026-11-04, Claude Code 2.1.233, Windows):**
- HTTP hooks support bearer-token headers via `allowedEnvVars`.
- Measured overhead per tool call, p50: no hook 14 ms; **command hook 376 ms (+362)**; **HTTP hook 85 ms (+71)**.
- Default on Windows: HTTP hooks to `aaes serve`, with command hooks as fallback.
- Hook matcher limited to shaped tools (`Read|Grep|Bash|PowerShell`).

Still open:
- keep-alive and other ways to cut the remaining ~70 ms;
- Linux/macOS default (measure first);
- memory footprint of the resident process;
- whether signing or install location lowers the spawn floor for the fallback.

**Implementation notes (M1, v0.6):**
- `SessionStart` runs `aaes serve --ensure --port <p>`. It applies the event, starts the server detached if the health check fails, and prints nothing.
- On Windows, the std handles are made non-inheritable before spawning. Otherwise the server inherits the hook's pipes and Claude Code waits for them, stalling the session (found in the contract test).
- Token: random, stored in the user-private data folder and in the settings `env` block, which Claude Code interpolates into the HTTP-hook header (verified).
- Measured: **+27 ms p50** per hooked call with keep-alive.

**Tradeoff**: a long-lived component arrives earlier than M5 (lifecycle, reattach and late-reply rules from SEC-008 now apply), plus localhost exposure. The gain is removing roughly 50–100 ms per tool call on the platforms most of the target teams use.

## ADR-022 — Large-file read shaping: outline + head + exact ranges (ACCEPT v0.4)
**Context (M-1):** file-content reads are ≈ 18% of cost-weighted input (Read tool 12.5%, `sed`/`cat` ≈ 5%). Large whole-file reads (> 2k tokens) are 6% on their own, and 70% of them were never edited later in the same context. Unchanged re-reads are negligible (0.1%), so read de-duplication isn't worth building.
**Decision:** for whole-file reads above a threshold, replace the delivered content with:
1. a header (`path · N lines · ~T tokens · showing outline + lines 1–H`);
2. a deterministic **structural outline** (types, members, functions, selectors, top-level keys, headings) with line ranges;
3. the first H lines verbatim in the host's line-numbered format;
4. a footer naming exact retrieval: `Read(offset, limit)` or `aaes show <handle> --lines a-b`.

The raw content is kept (ADR-005), so nothing becomes unreachable. **Ask twice, get full:** a repeated whole-file read of the same unchanged file in the same context passes through in full.
**Outline extractors:** line-oriented and dependency-light (no tree-sitter on the hot path), with one extractor per language family and a generic block fallback. Target well under 1 ms per 10k lines. tree-sitter can come behind a feature flag if accuracy demands it.
**Why:** saves the content the agent never needs while keeping exact text one call away. It is lossless in the ADR-001 sense, because nothing is paraphrased.
**Risks:**
- *Rebound*: the agent fetches most of the file anyway. Measured by MET-015, with a per-family kill switch.
- *Edit failures*: the agent guesses `old_string` without exact text. Measured by MET-016; the outline footer tells it to read the exact range before editing.
- Claude Code's own "file was read" bookkeeping for Edit is unaffected, because the Read call itself still executes. Verify in the M1 spike.

## ADR-023 — Search-result shaping (ACCEPT v0.4)
**Decision:** large search outputs (Grep tool content mode, `grep`/`rg`/`git grep`/`Select-String`) are regrouped by file:
- each path printed once;
- line numbers and match text verbatim;
- matches per file capped, with an exact `+N more` count and a handle;
- grand totals kept exact.

No reordering within a file; the files' order follows the tool's. **Why:** ≈ 3.7% of input, and repeated path prefixes and over-broad matches are pure overhead. **Tradeoff:** a capped file may hide the relevant match, so the cap is generous and per-file counts make the gap visible.

## Contracts and pseudo-schemas
### Session capability
```
SessionPrincipal {
  principal_id, agent_instance_id, session_id, workspace_scope,
  granted_tool_scopes, created_at, expires_at,
  token_binding?            // Hardened tier only (ADR-013)
}
```
Never accept a session_id without validating it against the tier's mechanism. Distinguish user identity, agent process identity, conversation/session identity and task identity.

### Invocation
```
ToolInvocation {
  correlation_id, authenticated_session_ref, tool_id, tool_version,
  arguments, permission_context, command_class, task_hint?, deadline, cache_policy
}
```
Tool outputs are untrusted, including content from a repository or search index.

### Optimized result (internal envelope — not sent to the model, ADR-012)
```
OptimizedToolResult {
  schema_version, correlation_id, tool_id, status, exit_code?,
  content_type, findings[], source_refs[], exact_diagnostics[],
  original_artifact_handle, omitted_sections[], validation_status,
  freshness, tokenizer_id, estimate_kind, raw_tokens?, delivered_tokens?,
  rendered_text, footer
}
```
Original errors and protocol semantics must be preserved for any tool contract requiring them.

### Command registry entry (ADR-016)
```
CommandClass {
  pattern, shells[bash|powershell|cmd], class, parser_id?, renderer_id?,
  min_tokens_to_transform, cacheable, registry_version
}
```

### Metrics event
```
EfficiencyEvent {
  session_ref, invocation_id, timestamp, agent_adapter, tool_class, command_class,
  optimizer_strategy_id, tokenizer_id, estimate_kind,
  raw_tokens, delivered_tokens, footer_tokens, local_inference_tokens?,
  latency_ms, cold_start_ms, local_cpu_ms?, cache_hit, validation_status,
  passthrough_reason?, raw_recovery_requested, rerun_of_handle?, outcome_signal?,
  compaction_epoch, policy_version
}
```
Prefer pseudonymous IDs. No prompts, code, secrets or paths in ordinary aggregate telemetry.

## Isolation tiers
- **Standard single-user**: physical per-session SQLite and encrypted artifacts. Every AAES interface checks the session handle. Defends against accidental leakage only; it is **not** a boundary against a hostile same-user agent with shell access (ADR-013). When inference is enabled: a shared local inference process with proven state clearing and no cross-session prefix reuse.
- **Hardened**: per-principal process identity, principal-bound capabilities, sandboxed tool workers, isolated inference workers or processes, and restrictive filesystem ACLs.
- **High-assurance**: separate process/VM/container/security boundary per session where the threat model includes hostile co-resident agents. A claim of "utmost" isolation must state which boundary is enforced; user-mode code cannot guarantee isolation from OS administrators or unrestricted sibling processes.

## State machine
Session: CREATED -> ACTIVE -> IDLE -> SUSPENDED -> CLOSED -> EXPIRED. Closed sessions reject new tool calls. Stale queued responses are discarded. Resume requires identity reauthentication, grants no access outside its original namespace, and **resets seen-state (ADR-014)**. Each invocation: RECEIVED -> AUTHORIZED -> EXECUTED -> TRANSFORMED -> VERIFIED -> RENDERED -> DELIVERED; fallback on validation error; DENIED on authorization failure; PASSED_THROUGH for unknown/interactive/small results.

## Risks and mitigations
- Prompt injection in tool output: data/instruction separation; no autonomous privileges.
- Wrong summarization: deterministic invariant checks, raw fallback and recovery.
- **Agent re-runs instead of retrieving** (cost and side effects): once-per-session recovery guidance, explicit footer, MET-010 monitoring, and loosening compression for command families with high re-run rates.
- **Stale "already-seen" after compaction**: ADR-014.
- Stale cached results: dependency hashes, versions, TTL and invalidation events.
- Context leak through embeddings or inference caches: per-session partitions; no shared context-derived states.
- **Secrets persisted in artifacts/indexes**: ADR-015.
- Model overhead exceeds token savings: bypass when expected benefit low; measure p95 latency; gate per ADR-010.
- **Hook API drift**: version pinning, feature detection, pass-through on unknown versions.
- Adapter limitations: capability matrix, version pinning and graceful pass-through.
- **Overclaiming isolation**: tier-specific wording (ADR-013, SEC-011).
- **Compromised dependency or release artifact**: ADR-018 (signing, provenance, cargo-deny, minimal hot-path deps).
- **Private data committed to the public repo**: ADR-020 (git-ignored private corpus, secret scan, provenance file).
- Provider caching changes true economics: reconcile usage; use the cache-aware cost model; don't equate text compression with money saved.
