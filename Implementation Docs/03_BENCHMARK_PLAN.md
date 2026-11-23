# Benchmark and Evaluation Protocol — AAES v1.5

## 0. Change log
| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-11-03 | Initial draft. |
| 0.2 | 2026-11-03 | Applied design review (`05_DESIGN_REVIEW.md`). Added Phase 0 observational study from existing transcripts (R2), prompt-only arm A′ (R10), fix-commit-replay task corpus and sample-size table with a 3–5 pp noninferiority margin, fixture fidelity as the primary gate (R9), re-run/occupancy/compaction/footer metrics (R5, R8, R4), Windows/PowerShell and long-running strata (R13), compaction and same-user tests (R6, R3). |
| 0.3 | 2026-11-03 | Phase 0 gate fixed at **≥20% cost-weighted** and implemented as `aaes mine`. Results reported in both billing views (ADR-019). Benchmark corpus split into **public** (OSS repos, reproducible) and **private** (team repos, local only) per ADR-020. Added initial-context overhead metric. |
| 0.4 | 2026-11-03 | M1 evaluation focuses on read and search shaping: new stratum 13 (read-then-edit on large files), rebound and Edit-failure metrics and gates. Phase 0 result: GO (calibrated); calibration validated against `count_tokens` (`tools/calibration-check`): within ~5%. |
| 0.5 | 2026-11-04 | Read-rebound gate adds ≤ 1 follow-up call per shaped result (MET-015). Early local dogfooding suggests agents fetch most of smaller shaped files anyway, so the read threshold is being evaluated at a higher value (details in the private findings). |
| 0.6 | 2026-11-10 | Added the **M2 pilot protocol**: a small, runnable first study (headless runs, arms A / A′ / C / C-shadow, public corpus, pre-registered metrics, pilot to estimate ψ before the powered study). Cache-aware result cost now uses the same formula as `aaes report` and `aaes mine` (`k` later requests until compaction). Shadow mode added as a control arm. |
| 0.7 | 2026-11-14 | Ran the M2 pilot (240 runs, 20 tasks, 4 arms, 3 replications) and recorded the result below. The pilot is **inconclusive on efficiency** because the corpus almost never produces a shapeable tool result. Added the corpus requirement that a task must generate large tool output, and a pre-run coverage check. |
| 0.8 | 2026-11-15 | Built the large-output corpus (ESLint, 10 verified tasks) and the shaping-coverage pre-check (`bench/pilot/coverage_check.py`). The corpus passes the gate at **48.1% byte coverage**. The gate is weighted by bytes, not by results: tool-result sizes are heavy-tailed, and counting results would reject every realistic corpus. The probe also showed that **72.5% of tool-result bytes come from `Bash`**, which the M1 shapers do not handle, and that a short fix-replay run cannot show the cache-weighted effect. |
| 0.9 | 2026-11-15 | Added the **powered study design**: two tracks. Track A keeps the short fix-replay runs and reports direct savings only, with the limitation pre-registered. Track B introduces the **chain task** - one long session over many bugs from a single base - so the cache weighting can accumulate and be measured rather than assumed. |
| 1.0 | 2026-11-16 | Ran **Track A** (63 of 120 runs, stopped on the budget cap). The pre-registered coverage gate **failed at 25.2%** of shapeable bytes, so the result supports no efficiency claim. The C-shadow arm, which is null by construction, moved +23.5% against A, which measures the run-to-run noise floor directly. Shaping was also found to suppress its own opportunity: coverage in the live arm is half the shadow arm's, because the agent reads narrow ranges after seeing a shaped result. |
| 1.1 | 2026-11-16 | Measured one **chain run** before committing a Track B budget: 131 turns, 10 of 10 bugs fixed, $5.78, and it **passes the coverage gate at 43.8%**. A shaped result is re-read 61 to 112 times, so the cache weighting is off its floor and the net saving (118,580 cost-weighted units) is close to a whole short run's input. It also failed the read rebound gate: **1.6 follow-ups per shaped read** against a limit of one, giving back 66% of the read saving, while search shaping gives back none. |
| 1.2 | 2026-11-16 | Traced every read rebound in the chain run. **Corrects v1.1**: read shaping is net **+27%** in delivered bytes, not close to self-cancelling; the 66% was a gross-to-net ratio. The real defect is in the gate - its follow-up **count** condition is anti-correlated with its **net saving** condition, so every threshold that improves the net makes the count worse. A change is proposed with evidence, not made. |
| 1.3 | 2026-11-16 | **Gate decision: MET-015 stands, and the v1.2 proposal is withdrawn.** A follow-up costs an extra request, measured at **12,458 cost-weighted units**; charging it makes read shaping net **-64,144** units, not positive. Break-even is **0.99 follow-ups per shaped read** against a gate pre-registered at 1. Only the reporting changes: express both conditions in one currency so the threshold is derived per run. |
| 1.4 | 2026-11-16 | Ran the chain in **C-shadow** to settle attribution. Ranged re-reading is the agent's baseline habit: **45.9% of read events in C against 46.2% in C-shadow**, and lower per repeated file under shaping. MET-015 has no baseline subtraction, so it charges the shaper for the agent's own pattern. The **-64,144** figure is therefore the full-attribution end of a range, and the read verdict is open rather than negative. The thresholds still stand; the rebound measurement needs the shadow-controlled form. |
| 1.5 | 2026-11-23 | **The C / C-shadow chain pair replicates** (C 24.4% below C-shadow, about 2% spread per arm), but the arm-A anchor run exposed an **unpinned model**: Claude Code auto-updated and the `sonnet` alias moved from Sonnet 5 to Sonnet 5.5. The harness now records `served_models`, refuses mixed-agent comparisons and tags each series. The pinned Sonnet 5.5 series **fails the coverage gate at 0.0%**: the model reads and edits through `Bash` (0 `Read` calls), which M1 does not shape. Shell file-read shaping becomes the precondition for further study. |

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
**Read rebound** (MET-015) = tokens of follow-up ranged reads, `aaes show` and "ask twice" full reads after a shaped read of the same file in the same context. **Net shaped saving** = raw − delivered − rebound. **Follow-ups per shaped result** = follow-up calls ÷ shaped results; a follow-up to an outlined section is expected, so this guards the extra-call cost rather than counting failures. Measured in a live arm alone this is an **upper bound**: the agent makes ranged re-reads at a high baseline rate with no shaping present, so the attributable figure subtracts the same rate measured in C-shadow.
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
| Read rebound — *decided 2026-11-16* | **Unchanged.** The proposal to demote the count condition was withdrawn: a follow-up costs a whole request, and charging it puts break-even at 0.99 follow-ups per shaped read, within 1% of the pre-registered threshold. See "Gate decision: MET-015 stands". | Report `saved − follow_up_units − follow_ups × units_per_request` so the threshold is derived per run |
| Read rebound — *measurement corrected 2026-11-16* | The thresholds stand, but the rebound **count** is uncontrolled: ranged re-reading runs at 46% of read events with or without shaping. Use `follow-ups per shaped read in C − the same rate in C-shadow`; treat the uncontrolled count as an upper bound. See "Shadow-controlled rebound". | Run C and C-shadow on the same chains so the baseline exists per family |
| Coverage — *model drift 2026-11-23* | Unchanged threshold. On Sonnet 5.5 (Claude Code 2.1.330) the chain corpus scores **0.0%**, because file reads, edits and searches all go through `Bash`. See "Replication, model drift and the pinned series". | Pin the full model id; shape shell file reads before running another efficiency series |
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

