# M1 contract test — AAES hooks with real Claude Code (2026-11-04)

**Build under test:** `aaes` 0.1.0 (M1 increment 1): `aaes init` (HTTP mode), resident `aaes serve`, read shaping (ADR-022), search shaping (ADR-023).
**Environment:** Claude Code 2.1.233, Windows 11 (managed laptop), model `haiku`, headless `claude -p`. Isolated `CLAUDE_CONFIG_DIR` and `AAES_HOME`; the user's own Claude Code setup was untouched. API cost: **$0.32** for all runs.

**Fixtures (synthetic):**
- `BigService.cs`: 1,810 lines, 31 KB (~13k tokens), one class with 300 methods; `ComputeTotal123` at lines 748–752, `Method250`'s return at line 1513.
- `src/Feature1..8/Handler.cs`: 30 `Send(` matches each.

## Scenarios and results (final build)

| # | Scenario | Result | Shaping |
|---|---|---|---|
| C1 | Read the large file, then report its first line and `ComputeTotal123`'s range without other calls | First line ✅ (from the exact head). Range omitted by the model, though the outline contained it (see note) | 31 KB → 9.3 KB delivered (head 40 lines + complete outline) |
| C2 | Read the large file, then edit `Method250` | ✅ Edit succeeded; file correct | Shaped read, then **one 96-byte ranged read** (rebound), then Edit |
| C3 | Large Grep (240 matches / 8 files), report totals | ✅ `{"matches": 240, "files": 8}`, exact | 24.6 KB → 13.6 KB, capped 20 lines/file, handle for the rest |
| C4 | Sub-agent reads the large file and reports `ComputeTotal123`'s range | ✅ `748-752`, from the outline alone | Shaped inside the sub-agent (separate ask-twice context) |
| C5 | 15 small Reads (latency) | ✅ all passed through unchanged | p50 **41 ms**, p90 64 ms per tool call (no hook: 14 ms) |

**Token check (C1):** the prompt grew by 4,451 tokens across the shaped Read, against ~3.9k estimated for head + outline. The unshaped Read would have been ~13k tokens plus line-number overhead, so the saving is **≈ 70%** on this file. The outline is delivered once (`hook_additional_context`); a `hook_success` transcript record with the same text is not billed again.

**Latency:** the resident server with keep-alive adds **+27 ms** p50 per hooked call. That compares with +71 ms for the spike probe (no keep-alive) and +362 ms for command hooks (M1 spike).

**Note on C1:** `hook_additional_context` reached the model (confirmed in the transcript), and the same lookup succeeded in C4. The missing JSON field is a Haiku instruction-following lapse, not a delivery failure. Keep an eye on it in M2 with stronger models.

## Defects found and fixed during the test
1. **Incomplete outlines for large files.** With 300 methods and a 120-item cap, only the first ~118 methods were listed, so the model either fetched the whole file again (C2 first run: 31 KB rebound) or reported "not found" (C4 first run).
   - **Fix:** compact member labels when over budget (`ComputeTotal123(int x)`), cap raised to 400 items / 10 KB, and a pass-through guard requiring ≥ 30% saving.
   - **Result:** complete outline, rebound down to 96 bytes.
2. **Session hang on Windows.** The server spawned by the `SessionStart` hook inherited the hook's stdout/stderr pipes, so Claude Code waited for them to close.
   - **Fix:** clear `HANDLE_FLAG_INHERIT` on std handles before spawning (one audited `unsafe` block). Verified: hook pipes close after ~2 s while the server keeps running.
3. **`aaes init --port` not carried into the `SessionStart` command**, so the server started on the default port while hooks targeted another. AAES failed open as designed; nothing was shaped. **Fix:** the ensure command now includes `--port`.
4. **`aaes serve --stop` didn't stop the server.** The `Connection: close` check ran before the exit. **Fix:** exit right after responding to an authenticated shutdown.

## Scripted contract test (2026-11-07)

`scripts/contract-test.ps1` (PowerShell 7, any OS) runs these scenarios against the installed `claude`, plus **C6**: a shell `grep -rn` through the Bash tool. It creates the fixtures, an isolated `AAES_HOME` with read shaping turned on, a separate settings file and server port, and then checks:

- **Hard checks (exit 1 on failure):** AAES recorded the shaping decision (main context, sub-agent, Grep, Bash), the shaped text reached the model (outline in `hook_additional_context`, handle in the Grep result), exact facts survived (first line, 240 matches / 8 files, the edit landed on line 1513), small reads passed through, and AAES's log has no errors or schema-drift warnings.
- **Soft checks (warnings):** answers that depend on the model's instruction following (C1 range, C2 `SUCCESS`, C4 range).

The Claude Code version is pinned in `tests/contract/claude-code.version`. A different version gives a warning, and `-Strict` makes it a failure. `-UpdatePin` records a new version after a run with no hard failures.

```bash
pwsh scripts/contract-test.ps1 -Aaes target/release/aaes -Auth subscription   # your normal login, user settings/plugins/MCP left out
pwsh scripts/contract-test.ps1 -Aaes target/release/aaes                       # isolated CLAUDE_CONFIG_DIR + API key ($AAES_ANTHROPIC_API_KEY)
pwsh scripts/contract-test.ps1 -Claude <path to another claude.exe>          # e.g. the build bundled with the desktop app
```

First run: Claude Code 2.1.233, `haiku`, subscription auth: **29/29 checks pass, 0 warnings**, about USD 0.35 as reported by Claude Code. It is not in CI, because it needs a Claude Code login. Run it after each Claude Code update and before a release.

### Runs

| Date | Claude Code | aaes | Result | Cost (USD) | Hook p50 (C5) |
|---|---|---|---|---|---|
| 2026-11-07 | 2.1.233 | 0.1.0 | 29/29 pass, 0 warnings | 0.35 | - |
| 2026-11-14 | 2.1.321 | 0.1.3 | 28/29 hard checks pass, 1 warning | 0.034 | 29.5 ms |

The 2026-11-14 run moved the pin to 2.1.321. No payload shape changed between the two versions:
every hard check passed, and there were no schema-drift warnings. The one warning is C4, where the
sub-agent answered "Cannot confirm from the Read result" although the outline reached it (an 8.6 KB
`hook_additional_context` in the sub-agent transcript, containing the line range it was asked for).
That is the same Haiku instruction-following lapse as C1 in the first run, not a delivery failure.


## Follow-ups
- ~~Automate this contract test as a scripted, version-pinned check (FUN-014).~~ Done 2026-11-07, see above.
- ~~MET-016 Edit-failure instrumentation (`PostToolUseFailure` for Edit/MultiEdit).~~ Done from transcripts instead (2026-11-05): with Claude Code 2.1.233, a failed Edit (`String to replace not found`) fires no `PostToolUseFailure` hook. Bad `old_string` errors come from input validation, before the tool runs. A successful Edit does fire `PostToolUse`. `aaes report` reads the session transcripts.
- ~~SEC-012 encryption at rest and SEC-013 redaction for stored search outputs.~~ Done 2026-11-05.
- First-start latency: `serve --ensure` waited ~2 s for the first health check on a fresh binary (endpoint scanning). Consider not waiting, since hooks fail open until the server is up.
