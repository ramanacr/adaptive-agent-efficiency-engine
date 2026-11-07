# AAES user guide

This guide explains how to install AAES (Adaptive Agent Efficiency Engine) for Claude Code, what it changes for the agent, and how to read its reports. It applies to the M1 preview.

## 1. What AAES does

AAES is a local sidecar. It connects to Claude Code through hooks. After a Read, Grep, Bash or PowerShell tool call, Claude Code sends the tool result to AAES. AAES then does one of two things:

- **Shape** the result: replace a large output with a shorter version that keeps the facts the agent needs, and tell the agent how to get the rest.
- **Pass** the result through unchanged.

The real tool always runs unchanged. If AAES fails or is not running, the agent gets the original output ("fail open").

## 2. Install

AAES is built from source in the M1 preview. You need Rust 1.85 or later.

1. Build the binary:
   ```bash
   cargo build --release            # binary: target/release/aaes(.exe)
   ```
2. Copy `aaes` (or `aaes.exe`) to a folder on your `PATH`. `aaes init` warns if the folder is not on `PATH` and prints the command to add it.
3. Install the hooks:
   ```bash
   aaes init
   ```
   `aaes init` backs up `~/.claude/settings.json` first (`settings.json.aaes-backup-<time>`) and adds only AAES entries. Your own hooks and settings stay unchanged.
4. Start a new Claude Code session. Hooks load at session start.

**Hook modes.**

| Mode | Default on | How it works |
|---|---|---|
| `http` | Windows | Claude Code sends hook events to a resident server (`aaes serve`) on `127.0.0.1:47830`. The `SessionStart` hook starts the server if it is not running. A random token in `AAES_HOOK_TOKEN` authenticates each request. |
| `command` | macOS, Linux | Claude Code starts `aaes hook` once per tool call. |

Select a mode with `aaes init --mode http|command`. Use `aaes init --dry-run` to see the result first.

**Remove AAES.** Run `aaes uninstall`. It removes only AAES hooks and the token variable, and stops the server.

## 3. What changes for the agent

### 3.1 Large whole-file reads

This applies to the Read tool and to whole-file shell reads (`cat`, `type`, `Get-Content`).

If a file is above `read_threshold_tokens`, the agent gets:

- the first 40 lines, verbatim;
- an outline of the rest of the file (classes, functions, sections) with exact line ranges;
- a note that says how to get exact text.

The agent gets more of the file in one of two ways:

- Read a range with `offset`/`limit`. Ranged reads always pass through.
- Read the same whole file again in the same context. The second read returns the full file ("ask twice, get full").

### 3.2 Large search results

This applies to the Grep tool and to `grep`, `rg`, `git grep` and `Select-String` in a shell.

If a search result is above `search_threshold_tokens`, the agent gets:

- results grouped by file, with paths relative to the working directory;
- exact match totals per file and overall;
- at most `search_max_lines_per_file` lines per file;
- a handle for the lines that were left out, for example `aaes show h_abc…`.

AAES shapes a search only if at least `search_min_parsed` of its lines look like grep output. Lines that do not parse are kept verbatim, in order, after the grouped part.

Output of a search in one file has no paths (`12:text` lines, from the Grep tool on one file or `grep -n pattern file`). AAES shapes it as one group, labelled with the file path when the Grep tool gives it, else `(single file)`.

### 3.3 Everything else

All other tool results pass through unchanged. AAES never changes tool input, and never changes Edit, Write or other tool results.

## 4. How the agent learns about AAES

The agent does not call AAES directly. AAES communicates only through text in the agent's context:

1. **Session notice.** At session start, AAES adds a short notice (about 150 tokens). It says that large reads and searches are shaped and how to get the full text. It also gives the full path of the `aaes` binary. Set `session_guidance` to `false` to turn the notice off.
2. **Footer on each shaped result.** A shaped result always says what was left out and how to get it.

The agent replies only with normal tool calls: a ranged Read, a second whole read, or `aaes show` in a shell. An agent without a shell tool cannot run `aaes show`, but it can still use ranged and repeated reads.

## 5. Get stored output: `aaes show`

```bash
aaes show <handle>                  # the full stored output
aaes show <handle> --lines 120-180  # a line range
aaes show <handle> --grep Handler   # lines that contain a substring (case-insensitive)
aaes show <handle> --head 50
aaes show <handle> --tail 50
```

A handle is a 128-bit random value. It is valid until the session's data is deleted (see section 8). After that, `aaes show` reports `unknown or expired handle`.

## 6. Read the report: `aaes report`