### M2 pilot result (2026-11-14)

Run on 2026-11-14 with Claude Code 2.1.321 and model `sonnet`. 20 tasks (commander.js 8, marked 8, CsvHelper 4), 4 arms, 3 replications: **240 runs**, all completed. Reported agent cost for the whole pilot: $24.19.

| Arm | Runs | Success | Cost-weighted input / run | Cost / run (USD) | Tool calls | Compactions | Wall (s) |
|---|---:|---:|---:|---:|---:|---:|---:|
| A | 60 | 100% | 55,234 | 0.100 | 7.0 | 0 | 205 |
| A' | 60 | 100% | 55,081 | 0.099 | 8.1 | 0 | 236 |
| C | 60 | 100% | 57,780 | 0.104 | 7.3 | 0 | 217 |
| C-shadow | 60 | 100% | 55,703 | 0.101 | 7.1 | 0 | 201 |

Paired differences against A (per task, replications averaged, 95% bootstrap interval over the 20 tasks):

| Arm | Success (pp) | Cost-weighted input | Cost (USD) | Wall (s) |
|---|---|---|---|---|
| A' | +0.0 [+0.0, +0.0] | -153 (-0.3%) [-5,104, +4,198] | -0.001 [-0.010, +0.007] | +31 [-5, +73] |
| C | +0.0 [+0.0, +0.0] | +2,546 (+4.6%) [-2,494, +7,802] | +0.004 [-0.004, +0.012] | +12 [-26, +53] |
| C-shadow | +0.0 [+0.0, +0.0] | +469 (+0.8%) [-2,725, +3,575] | +0.001 [-0.004, +0.005] | -4 [-39, +29] |

**psi = 0/60.** Every arm passed every task in every replication, so the corpus cannot separate arms on success and the sample-size formula returns 0. Every efficiency interval contains zero.

**Why the efficiency result is inconclusive.** The AAES session events explain it. Across the 60 C runs the sidecar saw 103 tool results it could act on and shaped **3**:

