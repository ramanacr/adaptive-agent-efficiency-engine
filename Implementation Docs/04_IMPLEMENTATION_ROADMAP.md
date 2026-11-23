# Implementation Roadmap — AAES v1.7

This is a build sequence, not a promise of timing. Each milestone has objective exit criteria.

## 0. Change log
| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-11-03 | Initial draft (M0–M6). |
| 0.2 | 2026-11-03 | Re-sequenced per design review (`05_DESIGN_REVIEW.md`): added M-1 Measure; the first vertical slice targets Claude Code native hooks instead of MCP; daemonless M1 (ADR-011); Standard-tier scope (ADR-013); MCP/Codex moved to M3; local model (M5) and policy learning + Hardened tier (M6) gated on evidence. Added a language spike (ADR-017) to M0. |
| 0.3 | 2026-11-03 | Rust accepted, so the M0 spike becomes a permanent cold-start CI benchmark. M-1 tool implemented (`aaes mine`). Release pipeline added to M1 (signed binaries) and team policy to M3. Public/private corpus split. Repository layout reflects the real workspace. |
| 0.4 | 2026-11-03 | **M1 re-targeted to read and search shaping** (ADR-022/023) on M-1 evidence. Build/test parsers moved to M3. Sub-agent hook coverage and rebound/Edit-failure metrics added to M1/M2. |
| 0.5 | 2026-11-04 | M1 spike done (`docs/spikes/M1-hook-spike.md`): all four assumptions confirmed; ADR-021 accepted, so `aaes serve` (HTTP hooks) is part of M1 on Windows. |
| 0.6 | 2026-11-04 | **M1 increment 1 delivered** and contract-tested with Claude Code (`docs/spikes/M1-contract-test.md`). Status markers added to M1; repository layout updated to the real crates. |
| 0.7 | 2026-11-05 | Added **M2b, web-search admission control** (ADR-024, FUN-027), gated on a new M-1 measurement (MET-017 baseline). |
| 0.8 | 2026-11-14 | M2 pilot ran (240 runs). Reliability and latency results are clean; the efficiency result is **inconclusive** because the fix-commit-replay corpus almost never produces a shapeable tool result (3 shaped results in 60 arm-C runs). The powered study needs a corpus of large-output tasks and a shaping-coverage pre-check. Details in `03_BENCHMARK_PLAN.md`. |
| 0.9 | 2026-11-15 | Large-output corpus built (ESLint, 10 verified tasks) and probed. Weighted by bytes, as the gate is defined, the corpus **passes at 48.1%**. The probe found that **72.5% of tool-result bytes come from `Bash`**, outside the M1 shapers, but that output is already small and mostly piped, so it does not raise the priority of the M3 parsers. It also found that a short run cannot show the cache-weighted saving. |
| 1.0 | 2026-11-15 | Powered study design settled: **two tracks**. Track A reports direct savings from the short fix-replay runs, with the limitation pre-registered. Track B runs **chain tasks** - one long session over many bugs from a single base - so the cache weighting can accumulate. `bench/pilot/make_chain.py` builds and verifies a chain; the first is `eslint-chain-10`. |
| 1.1 | 2026-11-16 | **Track A ran and the coverage gate failed** (25.2% of shapeable bytes, 63 of 120 runs, stopped on the budget cap). The null C-shadow arm moved +23.5% against A, so the noise floor on cost-weighted input is about a quarter of the mean at this run length. Remaining runs not bought. |
| 1.2 | 2026-11-16 | **The chain task works.** One run: 131 turns, 10 of 10 bugs fixed, $5.78, coverage 43.8% (passes), and a shaped result re-read 61 to 112 times, so the cache weighting is measurable at last. It also showed **read shaping failing the rebound gate** (1.6 follow-ups per shaped result, 66% of the saving given back), while search shaping gives back none - which supports the shipped default of read shaping off. |
| 1.3 | 2026-11-16 | Traced the read rebounds. **Corrects v1.2**: read shaping is net **+27%** in delivered bytes, not self-cancelling. The defect is in the gate: counting follow-up calls prefers the shaped read that loses bytes over the one that saves 35%. A replacement is proposed in `03_BENCHMARK_PLAN.md` and not yet applied. |
| 1.4 | 2026-11-16 | **Gate decided: MET-015 stands.** The v1.3 proposal is withdrawn. A follow-up costs a whole request (12,458 cost-weighted units measured), which makes read shaping net **-64,144** units rather than positive, and puts break-even at **0.99** follow-ups per shaped read against a gate pre-registered at 1. |
| 1.5 | 2026-11-16 | Ran the chain in **C-shadow**. Ranged re-reading is the agent's baseline habit (**45.9%** of read events in C against **46.2%** in C-shadow), so MET-015 charges the shaper for behaviour it does not cause and the **-64,144** figure is an upper-bound end of a range. The read verdict is **open**, not negative. Thresholds stand; the rebound measurement needs a shadow baseline. |
| 1.6 | 2026-11-23 | The C / C-shadow chain pair **replicates** (C 24.4% below C-shadow). The arm-A run exposed **model drift** (the `sonnet` alias moved to Sonnet 5.5 after a Claude Code auto-update); the harness now pins and records the served model. On Sonnet 5.5 the coverage gate **fails at 0.0%** because the agent reads and edits only through `Bash`. **Shell file-read shaping moves ahead of Track B.** |
| 1.7 | 2026-11-23 | **Corrects v1.6.** Shell file-read shaping is withdrawn as the next step. No tool result in the pinned Sonnet 5.5 series exceeds 10 KB, every file read is a narrow `sed -n` range, and whole-file shell reads are already shaped. The shapers have nothing to act on at this stack; test-runner output is the only candidate source seen. |