`aaes report` summarises the local event logs of the sessions that are still kept. Add `--json` for machine-readable output. The report contains numbers only, never file content, paths or commands.

### 6.1 Rows by kind and action

| Column | Meaning |
|---|---|
| `kind` | `read` (Read tool), `shell_read` (`cat`/`Get-Content`), `search`, `other` (shell commands AAES does not shape) |
| `action` | `shaped`, `pass` or `full_on_repeat` (see below) |
| `count` | Number of tool results |
| `raw tok` | Estimated tokens of the original output |
| `delivered` | Estimated tokens the agent received |

| Action | Meaning |
|---|---|
| `shaped` | AAES replaced the output with the shorter version. The saving is `raw tok` minus `delivered`. |
| `pass` | AAES delivered the output unchanged. |
| `full_on_repeat` | The agent read the same whole file a second time in the same context, and AAES delivered it in full. |

The diagnostic log (section 7) records the reason for each `pass`:

| Reason | Meaning |
|---|---|
| `small` | Below the threshold. Shaping is not worth it. |
| `ranged` | A Read with `offset`/`limit`. It is already targeted. |
| `not_handled` | A shell command that AAES does not shape (for example `git push`). |
| `unparsed` | A search output with too few grep-style lines. |
| `low_saving` | Grouping the search would save too little. |
| `not_content_mode` | A Grep call in files-only or count mode. |

Token numbers are estimates: bytes divided by `bytes_per_token` (default 2.4).

### 6.2 Follow-ups and net saving (MET-015)

A **follow-up** is a read of a file within `followup_window_secs` (default 5 minutes) after AAES shaped it in the same context. A read after that is a new need, not a follow-up. Follow-ups are expected: the outline points the agent to a section. But they cost tokens, so the report subtracts them:

- **net tok** = saved tokens minus follow-up tokens;
- **per shaped** = follow-up calls per shaped result.

The MET-015 verdict for a family (`read`, `search`) is `ok` when net tok is above 0 **and** there is at most 1 follow-up per shaped result. Otherwise the report says `review: raise the threshold or disable this family`.

### 6.3 By repo index (CodeGraph)

Some agent setups tell the agent to use CodeGraph (a code-graph index with an MCP tool and a `codegraph` CLI) instead of grep and whole-file reads when the repo has an index. AAES does not hook CodeGraph's MCP tool, and it passes `codegraph` shell output through unchanged. So in an indexed repo, AAES sees fewer calls it can shape.

To make this visible, AAES records for each event whether the working folder is inside a CodeGraph-indexed repo (`.codegraph/codegraph.db` in the folder or a parent folder). The report then shows calls, shaped results, saved tokens, follow-ups and net tokens for each group:

| Row | Meaning |
|---|---|
| `codegraph` | The working folder is in an indexed repo |
| `no index` | No index found |
| `unknown` | Events recorded before AAES stored this flag |

Lower savings in the `codegraph` row are expected: the agent used CodeGraph instead. AAES checks each folder once per server run, so after `codegraph init`, restart the server (`aaes serve --stop`) to update the flag.

### 6.4 Edit failures (MET-016)

This section shows the share of Edit/MultiEdit calls that failed, for example because the `old_string` was not found. A failed Edit can mean that the agent guessed text it did not see.

AAES reads these numbers from the Claude Code transcripts of the kept sessions (`~/.claude/projects`), not from hooks. Claude Code does not send a hook event for most Edit failures, because it rejects a bad `old_string` before the tool runs. AAES counts only edits made while it was active in the session, and keeps only the failure class, never the error text.

| Row | Meaning |
|---|---|
| `all` | All Edit/MultiEdit calls |
| `after shaped read` | Edits of a file that AAES showed shaped earlier in the session |
| `other files` | All other edits |

Compare the failure rate of `after shaped read` with `other files`. A clearly higher rate after shaped reads means read shaping hides text the agent needs. Failure classes: `not_found`, `not_unique`, `not_read`, `stale`, `no_change`, `other`. Edits the user rejected are counted separately.

## 7. Diagnostics: `aaes logs`

```bash
aaes logs                 # last 50 lines of today's log
aaes logs --errors        # warnings and errors only
aaes logs --tail 200 --all
aaes logs --raw           # JSON lines
```

Each tool result has one `decision` line with the action, reason, sizes and handler time. The log contains structural facts only: sizes, line numbers, file extensions, command shapes and short hashes. It never contains file content, paths, search patterns or command arguments.

Set `"log_level": "debug"` in `config.json`, or the `AAES_LOG` environment variable, for more detail.