| Arm | Pass | Shaped / shadow | Raw bytes | Delivered bytes | Saved |
|---|---:|---:|---:|---:|---:|
| C | 100 | 3 shaped | 157,406 | 149,026 | 8,380 |
| C-shadow | 96 | 6 shadow | 142,790 | 142,790 | 0 |

Pass reasons in C: `small` 71, `ranged` 16, `read_shaping_off` 8, `unparsed` 2, `not_content_mode` 2. The agent reads narrow line ranges and runs greps that return less than the cap, so the shaper almost never fires. The 8,380 bytes saved over 60 runs are about three orders of magnitude smaller than the per-run noise in the table above, so the +4.6% on C is noise, not an effect of shaping.

**What the pilot does establish.**
- The harness works end to end: 240 paired runs, fresh config and `AAES_HOME` per run, randomized arm order.
- AAES costs nothing in reliability on this corpus: 100% success, **0 hook errors** in 120 C and C-shadow runs, and no stop-rule task.
- AAES costs nothing in latency: C-shadow wall-clock is -4 s against A, interval [-39, +29].
- No fidelity failure and no compaction in any run.

**Corpus requirement added for the powered study.** Fix-commit replay with the failing test named in the issue text is the wrong workload to measure shaping: the agent already knows where to look, so its tool output stays small. The powered study needs tasks whose tool output is large by construction - whole-file reads of large files, repo-wide searches, and open-ended exploration (strata 2, 6 and 13). Before the powered study runs, a **coverage pre-check** must show that at least 30% of shapeable tool results in the C arm are actually shaped; a corpus below that threshold cannot measure the treatment and must be replaced.

### Large-output corpus probe (2026-11-15)

The pilot result above said the corpus could not exercise the shaper, so the next step was a
corpus whose tool output is large by construction. Three changes were made and then probed
cheaply, before any powered run.

1. **A larger repository.** ESLint joins the public corpus: 2,363 tracked files and 676,162
   lines against 220 files and 29,003 lines for commander.js, with 306 rules, 302 rule test
   files and single test files up to 14,411 lines. Ten tasks were mined and verified
   (`bench/pilot/tasks/eslint.json`); the oracle is one `mocha` file and takes about 9 seconds.
   ESLint commits no lockfile, so its dependency set is keyed on `package.json` instead.
2. **A prompt that hides the answer.** `run_pilot.py --prompt-style discover` gives the bug
   report and a command template, not the failing test path. The located prompt handed the agent
   the file and the exact command, which is why its searches returned `small`.
3. **Read shaping on.** `--read-shaping` turns it on in the C arms; the pilot ran with the
   shipped default (off), so every large read passed with `read_shaping_off`.

**Probe:** 3 ESLint tasks, arm C only, one replication, $0.60.

| Corpus and configuration | Shapeable results | Covered | Byte coverage |
|---|---:|---:|---:|
| Pilot corpus (commander.js), located prompt, read shaping off | 36 | 0 | 0.0% |
| ESLint, discover prompt, read shaping on | 6 | 1 | **48.1%** |

**The corpus passes the gate.** The one shaped read cut 6,204 bytes to 1,511 (76%).

The gate is measured on bytes, not on results, and the first version of
`coverage_check.py` got this wrong: counting results it read 16.7% and failed the corpus. Tool
result sizes are heavy-tailed, so a corpus is judged by the share of shapeable *bytes* the
shaper acted on. A replay over real sessions shows the same shape - the shaper acts on about
5% of read and search results, and those results carry roughly a fifth of the raw tokens in
their families. Counting results would reject every realistic corpus, this project's own
sessions included.

The corpus clears the gate, but the event log shows where the rest of the output goes.
**The agent does most of its file reading through `Bash`.**

| Tool family | Events | Raw bytes | Share |
|---|---:|---:|---:|
| `Bash` (`other`, not handled) | 304 | 448,860 | **72.5%** |
| Search (`Grep`) | 80 | 107,854 | 17.4% |
| Read | 28 | 59,674 | 9.6% |
| Shell read | 1 | 2,773 | 0.4% |

Measured over every arm-C run to date. One probe run used `Bash` exclusively and no `Read` or
`Grep` at all; its transcript shows `cat ... | sed -n`, `sed -n 485,640p`,
`npx mocha ... | grep -A40 failing | head -6`, and a heredoc script to edit the file. The agent
pipes and truncates its own output by default, and it reaches for the shell rather than the
shaped tools.

**Consequences for the study.**
- The M1 shapers (`Read`, `Grep`) address at most about 27% of tool-result bytes on this
  workload. Shell output is the other 72.5%, and it is not a large-output tail: the mean `Bash`
  result is 1,363 bytes, nothing exceeds 16 KB, and 81% of the commands already contain a pipe
  (`| tail -30`, `| head -6`). The agent truncates its own output, so a shell parser would have
  little left to remove. Of those bytes, 48.8% are file reads (`cat`, `sed -n`, `head`), 35.3%
  test-runner output, 9.6% `git`.
