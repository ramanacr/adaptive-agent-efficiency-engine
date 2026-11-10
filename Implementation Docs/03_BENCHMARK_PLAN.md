# Benchmark and Evaluation Protocol — AAES v0.6

## 0. Change log
| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-11-03 | Initial draft. |
| 0.2 | 2026-11-03 | Applied design review (`05_DESIGN_REVIEW.md`). Added Phase 0 observational study from existing transcripts (R2), prompt-only arm A′ (R10), fix-commit-replay task corpus and sample-size table with a 3–5 pp noninferiority margin, fixture fidelity as the primary gate (R9), re-run/occupancy/compaction/footer metrics (R5, R8, R4), Windows/PowerShell and long-running strata (R13), compaction and same-user tests (R6, R3). |
| 0.3 | 2026-11-03 | Phase 0 gate fixed at **≥20% cost-weighted** and implemented as `aaes mine`. Results reported in both billing views (ADR-019). Benchmark corpus split into **public** (OSS repos, reproducible) and **private** (team repos, local only) per ADR-020. Added initial-context overhead metric. |
| 0.4 | 2026-11-03 | M1 evaluation focuses on read and search shaping: new stratum 13 (read-then-edit on large files), rebound and Edit-failure metrics and gates. Phase 0 result: GO (calibrated); calibration validated against `count_tokens` (`tools/calibration-check`): within ~5%. |
| 0.5 | 2026-11-04 | Read-rebound gate adds ≤ 1 follow-up call per shaped result (MET-015). Early local dogfooding suggests agents fetch most of smaller shaped files anyway, so the read threshold is being evaluated at a higher value (details in the private findings). |
| 0.6 | 2026-11-10 | Added the **M2 pilot protocol**: a small, runnable first study (headless runs, arms A / A′ / C / C-shadow, public corpus, pre-registered metrics, pilot to estimate ψ before the powered study). Cache-aware result cost now uses the same formula as `aaes report` and `aaes mine` (`k` later requests until compaction). Shadow mode added as a control arm. |

## Questions to answer
Q0 Where do tool-output tokens actually go today, by tool and command family? Q1 Does the sidecar reduce raw delivered tool tokens? Q2 Does that translate to provider-billed and net monetary savings? Q3 Does it preserve or improve coding task success? Q4 Does added latency/CPU/RAM negate benefits? Q5 Does it protect concurrent sessions (within the stated tier)? Q6 Does learning improve quality-adjusted efficiency over time? Q7 How much of the gain does a zero-engineering prompt-only approach already capture?

## Phase 0 — Observational study (no AAES code required)
Run `aaes mine` (FUN-022) over existing Claude Code (`~/.claude/projects/**/*.jsonl`) and Codex (`~/.codex/sessions`) history. It is read-only and emits only aggregate, content-free numbers.
- Share of billed input attributable to tool results, by tool (Bash/PowerShell/Read/Grep/Glob/MCP) and by command family (dotnet build/test, ng/tsc/eslint, git, rg/grep, file reads).
- Top 20 commands by cumulative tokens; result-size distribution (p50/p95/max).
- Average turns a tool result remains in context (occupancy), compactions per session, and command re-run rate.
- Cache-write/cache-read mix from the usage fields.

- Mean prompt size at the first request (system prompt + tool schemas + memory): a separate token sink (MET-014).

**Go/no-go (decided):** **GO if tool results are ≥ 20% of cost-weighted input** across ≥ 5 sessions per agent. Otherwise re-scope before M1, e.g. toward context occupancy, compaction reduction or initial-context (tool-schema) reduction. `aaes mine` prints the verdict. The top command families from this phase define the M1 parser list and the M0 fixture corpus.