## 8. Data, privacy and security

**Data folder.**

| OS | Folder |
|---|---|
| Windows | `%USERPROFILE%\.aaes` |
| macOS, Linux | `$XDG_STATE_HOME/aaes`, else `~/.local/state/aaes` |
| Any | `$AAES_HOME`, if set |

On Windows the folder is outside `AppData` on purpose. Apps installed as MSIX packages, such as the Claude desktop app, have their `AppData` writes redirected to a private copy.

**Contents.**

| Item | Path | Notes |
|---|---|---|
| Configuration | `config.json` | Optional (section 9) |
| Hook token | `hook-token` | Used by `http` mode |
| Encryption key | `artifact-key` | See below |
| Session state and events | `sessions/<id>/` | Content-free |
| Stored outputs | `sessions/<id>/artifacts/*.bin` | Encrypted |
| Logs | `logs/aaes-YYYY-MM-DD.jsonl` | Kept 14 days |

**Encryption at rest.** Stored outputs are encrypted with ChaCha20-Poly1305. The key is a random 256-bit key in `artifact-key`. On Windows, DPAPI protects the key for the current user. On macOS and Linux, the key file is readable only by your user (mode 0600). Keychain and libsecret support are planned. If the key cannot be read (for example after a profile reset, or on another machine), AAES makes a new key, and older handles read as expired.

**Retention.** AAES deletes a session's state and stored outputs `retention_hours` after the session's last activity (default 24).

**Network.** AAES sends no telemetry. In `http` mode the server listens only on `127.0.0.1`.

**Limits.** Encryption protects against other OS users and against disk or backup leaks. It does not protect against a hostile process that runs as your own user. See the [threat model](../Implementation%20Docs/06_THREAT_MODEL.md).

## 9. Configuration

Put an optional `config.json` in the data folder. Missing keys keep their defaults.

| Key | Default | Meaning |
|---|---|---|
| `read_threshold_tokens` | 2000 | Shape whole-file reads above this size |
| `head_lines` | 40 | Verbatim lines at the start of a shaped read |
| `search_threshold_tokens` | 1500 | Shape search results above this size |
| `search_max_lines_per_file` | 20 | Lines shown per file in a shaped search |
| `search_min_parsed` | 0.7 | Minimum share of grep-style lines (0.5–1.0) |
| `bytes_per_token` | 2.4 | Token estimate in reports |
| `followup_window_secs` | 300 | A read of a shaped file counts as a follow-up only within this time |
| `retention_hours` | 24 | Delete session data this long after last activity |
| `session_guidance` | true | Add the session notice |
| `log_level` | `info` | `error`, `warn`, `info` or `debug` |
| `port` | 47830 | Server port for `http` mode |

Example:

```json
{"read_threshold_tokens": 6000, "search_max_lines_per_file": 20, "log_level": "info"}
```

The server reads `config.json` when it starts. After a change, restart it: `aaes serve --stop`. The next Claude Code session starts it again.

## 10. Turn shaping off

| Method | Scope |
|---|---|
| `aaes disable` / `aaes enable` | All sessions, at once. Hooks stay installed; every result passes through. |
| `AAES_RAW=1` | `command` mode only: set it in the environment of Claude Code. |
| `aaes uninstall` | Removes the hooks. |

## 11. Troubleshooting

| Problem | What to do |
|---|---|
| No shaping at all | Run `aaes logs --errors`. In `http` mode, check that the server runs: `aaes serve --ensure`. Make sure you started a new session after `aaes init`. |
| `aaes` not found | Add its folder to `PATH`, or use the full path from the session notice. |
| Cannot replace `aaes.exe` (file in use) | Run `aaes serve --stop` first, copy the new binary, then start a new session. |
| `unknown or expired handle` | The session data was deleted (retention) or the key changed. Run the tool again. |
| Output looks wrong | Run `aaes disable`, repeat the tool call, and compare. Report the `decision` line from `aaes logs` (it holds no content). |
| Server port in use | Set `port` in `config.json` and run `aaes init` again. |

## 12. Measure first: `aaes mine`

`aaes mine` reads your existing Claude Code and Codex transcripts (read-only). It reports where tool-output tokens go, so you can see if AAES is worth trying. The report ends with a web-search section (MET-017): how many `WebSearch` calls you made, how many were duplicates, how often searches came in bursts, and how often the provider's rate limit was hit. It shows counts only, never query text. See the [README](../README.md#quick-start-measure-your-own-sessions-m-1) and `aaes mine --help`.