- `shape::shell_read_target` only recognises a single `cat`/`type`/`Get-Content` invocation, so
  the compound, piped commands the agent actually writes are classed `not_handled`. Widening it
  would raise coverage, but the bytes behind it are small, so it is not where the gain is.
- **A pilot run is too short to show the real effect.** The saving is cache-weighted: it is
  multiplied by the number of later requests that re-read the result before compaction. Real
  sessions re-read a tool result about 200 times; a 7-turn fix-replay run re-reads it a handful
  of times. The same shaping decision is therefore worth about two orders of magnitude less per
  run here than in ordinary use, which is why the pilot's per-run differences sat inside the
  noise. A powered study must either use long, multi-turn sessions or report direct savings and
  state that it does not measure the cache-weighted effect.
- The 30% gate stays, now byte-weighted. It is doing its job: it stopped a second inconclusive
  study for about a dollar, and it passes the corpus that can actually exercise the shaper.


The probe is small (3 tasks, one replication) and the byte shares come from four repositories on
one model, so they size the problem rather than settle it.

## Powered study design (2026-11-15)

The pilot and the probe together rule out a single run shape. The direct saving and the
cache-weighted saving need different sessions, so the powered study runs two tracks and reports
them separately. Neither track is a noninferiority claim on its own.

### Track A — direct savings, short runs

The existing fix-replay harness on the ESLint corpus, arms A / A′ / C / C-shadow, with
`--prompt-style discover` and `--read-shaping`. It is ready: the corpus passes the coverage gate.

- **Primary endpoint:** delivered tool-result bytes per task, and cost-weighted input per task.
- **Secondary:** task success, edit failures, rebound calls, wall-clock time.
- **Pre-registered limitation.** A fix-replay run is about 7 turns, so the number of later
  requests that re-read a shaped result is near its floor. Track A therefore reports the direct
  reduction and states that it does not estimate the cache-weighted saving. A null result here is
  evidence about direct savings only.

### Track B — cache-weighted savings, long sessions

The saving on a shaped result is multiplied by the number of later requests that re-read it
before compaction. That count scales with session length, so the session has to be long by
construction rather than by luck.

**The chain task.** One session covers many bugs from a single base commit:

1. The base is the parent of the earliest fix commit in the set, so no fix in the set is present.
2. The test files of every fix in the set are copied onto that base, so every one of them fails.
3. The agent receives all K bug reports in one prompt and works through them in one session, so
   the context accumulates across the whole run instead of resetting per bug.
4. The oracle is that every one of the K tests passes. Per-bug pass or fail is recorded as well,
   so a partial run still carries information.

**Validity conditions, verified before a chain is used:**

- every fix's source diff applies cleanly to the base,
- no two fixes in the set touch the same test file,
- each test fails on the base and passes once the fixes are applied.

For ESLint, all ten mined tasks satisfy the first two conditions against base `61949398`, a span
of about three weeks of history.

**Building and running a chain.** `bench/pilot/make_chain.py` builds the task from tasks that
`mine_tasks.py` has already verified individually, checks the static conditions, and with
`--verify` runs every oracle on the base and again with the fixes applied:

    python bench/pilot/make_chain.py --work <work> --repo eslint --out bench/pilot/tasks/eslint-chain.json --verify
    python bench/pilot/run_pilot.py --work <work> --tasks eslint-chain-10 --max-turns 200 --timeout 7200 --budget <cap> ...

A chain task is never part of a default sweep; `run_pilot.py` runs one only when it is named in
`--tasks`, because a Track B run is long and costs far more than a Track A task. Per-bug pass or
fail is written to `runs.jsonl` as `per_bug`, with `bugs_passed` and `bugs_total`.

**Reported alongside the endpoints:** the mean number of later requests per shaped result,
measured per run. The cache weighting is then visible in the result rather than assumed, and
Track B can be compared directly against the pilot's floor and against the figure that real
sessions show.

### Open item: powering the success comparison

The pilot's ψ (the share of tasks where the baseline and the treatment disagree on success) came
out at 0, because every one of the 240 runs succeeded. A zero discordant-pair share cannot size a
noninferiority test. Until a run shape produces disagreement, the sample size for the success
claim uses a conservative assumed ψ, stated with the result, rather than a measured one.

### Track A result (2026-11-16)

Track A ran on the ESLint corpus with the discover prompt and read shaping on. It stopped on the
budget cap after **63 of 120 runs** and $35.78. The per-run cost was about three times the figure
taken from the three-task probe ($0.568 against $0.201), because the discover prompt on these
tasks takes 9 to 23 turns rather than the probe's 7. A sweep sized from a probe's mean cost
should carry that margin.