## M-1 — Measure before building  *(done — GO)*
- ✅ `aaes mine` (FUN-022): parallel, read-only analysis of Claude Code and Codex history with aggregate content-free output, `--probe` structure check, `--anonymize`, JSON + Markdown, and the go/no-go verdict at the 20% gate.
- ✅ Run on the maintainer's machine (2026-11-03): **verdict GO** with bytes/token calibrated from transcripts. Context-lever metrics added (unchanged re-reads, large whole-file reads, shell file reads, sub-agent split). Detailed numbers are kept privately (ADR-020).
- ✅ Calibration validated against the `count_tokens` API (`tools/calibration-check`, 60 samples across 5 models): the transcript-derived estimate is within ~5% of API counts and slightly over-counts tokens, so it's conservative for savings claims. Verdict unchanged (GO).
- ☐ Codex: re-run once Codex sessions exist locally (`--probe` first).
- ✅ Web-search baseline (MET-017): `aaes mine` now ends with a web-search section: searches (and the share in sub-agents), exact and reordered duplicates within 30 min, duplicates within 10 s (single-flight candidates), bursts (> 3 within 2 s, all sessions), upstream rate-limit errors per 100 searches, and the same agent's searches in the 60 s after its rate-limit error. Counts only; query text is hashed in memory and never written. Format finding: a failed `WebSearch` is not an `is_error` result. The failure appears inside the normal result text as `Web search error: <code>` (`too_many_requests` for the rate limit). Maintainer's numbers are kept privately (ADR-020); the M2b gate decision is pending.
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
- ✅ **Spike** (`docs/spikes/M1-hook-spike.md`):
  - `updatedToolOutput` replaces Read/Grep/Bash/PowerShell output;
  - hooks fire inside sub-agents;
  - shaped Read still allows Edit;
  - Windows hook overhead: command +362 ms, HTTP +71 ms per call (p50).
