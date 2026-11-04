# M1 Spike — Claude Code hook capabilities (2026-11-04)

**Goal:** verify the four assumptions M1 depends on before building read/search shaping (ADR-022/023).
**Verdict:** all four confirmed. The latency result makes **HTTP hooks to a resident `aaes serve` the default on Windows** (ADR-021 accepted).

## Setup
- Claude Code **2.1.233** on Windows 11 (managed laptop), model `haiku`, headless `claude -p`.
- Isolated `CLAUDE_CONFIG_DIR` and synthetic workspace under the build folder; the user's own Claude Code config was untouched.
- Probe: [`spikes/hook-probe`](../../spikes/hook-probe/src/main.rs), a Rust hook handler with two modes:
  - **command:** JSON on stdin and stdout;
  - **HTTP:** `127.0.0.1` with a bearer token sent via `allowedEnvVars` header interpolation.

  With `--replace`, it swaps the main text field of the tool response for a marker and keeps the rest of the object, so the tool's output shape is preserved.
- Hooks were registered per run via `--settings <file>` (`PostToolUse`, matcher `*`).
- Total API cost for all runs: **$0.31**.

## Results

| # | Question | Result | Evidence |
|---|---|---|---|
| a | Does `PostToolUse.updatedToolOutput` replace what the model sees? | **Yes**, for Read, Grep, Bash and PowerShell | The model reported the marker for all four calls (T1) |
| b | Do hooks fire for tool calls inside sub-agents? | **Yes** | A Read inside a `general-purpose` sub-agent fired the hook with `agent_type` set; the sub-agent returned the replaced text (T2) |
| c | Does a shaped Read still satisfy read-before-Edit? | **Yes** | Edit succeeded after shaped reads and the file changed on disk (T3) |
| d | Hook overhead per tool call (Windows) | **Command +362 ms, HTTP +71 ms** (p50) | Transcript tool_use → tool_result gaps, 15 Read calls per variant (T4) |

### (a) Tool output shapes (what `updatedToolOutput` must match)
| Tool | Text field to replace | Other fields (keep) |
|---|---|---|
| Read | `file.content` | `type`, `file.filePath`, `file.numLines`, `file.startLine`, `file.totalLines` |
| Grep | `content` (content mode) | `mode`, `filenames[]`, `numFiles`, `numLines`, `totalLines` |
| Bash | `stdout` | `stderr`, `interrupted`, `isImage`, `noOutputExpected` |
| PowerShell | `stdout` | `stderr`, `interrupted`, `isImage` |

Per the docs, a value that doesn't match the tool's schema is **silently ignored**, and the original output is used. The product must therefore verify on every Claude Code version (contract test) that the shaped text actually reached the model.

### (c) Rebound observed
The probe shaped *every* Read, including ranged ones, so the model never got the exact text. It read `config.cs` **3 times** before editing (5 turns, against 3 without shaping), though the Edit still succeeded. This confirms two ADR-022 rules are essential:
- ranged reads always pass through;
- "ask twice, get full".

Both should be covered by golden tests and the MET-015 rebound metric.

### (d) Latency detail
| Variant | p50 | p90 | min | max |
|---|---|---|---|---|
| no hook | 14 ms | 22 ms | 8 ms | 72 ms |
| command hook (`hook-probe.exe cmd`) | 376 ms | 545 ms | 287 ms | 957 ms |
| HTTP hook (`hook-probe serve`) | 85 ms | 205 ms | 31 ms | 317 ms |

- The handler itself ran in ~60 µs. The command-hook cost is process creation plus the shell Claude Code wraps the command in, on a machine with endpoint security (≈ 90 ms per bare process start was measured earlier).
- At ~100k tool calls in the M-1 history, the command hook would have added **~10 hours** of cumulative wall time; the HTTP hook about **2 hours**.
- The HTTP figure is still far above localhost round-trip time. Candidates to investigate:
  - keep-alive (the probe sends `Connection: close`);
  - a narrower matcher (only Read/Grep/Bash/PowerShell instead of `*`);
  - Claude Code's own HTTP-hook overhead.
- Sample size is small (one run of 15 calls per variant); repeat on Linux/macOS and with more runs in M0's benchmark.

## Decisions
1. **ADR-021 accepted:** on Windows, `aaes init` registers `type: "http"` hooks pointing at a per-user resident `aaes serve`, authenticated with a bearer token via `allowedEnvVars`. Command hooks are the fallback; Linux/macOS default to be decided after measurement.
2. Hook matcher limited to the tools AAES shapes (`Read|Grep|Bash|PowerShell`), so other tools pay no hook cost.
3. Shaping writes only the text field and preserves all other response fields. A per-version contract test confirms the replacement reached the model.
4. ADR-022 rules are confirmed as necessary: ranged reads pass through, "ask twice, get full", and MET-015 rebound is tracked from day one.