Two changes were forced before the runs. The pinned agent version moved: the pilot and the probe
ran Claude Code 2.1.321, and only 2.1.233 was on the machine, so the three probe rows were
removed from the results file and every Track A run is on one version. The runs used the
subscription rather than an API key, after the key returned `Credit balance is too low`; the
runner treated that as the agent being unable to run, wrote no rows and stopped, which is the
intended behaviour and cost nothing.

**The pre-registered coverage gate failed.**

| Arm | Shapeable results | Covered | Byte coverage |
|---|---:|---:|---:|
| C | 172 | 10 | **25.2%** |
| C-shadow | 132 | 15 | 52.1% |

Coverage is below the 30% threshold, so the efficiency numbers below do not support a claim in
either direction. They are reported because the arms themselves are informative.

| Arm | Runs | Success | Cost-weighted input / run | Paired difference against A (95% bootstrap) |
|---|---:|---:|---:|---|
| A | 16 | 100% | 127,604 | - |
| A′ | 17 | 100% | 151,064 | +31.5% [+3,194, +80,958] |
| C | 15 | 100% | 165,727 | +43.9% [-1,744, +123,846] |
| C-shadow | 15 | 100% | 150,436 | +23.5% [-6,275, +80,830] |

psi = 0 of 15 discordant pairs, 0 hook errors, no stop-rule task, no compaction in any run.

**C-shadow measures the noise floor directly.** It gives the agent byte-identical tool output to
A, so it is a null arm by construction, and it still moved +23.5%. The run-to-run spread on this
endpoint is therefore about a quarter of the mean, and C's +43.9% sits inside it. Nothing in this
table is evidence that the sidecar costs more, and nothing in it could have been evidence that it
costs less. A study on this endpoint, at this run length, cannot resolve an effect of a few
percent however many runs it buys.

**Shaping suppresses its own opportunity.** On the same tasks, coverage in C is half what it is
in C-shadow, and the pass reasons say why: `read:ranged` accounts for 70 of C's passes against 44
of C-shadow's. After the agent sees a shaped result it reads narrow ranges, and a narrow range is
already small enough to pass. The treatment changes the agent's later behaviour, so coverage
measured in a shadow arm overstates what the live arm will see. The corpus gate should be read
from the live arm, as it is.

**A′ is the one interval that excludes zero,** and it has a mechanism: prompt-only instructions
add a `CLAUDE.md` to every request, so they cost input tokens on every turn whether or not they
save output. With four comparisons at this noise level the finding is suggestive, not settled.

**Harness faults found and fixed.** Two runs were lost to
`fatal: Unable to create '.../.git/index.lock': File exists`, because the agent can leave a git
process running in the run's repository and on Windows the loser of that race dies; the exception
aborted the whole run. Replication 1 is missing two of its forty cells for this reason. Git calls
now wait the lock out. The analyzer also had no task filter, so a new study could not be reported
without the earlier pilot's runs on other repositories being averaged into it.

**Consequence.** The remaining 57 runs were not bought. The gate says this corpus cannot measure
the treatment, and more runs would estimate the same noise more precisely. Track B exists to
change the run length, which is the quantity that governs whether any of this is measurable.

### Chain run measurement (2026-11-16)

One chain run, arm C, one replication, before any Track B budget was committed. Track A had just
shown what a cost estimate taken from a short probe is worth, so the chain was measured rather
than predicted.

`eslint-chain-10`, arm C, read shaping on: **131 turns, 1,214 s, $5.78, all 10 bugs fixed**, no
hook errors.

| | Track A run | Chain run |
|---|---:|---:|
| Turns | about 16 | **131** |
| Cost | $0.568 | $5.78 |
| Byte coverage | 25.2% (fails the gate) | **43.8% (passes)** |
| Later requests per shaped read | - | **61.2** |
| Later requests per shaped search | - | **111.5** |

**The cache weighting is no longer at its floor.** A shaped result in this run is re-read 61 to
112 times before compaction, against a handful in a seven-turn fix-replay run and about 209 in
ordinary sessions. The chain reaches roughly a third to a half of ordinary use, which is enough
for the saving to be a large quantity rather than a rounding error:

| Family | Saved (cost-weighted units) | Given back by follow-ups | Net |
|---|---:|---:|---:|
| read | 103,943 | 68,423 | 35,520 |
| search | 83,060 | 0 | 83,060 |
| **total** | **187,003** | **68,423** | **118,580** |

For scale, a Track A baseline run consumed 127,604 cost-weighted units in total. The net saving
from one chain run is close to the whole input of a short run. That difference is what run length
buys, and it is what neither earlier study could see.

**Read shaping fails the rebound gate (MET-015).** It draws **1.6 follow-up calls per shaped
result** against a limit of one, and `families.read.ok` is false. Of what the read shaper saves,
68,423 cost-weighted units of 103,943 come back when the agent re-fetches what was trimmed.
Search shaping has no follow-ups at all and keeps everything it saves. The next entry traces
every follow-up and corrects what that ratio means: the read shaper is still net positive, and
the condition it fails is the call count rather than the saving. Read shaping is off by default;
the Track A and chain runs turned it on explicitly with `--read-shaping`.

