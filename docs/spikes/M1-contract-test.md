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

## Follow-ups
- Automate this contract test as a scripted, version-pinned check (FUN-014: a shape mismatch is silently ignored by Claude Code).
- MET-016 Edit-failure instrumentation (`PostToolUseFailure` for Edit/MultiEdit).
- SEC-012 encryption at rest and SEC-013 redaction for stored search outputs.
- First-start latency: `serve --ensure` waited ~2 s for the first health check on a fresh binary (endpoint scanning). Consider not waiting, since hooks fail open until the server is up.