- **`aaes serve`** (ADR-021): per-user resident HTTP hook server on 127.0.0.1 with token auth, started by `aaes init`/at login, with health check and command-hook fallback. Synchronous and lightweight. Measure and cut the remaining HTTP overhead (keep-alive).
- **Per-version contract test**: replaced output must reach the model (shape mismatches are silently ignored).
- `aaes init` / `aaes uninstall` that install and remove hooks with settings backup (FUN-020). Matcher limited to `Read|Grep|Bash|PowerShell`.
- **Read shaping** (FUN-025, ADR-022): threshold, outline extractors (C#, TS/JS, HTML, SCSS/CSS, JSON/YAML, Markdown, generic), head lines, footer, "ask twice, get full". Covers the Read tool plus shell `cat`/`type`/`Get-Content` single-file reads.
- **Search shaping** (FUN-026, ADR-023): Grep tool content mode, `grep`/`rg`/`git grep`/`Select-String`.
- Command registry with pass-through defaults; long-running/interactive streaming; Windows execution fidelity (FUN-017, FUN-018).
- Per-session SQLite (WAL) + encrypted content-addressed raw store; 128-bit handles; retention/quota; redaction (SEC-012..014).
- `aaes show <handle> [--lines|--grep|--tail|--head]`; once-per-session guidance via `SessionStart.additionalContext` (FUN-016).
- Compaction-aware state reset (FUN-019).
- Standard-tier namespacing and negative tests (SEC-001 Standard, SEC-002, SEC-004).
- Events for rebound (MET-015) and Edit failures (MET-016). Per-session CLI savings report with estimate labels, in both billing views (MET-013).
- Release pipeline v0: signed GitHub Release binaries for Windows/Linux/macOS, SBOM, `cargo-deny` gate (FUN-023, SEC-015).

**Status (increment 1, 2026-11-04):**
- ✅ `aaes init` / `uninstall` (settings backup, marker-based removal, idempotent)
- ✅ `aaes serve` (HTTP, token, keep-alive, health/shutdown, retention sweep), auto-started by the `SessionStart` hook (hook input read with a 2 s bound, so a caller that leaves stdin open cannot hang session start)
- ✅ Read shaping: head + complete outline with ranges; ranged pass-through; ask-twice per context; sub-agent contexts; compaction/clear/resume reset. Shell `cat`/`type`/`Get-Content` too.
- ✅ Search shaping (Grep tool, grep/rg/git grep/Select-String): grouped, relative paths, exact totals, capped lines kept behind a handle; `aaes show`
- ✅ Event log + `aaes report` (saved, pass-through, rebound); kill switch (`aaes disable`, `AAES_RAW=1`)
- ✅ Diagnostic log + `aaes logs` (decisions with timings, auth/parse failures, schema-drift warnings, caught panics fail open); once-per-session notice via `SessionStart.additionalContext` (FUN-016)
- ✅ Dogfood trial on the maintainer's machine, 2026-11-04 to 2026-11-07 (RTK and jCodeMunch temporarily disabled for an isolated measurement; CodeGraph and tokensave kept). MET-015 verdict: **search shaping passes** (net saving, no follow-ups); **read shaping fails** (net token loss, more than one follow-up per shaped read, almost all in sub-agents, which read the rest of the file within seconds). MET-016 showed no Edit-fidelity harm. A main-context-only slice was too small to judge.
- ✅ Read shaping is **off by default** for M1 (`read_shaping: false`); `config.json` can turn it on for experiments. Search shaping stays on. Revisit with a context- or file-type-aware policy after M1.
- ✅ Contract test with Claude Code 2.1.233: 5 scenarios pass. +27 ms p50 per hooked call; ≈ 70% saving on a 13k-token read; rebound 96 bytes on read-then-edit.
- ✅ SEC-012 encryption at rest: one 256-bit key per data folder (`artifact-key`), wrapped by DPAPI CurrentUser on Windows; 0600 file fallback on macOS/Linux until Keychain/libsecret. Artifacts sealed with ChaCha20-Poly1305, handle as associated data. Lost or foreign key: new key, older handles read as expired. Pre-encryption plaintext artifacts deleted by the sweep.
- ✅ SEC-013 for M1 (no FTS/vector index yet): canary test proves a secret in tool output, a search pattern or a command never reaches the log, the event log, any file in the data folder, or the text that replaces an omitted region. Redaction before indexing lands with the M3 index.
- ✅ SEC-012 macOS/Linux keystores: the key moves into the login Keychain (macOS: created by `/usr/bin/security -i` with an any-application access list so upgrades never prompt; native Security-framework read with user interaction off, `security` read as the fallback) or the kernel user keyring (Linux: `keyctl`, works headless and in WSL 2; dropped at reboot, then a new key). `artifact-key` holds only the entry id. 0600 file fallback where no keystore is usable, or with `AAES_KEY_STORE=file`; a plain key file moves into the keystore when one becomes usable. A keystore that cannot answer keeps the key and fails sealing (output passes through). libsecret not used: its D-Bus stack breaks the static musl build and needs a desktop session.
- ✅ MET-016 Edit failure rate in `aaes report`, from the Claude Code transcripts of AAES sessions: Edit/MultiEdit calls made while AAES was active, failure class, and a split by "file shown shaped earlier in the session" vs other files. Not from hooks: Claude Code 2.1.233 fires no `PostToolUseFailure` for `old_string` validation errors, which are nearly all Edit failures.
- ✅ Repo-index split: each event records whether the working folder is in a CodeGraph-indexed repo (`.codegraph/codegraph.db`), and `aaes report` splits calls and savings by it. Agents told to prefer CodeGraph make fewer shapeable calls there, so trial savings are read per group.
- ✅ Trial fixes (2026-11-07): single-file search output (`12:text`, no paths) is shaped as one group, labelled with the Grep tool's path. A follow-up (MET-015) counts only within `followup_window_secs` (default 300) after the shaped read; later reads were inflating the follow-up count.
- ✅ Scripted, version-pinned contract test (`scripts/contract-test.ps1`, FUN-014): 6 scenarios against real headless Claude Code, hard checks from AAES events and transcripts, version pin in `tests/contract/claude-code.version`. First run on 2.1.233: 29/29 pass. Not in CI (needs a Claude Code login); run after each Claude Code update and before a release.
- ✅ Release pipeline v0 (FUN-023, SEC-015): tag `vX.Y.Z` builds Windows x64/arm64, Linux x64/arm64 (musl, static) and macOS universal; zip/tar.gz archives with `SHA256SUMS`, an SPDX SBOM and Sigstore build provenance (GitHub artifact attestations); a **draft** release for the owner to publish. `cargo-deny` gate in CI (`deny.toml`). Install scripts (`scripts/install.ps1`, `scripts/install.sh`): per-user, no admin, checksum check, provenance check when `gh` is signed in, PATH handling. `cargo binstall` metadata. First release `v0.1.0` published 2026-11-07; the published install scripts install it on Windows x64 (checksum and provenance verified) and Linux x64 (WSL Ubuntu, checksum verified).
- ☐ Code signing: Authenticode (Windows) and notarization (macOS). Signing may also lower the Windows process-start cost (investigation 10).
  - Windows: Certum open-source certificate (SimplySign cloud) ordered 2026-11-08, identity check pending. The `sign-windows` release job is in place and off until the secrets and `AAES_SIGN_WINDOWS` are set (CONTRIBUTING, "Windows code signing").
  - macOS: not planned yet (needs an Apple developer account).
- ✅ Fidelity goldens (2026-11-10): `crates/aaes-core/tests/golden.rs` over synthetic fixtures in `tests/fixtures/search` (13 cases: GNU grep, absolute paths, rg context, git grep with a binary notice, Select-String with CRLF, single-file `N:text`, chained mixed output, truncated output, UTF-8 BOM and non-Latin paths, paths outside the working folder, unparsed, small, low saving) and `tests/fixtures/read` (C#, Angular TypeScript, Markdown). Invariants per case: exact totals, every shown line verbatim, shown + omitted + other lines cover every raw line, other lines in original order, delivered smaller than raw, handle offered if and only if lines were omitted, outline ranges inside the file and starting on the named line; then a reviewed snapshot (`*.expected`).
- ✅ Delivered ≤ raw is enforced in search shaping for every result, capped or not (it was only implied for capped results). All shaped results in the dogfood logs complied.
- ✅ Concurrent-session isolation test: two sessions read the same files at the same time, through one shared runtime (`aaes serve`) and through separate runtimes on one data folder (one process per hook call). Read state, stored raw output and event logs stay per session. Known limit: two hook processes of the *same* session at the same time (parallel sub-agents, command-mode hooks on macOS/Linux) can lose one `state.json` update, so a repeat read may be shaped twice; HTTP mode (Windows default) serialises state updates.
- ✅ RTK coexistence (after the trial): no conflicts or AAES errors. RTK rewrites shell `grep` before it runs, and AAES still sees the original command and parses RTK's output. Most shell searches are then below the AAES threshold, so with RTK on, AAES savings come mainly from the Grep tool and from large shell results.
- ✅ Host-truncated shell output passes through (2026-11-10). When a Bash/PowerShell output is too large, Claude Code saves it to a file and passes the hook only a preview (`persistedOutputPath`). AAES used to shape the preview, so its "exact" totals covered only the preview and the host's pointer to the full file was lost. Found by the contract test, whose C6 prompt was also ambiguous (a full stop read as a second path) and is fixed.
- **M1 exit criteria met (2026-11-10)**: fidelity goldens, no tool regressions (contract test 29/29 on Claude Code 2.1.233), delivered ≤ raw, hook latency measured, concurrent sessions isolated, measurable reduction on shaped families (trial). The open items below are distribution and hardening.
- ✅ Clean Windows install test (Windows Sandbox, Windows PowerShell 5.1, no Visual C++ runtime), 2026-11-10. It found that the `v0.1.0` Windows binaries need `VCRUNTIME140.dll` and fail to start, without any message, on a machine without the Visual C++ Redistributable. Fixed: the C runtime is now linked statically (`.cargo/config.toml`, `+crt-static`), and `install.ps1` stops with a clear error when the installed binary does not start. With a local build of the fix, install, `init`, `serve --ensure` and `uninstall` all work in the sandbox. Needs a new release (`v0.1.1`).
- ✅ Clean-machine install test per OS (FUN-023 acceptance, 2026-11-11): every release archive is unpacked and tested on a fresh GitHub runner of its own platform before the draft release (Windows x64 and arm64, Linux x64 and arm64, macOS arm64 plus the x86_64 slice through Rosetta): checksum, no dynamic C runtime, version, init, a shaped hook call, `aaes show`, report, uninstall, key location and time (`scripts/clean-install-smoke.sh`).

**Exit**: read/search fidelity goldens pass (exact retrieval of every omitted range; exact match totals); no tool regressions; delivered ≤ raw for every result; hook latency measured; two concurrent sessions isolated; measurable direct reduction on shaped families.

## M2 — Evaluate
- Arm C vs A / A′ / B on the fix-commit-replay corpus and workload strata, paired, powered for a 3–5 pp margin.
- Cache-aware economics; re-run rate; occupancy; compactions; footer overhead.
  - ✅ First part (2026-11-10): `aaes report` weights each family's saving and follow-ups by cache price, from the number of later requests (until compaction) in the session transcript. Units: uncached-input-equivalent tokens, same weights as `aaes mine`.
- Tune per-family policies (versioned config) via offline replay.
  - ✅ Shadow mode (2026-11-10): `"shadow": ["read", "search"]` in `config.json` computes the shaped output and logs its size, but delivers the raw output; `aaes report` shows the would-be saving (an upper bound, no follow-ups). New renderers (M3) start in shadow mode before they are turned on.
  - ✅ Offline replay (2026-11-10): `aaes replay` runs the current shapers over past Claude Code tool results (`toolUseResult`, the hook's input shape) with the hook's repeat, ranged and host-truncated rules, and reports would-be savings per family and per project, cost-weighted. An upper bound (no follow-ups); it is the input for per-repo thresholds.
  - ✅ Per-repo overrides (2026-11-10): `repos` in `config.json` sets `read_shaping`, `shadow` and the shaping thresholds per repo root (closest root wins, unset keys inherit). Events carry the override name; `aaes report` shows a per-repo table with advice (keep / turn off or raise the threshold / try it on) from the cost-weighted net saving, and `aaes replay` applies the same overrides. Settings change only by the user; automatic adjustment is not planned until the advice is validated.
- ✅ Version support matrix (2026-11-11): `docs/COMPATIBILITY.md` (Claude Code 2.1.209–2.1.321 by observed payload shapes, 2.1.233 by the live contract test; platforms; MSRV 1.85 now checked in CI). Offline payload contract in CI (`tests/contract/payloads.json`). Found and fixed: Read results that Claude Code cut at its token cap (`truncatedByTokenCap`) now pass through.
- ✅ Tool-definition overhead (2026-11-11): ADR-025 accepted after its spike. `aaes mine --tools` measures the fixed request prefix from the exact `system`/`tools` arrays that Claude Code 2.1.304+ records (`api-request-shape`), plus the deferred-tool, MCP-instruction, skill and agent listings; splits its cache-aware cost by built-in tools, MCP server, plugin and listing; lists servers with no calls. Read-only advice; no request rewriting.

- ✅ M2 pilot (2026-11-14): 20 tasks x 4 arms (A, A', C, C-shadow) x 3 replications = 240 runs on pinned OSS snapshots, headless, fresh `CLAUDE_CONFIG_DIR` and `AAES_HOME` per run, randomized arm order. Runner and analyzer in `bench/pilot/`. Result: 100% success in every arm (psi = 0), **0 hook errors**, no stop-rule task, no compaction, C-shadow wall-clock -4 s against A. All efficiency intervals contain zero, and the session events show why: AAES shaped 3 of 103 shapeable tool results, because the agent reads narrow ranges and its greps return under the cap. **The pilot does not support an efficiency claim in either direction on this corpus.**
- ⏳ Before the powered study: build a corpus of large-output tasks (strata 2, 6 and 13) and gate it on a shaping-coverage pre-check of at least 30% of shapeable results in arm C.
- ✅ Large-output corpus and coverage gate (2026-11-15): ESLint added to the public corpus (2,363 files, 676k lines, 302 rule test files, oracle about 9 s), `run_pilot.py --prompt-style discover --read-shaping`, and `bench/pilot/coverage_check.py` as the pre-check. Probe on 3 tasks: byte coverage 0.0% -> 48.1%, one shaped read 6,204 -> 1,511 bytes, so the corpus **passes the gate** (the gate is weighted by bytes; counting results rejects every realistic corpus). Two limits found: `Bash` carries 72.5% of tool-result bytes and the M1 shapers never see it, though that output is already small (mean 1,363 bytes, 81% of commands piped); and a 7-turn run re-reads a tool result a handful of times against about 200 in a real session, so the cache-weighted saving a pilot run can show is about two orders of magnitude too small.
- ✅ Powered study design (2026-11-15): two tracks, because one run shape cannot answer both questions. **Track A** keeps the short fix-replay runs and reports direct savings only, with that limitation pre-registered. **Track B** adds the *chain task*: one base commit carries the failing tests of many fixes, the agent gets every bug report in one prompt, and the whole set is fixed in a single session, so the context accumulates and the cache weighting can be measured instead of assumed. `bench/pilot/make_chain.py` builds a chain from already-verified tasks and checks it: every source diff applies to the base, no two fixes share a test file, and every oracle fails on the base and passes with the fixes applied. First chain: `eslint-chain-10`, 10 bugs on base `619493987`, all three conditions verified.
- ✅ Track A ran (2026-11-16): ESLint corpus, discover prompt, read shaping on, arms A / A′ / C / C-shadow, 63 of 120 runs for $35.78 before the budget cap. 100% success in every arm, psi = 0, 0 hook errors, no compaction. **The coverage gate failed at 25.2%**, so the run supports no efficiency claim. The null C-shadow arm moved +23.5% against A, which measures the noise floor; C's +43.9% sits inside it. Coverage in the live arm is half the shadow arm's, because shaping makes the agent read narrow ranges afterwards and so removes its own opportunity. Details in `03_BENCHMARK_PLAN.md`.
- ✅ Chain run measured (2026-11-16): `eslint-chain-10`, arm C, one replication. 131 turns, 1,214 s, $5.78, **10 of 10 bugs fixed**, 0 hook errors, and **43.8% byte coverage, which passes the gate**. A shaped result is re-read 61.2 times (read) and 111.5 times (search) before compaction, against a handful in a Track A run, so the cache weighting is off its floor. Net saving 118,580 cost-weighted units, close to the entire input of a short run. Edits after a shaped result: 10 calls, 0 failures.
- ⚠️ **Read shaping fails MET-015 on this workload**: 1.6 follow-up calls per shaped result against a limit of one. Every follow-up is the agent re-reading the same file with a line range; `aaes show` was never called in 131 turns. Read shaping is nonetheless **net +27% in delivered bytes** (an earlier version of this line called it self-cancelling, which was wrong: 66% is a gross-to-net ratio). Search shaping draws no follow-ups and keeps all of its 83,060 units.
- ✅ **Gate decided (2026-11-16): MET-015 stands, the proposal is withdrawn.** The case for demoting the follow-up count was missing a cost: a follow-up is an extra request, and an extra request re-reads the whole context. Measured at **12,458 cost-weighted units per request**, charging each follow-up one request makes read shaping net **-64,144 units** on the chain run, not +27% as the byte view suggested. Break-even is **0.99 follow-ups per shaped read**; the gate was pre-registered at 1. Only the reporting changes: express both conditions as `saved - follow_up_units - follow_ups x units_per_request` so the threshold is derived per run instead of assumed.
- ✅ C-shadow chain run (2026-11-16): 150 turns, $7.36, 10 of 10 bugs. **Ranged re-reading is the agent's normal pattern, not a reaction to shaping**: 45.9% of read events in C against 46.2% in C-shadow, and 1.7 per repeated file against 2.0, so the attributable excess is **-0.3**. MET-015 has no baseline subtraction, so its rebound count is an upper bound on what shaping costs.
- ⚠️ **The read-shaping verdict is open, not negative.** The -64,144 units assumed full attribution. At the attribution this pair suggests, most of the gross 103,943 units stands. Neither end is claimed from one pair of sessions. Weak corroboration: arm C ran $5.78 in 131 turns against C-shadow's $7.36 in 150, but one run per arm sits well inside the noise floor.
- ✅ Chain pair replicated (2026-11-23): second C and C-shadow runs on Sonnet 5 / Claude Code 2.1.233 land within about 2% of the first. C is **24.4% below C-shadow**, all four runs 10 of 10 bugs, and ranged re-reads per repeated file are 1.71 against 1.70 pooled. Still no arm-A anchor on that stack.
- ✅ Harness pins the model (2026-11-23): the arm-A run had been served by Sonnet 5.5 under Claude Code 2.1.330, because `--model sonnet` is an alias and the agent auto-updated. `run_pilot.py` now records `served_models` and takes `--tag` for a named series; `analyze_pilot.py` refuses to mix agent versions or served models; `coverage_check.py` reads one series.
- ⚠️ **Sonnet 5.5 bypasses M1 shaping (2026-11-23).** Pinned series, A / C / C-shadow x 2, all 10 of 10 bugs, $4.00: **coverage 0.0%**. The agent made 0 `Read` calls and 1 `Grep` call across six runs; it reads with `sed -n`, edits with `python` and searches with `grep` inside `Bash`. The C -16.1% against A is spread, not saving: C delivered every byte unchanged. Model and agent version changed together, so which one moved the tool mix is not yet separated.
- ❌ ~~Shell file-read shaping as the precondition for further study~~ (withdrawn 2026-11-23). Checked before building: 261 tool results across six runs, none over 10 KB, largest 7.4 KB (`npx mocha`). Every file read is a `sed -n` range the agent chose, and whole-file `cat` / `type` / `Get-Content` is already shaped. Shaping a chosen range would force follow-ups (MET-015).
- ⏳ Find a shapeable source on the current stack before another efficiency series. Only candidate seen: test-runner output, repeated after each fix. Design question, not a measured result.
- ⏳ Track B should run C and C-shadow on the same chains, so the shadow-controlled rebound rate exists for every family rather than being reconstructed afterwards.
- ⏳ Read threshold: shaping starts at about 4,800 bytes, where the head and outline are not much smaller than the file, and both net-negative reads sat just above that line. This is the first public measurement of what the v0.5 benchmark entry suspected from local dogfooding. Five shaped reads in one run is a direction, not a calibration.
- ⏳ Track B proper: one chain gives no task-level interval, so more chains must be mined before the bootstrap the plan specifies can run. Budget from the measured figure: about $70 per chain across four arms and three replications.

**Exit**: net-positive on the representative workload *and* a material gain over A′, or a documented re-scope.

## M2b — Web-search admission control *(gated, ADR-024)*
**Entry gate**: the MET-017 baseline from `aaes mine` shows a material problem (WebSearch rate-limit errors, bursts or duplicate queries in real transcripts).

**Status (2026-11-05): gate not met; ADR-024 stays PROPOSED.** On the maintainer's transcripts, rate-limit errors and duplicate queries are rare. Searches do come in heavy bursts from parallel research sub-agents, and after a rate-limit error agents keep searching with reworded queries, so pacing and the breaker are the parts to revisit first. Re-run `aaes mine` after more research-heavy work, or when users report search rate limits.
- **Spike** on the pinned Claude Code version: hooks fire for `WebSearch` (main and sub-agents) with matching `tool_use_id`; where and how rate-limit errors surface; how a `deny` reason reaches the model (size, rendering, whether the model keeps searching after a cache-hit deny, whether it treats the reason as instructions).
- Build in order of risk, lowest first:
  1. **Adaptive rate + circuit breaker** (content-free): stops retry storms, the cheapest win.
  2. **Global pacing**: in-flight cap and token bucket with a bounded, FIFO wait; parked-request cap.
  3. **Session cache + single-flight**: only if spike items 3 and 4 pass.
  4. **`fallback_tool` redirect** in denial reasons.
- `aaes init` adds `WebSearch` to the matchers only when `web_search.enabled`; `aaes report` gains the MET-017 section.
- Replay tests: synthetic bursts from parallel sub-agents, reordered duplicate queries, rate-limit sequences, leader failure in single-flight, lease expiry, server restart.

**Exit**: upstream rate-limit errors per 100 searches fall on dogfood sessions with no rise in failed tasks or abandoned searches; Read/Grep hook latency unchanged while searches are held.

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
  aaes-cli/                # `aaes` binary: mine · init/uninstall · hook · serve · show · report · enable/disable
  aaes-core/               # ✅ pure: command families, outline extractors, read/search shaping (no I/O)
  aaes-runtime/            # ✅ hook protocol, session state, artifact store, events, HTTP server
  aaes-transcripts/        # ✅ transcript parsing + analysis (Claude Code, Codex)
  # planned, added when first needed (no empty crates):
  aaes-runner/             # `aaes run` rewrite mode (if needed beyond PostToolUse)
  aaes-memory/             # SQLite, FTS5, optional vector trait (M4)
  aaes-adapter-codex/, aaes-mcp/, aaes-inference/, aaes-improvement/   # M3–M6
spikes/hook-probe/         # M1 spike probe (not product code)
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

No LLM, no vectors. On Windows the handler runs inside `aaes serve` (ADR-021); elsewhere as a command hook until measured.

## Decision log / open technical investigations
1. Which agent hooks reliably cover shell/file/MCP calls for pinned Claude Code and Codex versions? **Answered for Claude Code 2.1.233 (M1 spike):** output replacement works for Read/Grep/Bash/PowerShell, including inside sub-agents. **Earlier findings (v0.2):** Claude Code exposes `PreToolUse.updatedInput`, `PostToolUse.updatedToolOutput`, `PreCompact`/`PostCompact`, `SessionStart.source`; confirm `updatedToolOutput` coverage for built-in tools. Codex hooks are deny-only, cannot replace output, cover Bash only, and fail open. Re-verify per pinned version.
2. Which tokenizer is authoritative per provider/model and which input usage fields are available? **Partial findings (v0.2):** no public local Claude tokenizer, so use a calibrated estimator; transcripts carry usage fields including cache-write/read.
3. What local model performs best for relevance and decision classification under CPU-only 4/8/16GB scenarios? *(Only if the M5 gate is met.)*
4. Is semantic retrieval gain worth an embedding model and index under session workloads?
5. Which OS sandbox primitives are required for the Hardened/High-assurance tiers, beyond session checks? See `06_THREAT_MODEL.md`.
6. ~~What data retention and disk quotas…~~ **Decided (ADR-015):** session end + 24 h default, per-session quota with LRU, encryption at rest, redacted indexes. Revisit with user feedback.
7. ~~Rust vs .NET Native AOT~~ **Decided (ADR-017): Rust.**
8. Codex rollout format: validate parser assumptions with `aaes mine --probe` on real rollouts before trusting Codex numbers.
9. Initial-context overhead (MET-014): if it dominates, is tool-schema/context trimming in scope? Decide after M-1.
10. **Windows process-spawn floor (measured 2026-11-03):** 50–100 ms for unsigned executables outside System32 on a managed laptop. AAES's own overhead ≈ 0. Investigate ADR-021 (HTTP hooks + resident `aaes serve`), the effect of signing and install location, and include the spawn floor in the M0 cold-start benchmark as a separate line.
11. **Web-search rate limits (2026-11-05):** can AAES reduce upstream `WebSearch` rate-limit errors without raising limits or making network calls? Proposed answer in ADR-024. The MET-017 baseline (2026-11-05) did not meet the entry gate, so it stays open and PROPOSED; re-measure later.

## Stop conditions
Stop optimization and fall back to raw tool result on failed validation, uncertain critical information, unknown structured schema, unknown command class, interactive/long-running command, result below size threshold, stale cache dependency, unavailable inference, unsupported hook version, or budget overrun. Deny execution (not pass through) when authorization fails. Never silently suppress command errors, authorization failures or agent-visible tool protocol errors.