Two results are clean. Edits after a shaped result were 10 calls with **0 failures** (the run's
one failure was an unshaped edit, `not_found`), so shaping did not damage the agent's ability to
edit what it had read. And there were no hook errors in a 131-turn session.

**What this is not.** It is one run, so every number here is a point estimate with no interval.
The analysis this plan specifies bootstraps over tasks, and one chain gives only replication-level
variation, so Track B needs several chains before it can produce an interval. Budget, now measured
rather than guessed: **$5.78 per chain run**, so one chain across four arms and three replications
is about $70.

### Read rebound, traced (2026-11-16)

All eight follow-up events in the chain run were examined. Every one is the same move: a shaped
read, then a `read pass ranged` on the same key. The agent re-reads the file it was just given an
outline of, with an explicit line range. It did not call `aaes show` once in 131 turns.

| Raw | Shaped to | Follow-ups | Total delivered | Net |
|---:|---:|---|---:|---:|
| 5,573 | 1,712 | 4,241 | 5,953 | **-380 (-7%)** |
| 5,935 | 1,298 | 5,110 | 6,408 | **-473 (-8%)** |
| 6,204 | 1,511 | 1,955 + 1,747 | 5,213 | +991 (+16%) |
| 9,762 | 1,857 | 1,505 | 3,362 | +6,400 (+66%) |
| 12,180 | 1,975 | 4,549 + 398 + 976 | 7,898 | +4,282 (+35%) |
| **39,654** | | | **28,834** | **+10,820 (+27%)** |

**Correction to the entry above.** That entry said 66% of the read saving is handed back and that
the read shaper is "close to self-cancelling". The 66% is a gross-to-net ratio, not a loss: read
shaping is **net +27% in delivered bytes** here, and positive in cost-weighted units as well
(103,943 saved against 68,423 returned). The gate it fails is the follow-up count, not the saving.

**The gate's count condition is anti-correlated with its value condition.** The 12,180-byte file
drew three follow-ups and saved 35%. The 5,935-byte file drew one and lost 8%. Counting calls
prefers the file that loses bytes. Every threshold that improves the net makes the count worse:

| Read threshold | Shaped | Net bytes | Follow-ups per shaped |
|---|---:|---:|---:|
| 4,800 bytes (~2,000 tokens, the current default) | 5 | +27% | 1.6 |
| 6,000 bytes (~2,500 tokens) | 3 | **+41%** | 2.0 |
| 7,200 bytes (~3,000 tokens) | 2 | **+49%** | 2.0 |

**Proposed change, not yet made.** The read-rebound gate should be net shaped saving and
cost-weighted net per shaped result, with follow-ups per shaped result kept as a diagnostic
rather than as a pass condition. A pre-registered gate is not rewritten because it failed, so this
is recorded as a proposal with its evidence, to be decided before Track B runs and reported either
way.

**The size threshold looks too low.** `read_threshold_tokens` is 2,000 at `bytes_per_token` 2.4,
so shaping starts at about 4,800 bytes, where the head plus outline is not much smaller than the
file. Both net-negative files sat just above that line. This is the first public measurement of
something the v0.5 entry already suspected from local dogfooding: agents fetch most of a smaller
shaped file anyway.

**Limits of this evidence.** Five shaped reads in one run: a direction, not a calibration. The
follow-ups are attributed by key, and Track A's shadow arm showed 44 `read:ranged` passes with no
shaping at all, so some of these ranged reads would have happened regardless and the true rebound
is lower than the attributed figure. `aaes replay` cannot settle it offline either, because it
measures gross savings over past transcripts and has no way to observe a follow-up that shaping
would have caused.

### Gate decision: MET-015 stands (2026-11-16)

The entry above proposed demoting the follow-up-count condition to a diagnostic, on the grounds
that it is anti-correlated with net bytes. **The proposal is withdrawn. The gate stands as
pre-registered.** The argument for demoting it was missing a cost.

A follow-up is not only its bytes. It is an extra request, and an extra request re-reads the whole
conversation context. Measured on the chain run: **12,458 cost-weighted units per request** across
129 requests. Charging each follow-up one request changes the sign of the result.

| | Units |
|---|---:|
| read, saved | 103,943 |
| read, follow-up bytes (weighted) | -68,423 |
| read, follow-up call cycles (8 × 12,458) | -99,664 |
| **read, net** | **-64,144** |
| search, saved | 83,060 |
| search, follow-ups | 0 |
| **search, net** | **+83,060** |

So the earlier "+27% in delivered bytes" was right about bytes and wrong as a verdict: counted in
the currency that is actually billed, read shaping is net negative on this run. Search shaping is
unambiguously positive.