## Arm definitions
- A: unmodified target coding agent and tools.
- **A′: A plus prompt-only instructions** (CLAUDE.md / AGENTS.md) to use quiet flags, e.g. `dotnet build -v:q -clp:ErrorsOnly`, `dotnet test --logger "console;verbosity=minimal"`, `ng test --watch=false --no-progress`, `git --no-pager log --oneline -n 20`. Zero engineering; measures what instructions alone capture.
- B: coding agent + RTK. Record exact version/config and integration coverage. The RTK repo reviewed is MIT-licensed; confirm the edition actually benchmarked.
- C: coding agent + AAES deterministic-only.
- D: coding agent + AAES adaptive-model enabled (only if ADR-010's gate is met).
- E: AAES adaptive + memory/cache + permitted read-only delegation (later phase).

No unfair mixing of editions or configurations. Record that some native tools may bypass any integration, and report per-arm interception coverage.

## Workload strata
1. Angular/TypeScript compile, lint, test, repository symbol navigation.
2. .NET/C# restore, build, test, diagnostic search.
3. Git status/log/diff/merge-conflict investigation.
4. Log extraction with noisy repeated output and rare critical errors.
5. MCP tool output with JSON, binary metadata, pagination and errors.
6. Large repo exploration and repeated semantic lookups.
7. High-risk exact tasks: source file edits, database values, API schemas, security diagnostics.
8. Multi-session overlaps: same repo, different repo, same command, reused artifact IDs, concurrent cancel/restart.
9. Adversarial cases: malicious prompt instructions in README/logs, truncated outputs, tool failures, stale caches, modified files during read.
10. **Windows shells:** the same commands under Git Bash and the PowerShell tool; CRLF, UTF-16/code-page output, quoting edge cases.
11. **Long-running/interactive:** `ng serve`, `dotnet watch`, `docker compose up`, `npm start`. Must stream through untouched; measure that no buffering or hang occurs.
12. **Long sessions with compaction:** sessions long enough to trigger auto-compaction; verify behavior after compaction (ADR-014).
13. **Large-file navigation and read-then-edit:** tasks that require understanding or editing files of 500–5,000 lines (C# services, Angular components/templates, SCSS, JSON config). This is the primary M1/M2 stratum for ADR-022.

## Ground truth and quality
Create a fixed fixture corpus containing raw tool outputs, mandatory exact invariants (exit codes, error counts, paths, line numbers, code fragments, schema keys), and semantic questions with human-reviewed answer keys. Seed it from Phase 0's top command families.

**Two corpora (ADR-020):**
- **Public corpus (in repo, reproducible):** fix-commit replay on public OSS repositories in the target stacks, e.g. a .NET reference app (such as `dotnet/eShop`) and Angular OSS apps or libraries, pinned by commit with licenses recorded in `tests/fixtures/PROVENANCE.md`. Published results come from this corpus.
- **Private corpus (local only, `bench-private/`):** the same procedure on team repositories. Results may be summarised but never committed raw.

**Task construction: fix-commit replay.** Mine historical bug-fix commits from your own .NET and Angular repositories. For each, revert the fix (keeping the test that caught the bug, or adding one), give the agent the issue description, and use the repo's test suite as the oracle. These tasks are real, in-stack, cheap to generate, and absent from model training data. Supplement with blinded human review of final changes where tests are insufficient. Pre-register task success criteria before running each arm. Store raw fixtures only in an authorized local benchmark store.

## Experimental methodology
- Pin agent/model versions, temperature/settings where possible, tool versions, hook API versions, target repo commit, tokenizer, OS/shell, hardware and policy versions.
- Use identical starting repository snapshots and isolated worktrees. Run **paired** tasks (same task across arms) with multiple seeds/replications; randomize arm order to reduce drift.
- For non-deterministic agent behaviors, report distributions and uncertainty intervals, not single-run savings.
- Distinguish observed total tool tokens, observed provider usage (API or transcript usage fields), and baseline-attributed cost from paired runs.
- Compare equal permission grants and tool interception coverage. If one arm cannot intercept a tool, report it as uncovered.
- Control for model prompt-cache effects: record cache-write and cache-read input tokens and provider billing tier.
- Compare quality-adjusted metrics first; report token savings alongside failure/retry/re-run rates and latency.

### Sample size (task-level noninferiority)
One-sided α = 0.05, power 80%, baseline success ≈ 70%:

| Margin | Unpaired, per arm | Paired, 10% discordant pairs | Paired, 20% discordant pairs |
|---|---|---|---|
| 1 pp | ~26,000 | ~6,200 | ~12,400 |
| 3 pp | ~2,900 | ~690 | ~1,370 |
| **5 pp** | **~1,040** | **~250** | **~500** |

Paired approximation: n ≈ (z₁₋α + z₁₋β)² · ψ / δ², where ψ is the discordant-pair proportion. Estimate ψ in a pilot. The 1 pp margin is aspirational. The planning margin is **3–5 pp, paired**. Fixture-level fidelity (zero tolerance) is the primary gate because it is deterministic and cheap.

## Metric definitions
Direct tool-result reduction = 1 - sum(delivered tool tokens)/sum(raw tool tokens), for intercepted outputs only. Report un-intercepted coverage separately. Label Claude counts as calibrated estimates (MET-001).
Workflow token reduction = 1 - total optimized provider tokens / total baseline provider tokens, based on comparable matched task runs.
Net cash saving = provider baseline cost - provider optimized cost - attributable local compute/ops cost. (API mode only; subscription mode reports cost-weighted units as a labelled usage proxy, ADR-019.)
**Cache-aware result cost** ≈ T · (p_cache_write + (k − 1) · p_cache_read), where k = model requests whose prompt carries the result (until compaction or session end). `aaes mine` and `aaes report` use this formula with weights relative to the uncached input price (write ×1.25 or ×2, read ×0.1).
Cost per successful task = total cost across runs / number of successful runs; include failures and rework cost.
Token-per-successful-task = total provider tokens across runs / successful runs; include failures.
Retrieval usefulness = correct/evidence-backed retrieved facts divided by retrieved facts; recall measured against known relevant evidence.
Added latency = optimized end-to-end time minus matched baseline, reported median/p95 with confidence intervals where feasible. Report **per-invocation cold start** separately.
**Re-run-instead-of-retrieve rate** = re-executions of a command with a valid handle / compacted results delivered.
**Context occupancy** = Σ (tool-result tokens × turns retained). **Compactions per task.**
**Footer overhead** = footer tokens / delivered tokens; **pass-through rate** = results under the size threshold / all results.
**Read rebound** (MET-015) = tokens of follow-up ranged reads, `aaes show` and "ask twice" full reads after a shaped read of the same file in the same context. **Net shaped saving** = raw − delivered − rebound. **Follow-ups per shaped result** = follow-up calls ÷ shaped results; a follow-up to an outlined section is expected, so this guards the extra-call cost rather than counting failures.
**Edit failure rate** (MET-016) = failed Edit/MultiEdit calls / all Edit/MultiEdit calls.

## Initial decision gates (proposed thresholds, not observed)
| Gate | Threshold | Consequence |
|---|---|---|
| Phase 0 | Tool results ≥ **20%** of cost-weighted input (≥ 5 sessions per agent) | Otherwise re-scope before M1 |
| Security | 0 successful unauthorized session-crossing retrievals via AAES interfaces across all mandatory adversarial cases | Block release on any violation |
| **Critical fidelity (primary)** | 0 missing/mutated mandatory facts in qualifying golden tool-output suite | Block candidate policy on violation |
| Outcome (secondary) | Task success noninferior to baseline with pre-registered **3–5 pp** margin, paired, adequately powered | Otherwise hold/inconclusive |
| Economics | Positive net cost reduction on representative workload, not just cherry-picked compressible outputs; reported vs **A′** as well as A | Do not claim cost win otherwise |
| Re-run | Re-run-instead-of-retrieve rate does not erase measured savings (net tokens incl. re-runs > 0) | Loosen compression for offending families |
| Read rebound | Net shaped saving > 0 **and ≤ 1 follow-up call per shaped result**, per shaped family (Read, shell file reads, search); `aaes report` shows both | Raise threshold or disable shaping for that family |
| Edit fidelity | Edit failure rate with shaping not significantly higher than without (paired) | Block read shaping release |
| Performance | p95 additional pass-through overhead ≤20 ms **including cold start** on reference desktop (Windows + Linux); full-run latency measured | Optimize or document exception |
| Model fallback | All mandatory tools remain functional with local model unavailable | Block release if broken |
| Recovery | Authorized full-result retrieval works; foreign retrieval denied; expired handles report "expired" | Block on violation |
| Learning | Candidate is no worse on security/fidelity/outcomes and improves predefined efficiency measure with enough samples | Promote; else reject |

## M2 pilot protocol (runnable)

The full study above needs hundreds of paired tasks. The pilot is the first, small step. It checks that the harness works end to end, gives first effect sizes, and estimates the discordant-pair share ψ for the sample-size table. The pilot does **not** support a noninferiority claim.

### Arms
| Arm | Setup | Purpose |
|---|---|---|
| A | Claude Code, no AAES hooks | Baseline |
| A′ | A + the prompt-only instructions above in the task repo's `CLAUDE.md` | What instructions alone capture |
| C | AAES installed, default config (search shaping on, read shaping off) | Treatment |
| C-shadow | AAES installed, `"shadow": ["read", "search"]` | Hook overhead and would-be saving with identical agent input as A |

C-shadow gives the agent the same tool output as A. A difference in task success between A and C-shadow means harness noise, not AAES. A difference in wall-clock time is the hook overhead.

### Tasks
- **Corpus:** 20–30 fix-commit-replay tasks from the public corpus (pinned OSS repos in `tests/fixtures/PROVENANCE.md`), split across strata 1, 2, 6 and 13. Each task has an issue text, a reverted fix, and a test command as the oracle.
- **Selection rule (pre-registered):** a task qualifies when the oracle test fails on the reverted snapshot and passes on the fix commit, the fix touches 1–5 files, and the repo builds in under 5 minutes.
- Private repos (ADR-020) may be run the same way, but their results stay in the private findings.

### Run procedure
1. For each task and arm, create a fresh worktree of the reverted snapshot under the build folder (never in the repo folder). Give each run its own `CLAUDE_CONFIG_DIR` (so the arm's hooks and `CLAUDE.md` apply only to that run) and its own `AAES_HOME`, so no state crosses runs. Install the C and C-shadow hooks with `aaes init --mode command --settings <run config dir>/settings.json`: command mode reads `AAES_HOME` per call, while a shared http server would use one home for all runs.
2. Run the agent headless (`claude -p "<issue text>"`) with a pinned Claude Code version and model, the same permission settings in every arm, and a turn and time limit. Record the session id.
3. After the run, execute the oracle test in the worktree. Record pass or fail, and the diff size.
4. Collect the numbers: the session transcript (usage per request, compactions) and, for C and C-shadow, the run's `aaes report --json`.
5. Randomize the arm order per task. Run 3 replications per task and arm.

### Pre-registered metrics
| Metric | Source |
|---|---|
| Task success (oracle passes) | Oracle test |
| Cost-weighted input per task (uncached ×1, write ×1.25/×2, read ×0.1) | Transcript usage fields |
| Cost per successful task | Above, divided by successful runs |
| Tool calls, follow-up calls, `aaes show` calls | Transcript; `aaes report` |
| Direct reduction on shaped families; cache-aware saving | `aaes report` (C); shadow section (C-shadow) |
| Edit failure rate (MET-016) | `aaes report` |
| Compactions per task; wall-clock time | Transcript |

### Analysis
- Report each metric per arm with a 95% interval over tasks (bootstrap over tasks, replications averaged within a task).
- Primary pilot outputs: ψ (share of tasks where A and C disagree on success), the C vs A difference in cost-weighted input per task, and the C-shadow vs A difference in wall-clock time.
- Use ψ in the sample-size formula to size the powered study.

### Stop rules
- Stop and fix before the powered study if: any fidelity failure in the golden suite; a hook error in any C run (`aaes logs --errors`); a task where C fails and both A and C-shadow pass in every replication (inspect the transcript first).
- Publish the pilot result, positive or negative, with the full arm configuration.

## Multi-session isolation torture suite
Scope: the Standard tier is tested for **accidental** leakage via AAES interfaces. Hardened/High-assurance tiers are additionally tested against direct filesystem access by a hostile session (`06_THREAT_MODEL.md`).
- 2, 8, 32 concurrent sessions with independent principals and identical/different repos.
- Same command/arguments; verify no shared raw cache; check file artifact IDs and vector search.
- Guess or replay foreign artifact handle and session ID; verify denied independent of knowing names.
- Cancel while tool executes and while model is decoding; assert no late foreign delivery.
- Kill/restart (process or daemon, when present) while pending; require reattach and clean state.
- Inject misleading session IDs in MCP content, tool logs and model summaries; no routing change.
- Test model slots and KV states with canary secrets per session, then trigger mixed concurrent traffic (when inference is enabled).
- **Canary secrets in tool output:** never present in FTS/vector indexes, telemetry, footers or other sessions.
- **Compaction replay:** after `PreCompact`/`PostCompact` or `SessionStart(source=compact|clear|resume)`, no "already-seen/unchanged" replies reference pre-compaction content.
- **Hardened tier only:** a hostile session running as a different OS principal attempts direct file reads of another session's store; it must be denied by OS ACLs.
- Verify dashboard per-session permissions and aggregate telemetry excludes private strings.
- Shared repo but distinct sidecar-private memories; assert permitted shared file reads only via actual workspace ACL.

## Self-improvement experiment
Collect strategy/action/outcome records; derive candidate thresholds on training fixtures, evaluate on frozen holdout tasks not used to propose candidate. Offline replay is the default. Shadow candidates (P2, once an installed base exists) receive a copy of *authorized same-session* events and cannot change live responses. Report paired improvement, confidence and regression count. Promote only after hard gates; deploy feature flag; monitor canaries; auto-rollback on regression. For security-sensitive cases never use exploratory policies in production.

## Dashboard minimum panels
Per-session: raw/delivered tokens, direct reduction %, tokenizer precision (exact/estimated + error band), recovered bytes, **re-run rate**, **footer overhead and pass-through rate**, tool coverage %, tool category, cache hits, policy decisions, latency incl. cold start, model CPU, **compaction epochs**. Installation aggregate: usage only by authorized scope. Experiment comparison: A/A′/B/C/D task success, total billed tokens (cache-write/read split), cost per successful task, additional tool calls, retries and re-runs, occupancy, compactions, p95 latency, fidelity failures, security incidents. Every savings card must label observed vs estimated vs causal baseline.

## Reporting template
For each arm and workload: N tasks, OS/shell, intercept coverage %, raw/delivered tool tokens (estimate kind), provider input/output/cache-write/cache-read tokens, total billed $, local compute $, success % with interval, retries, re-runs, occupancy, compactions, p50/p95 wall-clock seconds, cold start p95, peak RSS MB, security violations, recovery %, full policy, registry and version. Publish negatives and inconclusive results.