**The threshold was well chosen.** Break-even, from the measured figures, is at **0.99 follow-up
calls per shaped read**. The gate was pre-registered at ≤ 1 before any of this was measured. It is
not an arbitrary round number that happened to fail; it is within one percent of where the
arithmetic puts the break-even, because one follow-up costs about one request and one request
costs about what one shaped read saves.

**What changes.** Nothing in the criterion, and one thing in how it is reported. The two
conditions stay, but `aaes report` should express them in one currency, charging each follow-up a
request:

    net = saved_units − follow_up_units − (follow_ups × units_per_request)

Then the count threshold is derived from the data of each run rather than assumed, and a family
whose follow-ups are cheap because its sessions are short is not judged by the same round number
as one whose sessions are long. The count stays visible either way.

**Attribution is the one real uncertainty.** Track A's shadow arm shows the agent making ranged
reads with no shaping present, so not all eight follow-ups were caused by shaping. The sign of the
result depends on how many were:

| Share of follow-ups caused by shaping | Net units |
|---|---:|
| 100% (8 calls) | -64,144 |
| 75% (6 calls) | -22,122 |
| 50% (4 calls) | +19,900 |
| 25% (2 calls) | +61,921 |

Read shaping is somewhere between marginally positive and clearly negative, and which one cannot
be settled from a single arm. Measuring it needs a chain run in C-shadow, where the agent sees
unshaped output and its ranged reads are the baseline rate. That is one run, and it is worth
making before read shaping is reconsidered.

**Consequence for Track B.** The gate is unchanged, so Track B runs against the criterion it was
registered with. Read shaping stays off by default, which both the shipped configuration and this
analysis agree on, and the study reports the read family as failing its gate rather than quietly
passing under a weaker one.

### Shadow-controlled rebound (2026-11-16)

The entry above left one question open: how many of the eight follow-ups shaping actually caused.
`eslint-chain-10` was run again in **C-shadow**, where the hooks run and record what they would
have shaped but the agent receives byte-identical output to an unmodified run. 150 turns, $7.36,
all 10 bugs fixed.

| | Arm C (shaping live) | C-shadow (output untouched) |
|---|---:|---:|
| Read events | 37 | 52 |
| Files read more than once | 10 | 12 |
| Later ranged reads on a repeated file | 17 | 24 |
| **Share of read events that are later ranged reads** | **45.9%** | **46.2%** |
| Per repeated file | 1.7 | 2.0 |

**Re-reading a file with a line range is this agent's normal working pattern.** Nearly half of all
its reads are a ranged re-read of a file it has already opened, and the rate is the same to within
a third of a percentage point whether shaping is on or off. Per repeated file the rate is lower
under shaping, 1.7 against 2.0, so the shaping-attributable excess comes out at **-0.3**.

**MET-015 has no baseline subtraction.** It counts any ranged read of a shaped key as a rebound
caused by shaping. On this evidence most of the eight follow-ups would have happened anyway, so
the metric charges the shaper for the agent's own habit. The rebound figure to use is the
shadow-controlled one:

    attributable rebound = (follow-ups per shaped read in C) − (the same rate in C-shadow)

**The read verdict is open, not negative.** The entry above computed -64,144 units by charging
shaping for all eight follow-ups. That is the full-attribution end of the range. At the
attribution this run suggests, the follow-up bytes are not a cost of shaping at all and most of
the gross 103,943 units stands. Neither end is claimed here. The honest statement is that the
sign of the read-shaping result depends on an attribution that one pair of sessions cannot fix.

**Weak corroboration, offered as such.** Arm C cost $5.78 in 131 turns; C-shadow, which hands the
agent untouched output, cost $7.36 in 150 turns. Shaping ran cheaper than the arm it should match.
With one run per arm and a noise floor of about a quarter of the mean from Track A, this is
suggestive and nothing more.

**Limits.** The two arms took different paths, 27 distinct files against 20, so these are not the
same reads being compared; the 45.9% against 46.2% figure is the strongest number here precisely
because it normalises that away. The direct like-for-like test - was a shadow-shaped file later
re-read - rests on the two reads C-shadow shadow-shaped, and neither was revisited. One pair of
sessions gives a direction and not a rate.

**What changes.** The MET-015 thresholds stand, as decided above. What needs correcting is the
rebound measurement rather than the criterion: `aaes report` should carry the shadow-controlled
rate wherever a shadow arm exists, and the uncontrolled count should be labelled as an upper
bound on the cost of shaping. Track B should run C and C-shadow on the same chains so the baseline
is available for every family.

### Replication, model drift and the pinned series (2026-11-23)

**The C / C-shadow pair replicates.** A second replication of each arm on `eslint-chain-10`
(Claude Code 2.1.233, Sonnet 5) landed close to the first:

| Run | Cost-weighted input | Cost | Turns | Bugs fixed |
|---|---:|---:|---:|---:|
| C-r1 | 1,607,037 | $5.78 | 131 | 10 / 10 |
| C-r2 | 1,575,662 | $5.90 | 116 | 10 / 10 |
| C-shadow-r1 | 2,085,413 | $7.36 | 150 | 10 / 10 |
| C-shadow-r2 | 2,122,324 | $7.39 | 140 | 10 / 10 |

Arm C sits **24.4% below C-shadow**, with about 2% spread inside each arm - far tighter than the
quarter-of-the-mean noise floor Track A measured on short runs. Pooled over both pairs, later
ranged reads per repeated file are 1.71 in C and 1.70 in C-shadow, which confirms the previous
entry: shaping does not raise the agent's ranged re-reading. This was not yet an efficiency
result, because the chain had no arm A and Track A had shown C-shadow itself drifting against A.

**The arm-A anchor run exposed an unpinned model.** The harness passed `--model sonnet`. Between
the C / C-shadow runs and the arm-A run, Claude Code auto-updated from 2.1.233 to 2.1.330, and the
alias moved from `claude-sonnet-5` to `claude-sonnet-5-5`. The arm-A run (10 of 10 bugs) was
therefore served by a different model on a different agent version, and is not comparable with
the four runs above. Nothing in the results file flagged it: the row recorded the alias, not the
model that answered. Three harness changes followed:

1. `run_pilot.py` records `served_models`, the model ids the API actually returned, counted from
   the transcripts, alongside the requested `--model`.
2. `analyze_pilot.py` refuses to compare rows whose agent version or served models differ, unless
   `--allow-mixed` is passed. On the existing chain rows it fires immediately.
3. `--tag` names a series: runs go to `<arm>-r<rep>-<tag>`, results to `results/runs-<tag>.jsonl`,
   and the budget counts only that series. `coverage_check.py` reads one series at a time.

The methodology already said to pin agent and model versions. An alias is not a pin; the full
model id is.

**The pinned series: Sonnet 5.5 gives AAES nothing to shape.** All three arms were re-run twice on
`claude-sonnet-5-5`, given by full id, on Claude Code 2.1.330, with read shaping on (tag `s55`).
Every row's `served_models` is that one id.

| Arm | Cost-weighted input (r1 / r2) | Mean | vs A | Requests | Cost / run |
|---|---|---:|---:|---:|---:|
| A | 434,646 / 409,062 | 421,854 | - | 45 / 45 | $0.71 |
| C | 328,235 / 379,584 | 353,909 | -16.1% | 35 / 41 | $0.60 |
| C-shadow | 390,292 / 424,519 | 407,406 | -3.4% | 42 / 47 | $0.69 |

All six runs fixed 10 of 10 bugs, for $4.00 in total. **The coverage gate fails at 0.0%**: arm C
saw 13 shapeable results, 11 below the size cap and 2 unparsed, and delivered every byte it
received. C and C-shadow handed the agent identical output, so the -16.1% is run-to-run spread
(arm C simply made fewer requests), not a saving. No efficiency claim is made from this series.

The cause is a change in how the agent works:

| | Sonnet 5, 2.1.233 (C-r1) | Sonnet 5.5, 2.1.330 (six runs) |
|---|---:|---:|
| `Read` calls | 38 | 0 |
| `Grep` calls | 25 | 1 in total |
| `Edit` calls | 16 | 0 or 1 per run |
| `Bash` calls | 50 | 35 to 46 per run |
| Tool-result bytes per run | about 140 KB | about 60 KB |
| Cost-weighted input per run | 1.6 to 2.1 M | 0.33 to 0.43 M |

Sonnet 5.5 reads files with `sed -n`, edits them with `python`, and searches with `grep`, all
inside `Bash`. M1 shapes the `Read` and search tool results; shell output passes through. The
v0.8 probe had already found 72.5% of tool-result bytes coming from `Bash`; on this model it is
nearly all of them. The run is also about four times cheaper than on Sonnet 5, so there is less
input to save in the first place.

**What this means.**

- The replicated -24.4% (C against C-shadow) holds for Sonnet 5 on Claude Code 2.1.233 only. It
  still has no arm-A anchor on that stack, and the stack is no longer the one users get by default.
- On the current default stack, AAES as built cannot be measured on this corpus, because there is
  nothing it handles. Shaping shell file reads (`sed -n`, `cat`, `head`, `python` printing a file)
  is the precondition for any further efficiency study, not an optional extension.
- Two changes moved at once - the model and the agent version - so this series cannot say which
  one changed the tool mix. Running `claude-sonnet-5` on 2.1.330 would separate them.
- Every future series is tagged and records `served_models`, so drift of this kind is caught by
  the analyzer instead of by reading transcripts.

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
