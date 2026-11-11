# AAES user guide

This guide explains how to install AAES (Adaptive Agent Efficiency Engine) for Claude Code, what it changes for the agent, and how to read its reports. It applies to the M1 preview.

## 1. What AAES does

AAES is a local sidecar. It connects to Claude Code through hooks. After a Read, Grep, Bash or PowerShell tool call, Claude Code sends the tool result to AAES. AAES then does one of two things:

- **Shape** the result: replace a large output with a shorter version that keeps the facts the agent needs, and tell the agent how to get the rest.
- **Pass** the result through unchanged.

The real tool always runs unchanged. If AAES fails or is not running, the agent gets the original output ("fail open").

## 2. Install

You do not need admin rights.

1. Get the binary. Use one of these ways:
   - **Install script (Windows):** downloads the latest release for your CPU, checks its SHA-256 checksum, installs `aaes.exe` into `%LOCALAPPDATA%\Programs\aaes` and adds that folder to your user `PATH`. It also stops a running `aaes serve`, so it works for updates too.
     ```powershell
     & ([scriptblock]::Create((irm https://raw.githubusercontent.com/ramanacr/adaptive-agent-efficiency-engine/main/scripts/install.ps1)))
     ```
   - **Install script (Linux, macOS):** installs into `~/.local/bin` (change it with `AAES_INSTALL_DIR`).
     ```bash
     curl -fsSL https://raw.githubusercontent.com/ramanacr/adaptive-agent-efficiency-engine/main/scripts/install.sh | sh
     ```
   - **Release archive:** download the file for your platform from [GitHub Releases](https://github.com/ramanacr/adaptive-agent-efficiency-engine/releases), check it against `SHA256SUMS`, and put `aaes` (or `aaes.exe`) in a folder on your `PATH`.
   - **`cargo binstall aaes-cli`**, or **build from source** with Rust 1.85 or later: `cargo build --release` (binary: `target/release/aaes(.exe)`).

   `aaes init` warns if the binary's folder is not on `PATH` and prints the command to add it.
2. Optional: verify the build provenance. Each release archive has a Sigstore attestation made by the release workflow. With the GitHub CLI: `gh attestation verify <archive> --repo ramanacr/adaptive-agent-efficiency-engine`. The install scripts run this check when `gh` is installed and signed in.
3. Install the hooks:
   ```bash
   aaes init
   ```
   `aaes init` backs up `~/.claude/settings.json` first (`settings.json.aaes-backup-<time>`) and adds only AAES entries. Your own hooks and settings stay unchanged.
4. Start a new Claude Code session. Hooks load at session start.

**Release platforms.** Windows x64 and arm64, Linux x64 and arm64 (static, musl), macOS universal (Intel and Apple silicon). The binaries are not yet code-signed (no Authenticode signature on Windows, no notarization on macOS). On macOS, a downloaded binary may need `xattr -d com.apple.quarantine aaes` before it runs. The install script uses `curl`, which does not set that flag.

**Hook modes.**

| Mode | Default on | How it works |
|---|---|---|
| `http` | Windows | Claude Code sends hook events to a resident server (`aaes serve`) on `127.0.0.1:47830`. The `SessionStart` hook starts the server if it is not running. A random token in `AAES_HOOK_TOKEN` authenticates each request. |
| `command` | macOS, Linux | Claude Code starts `aaes hook` once per tool call. |

Select a mode with `aaes init --mode http|command`. Use `aaes init --dry-run` to see the result first.

**Remove AAES.** Run `aaes uninstall`. It removes only AAES hooks and the token variable, and stops the server.

## 3. What changes for the agent

### 3.1 Large whole-file reads (off by default)

Read shaping is **off by default**. In the M1 dogfood trial, agents (mostly sub-agents) read the rest of a shaped file within seconds, so read shaping cost more tokens than it saved. Whole-file reads pass through unchanged. `aaes report` still counts them, with the reason `read_shaping_off`.

To try it, set `"read_shaping": true` in `config.json` (section 9). Then it applies to the Read tool and to whole-file shell reads (`cat`, `type`, `Get-Content`).

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

### 3.4 Shadow mode

Shadow mode lets you measure a shaping family on your own work without any risk. For each family in the `shadow` list (`"read"`, `"search"`), AAES computes the shaped output and records its size, but the agent gets the original output. The event has the action `shadow`, and `aaes report` shows a "Shadow mode" section with the tokens that shaping would have saved, also cost-weighted (section 6.3).

```json
{"shadow": ["read"]}
```

Shadow mode wins over `read_shaping`. The agent sees everything, so there are no follow-ups: the shadow saving is an upper bound. To measure follow-ups, turn the family on for real.

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
| `shadow` | Shadow mode (section 3.4): AAES computed the shaped output but delivered the original. |

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

### 6.3 Cache-aware cost

Token counts treat every token the same. The model price does not. A tool result stays in the context, so every later request until compaction sends it again. With prompt caching, the first of these requests writes it to the cache and the later requests read it from the cache, at about a tenth of the price.

So the report also shows each family's saving in **uncached-input-equivalent tokens**. AAES finds, in the Claude Code transcript of the session, how many requests came after each shaped result and before the next compaction (`later requests`). A saved token then counts as `w_write + (later requests − 1) × w_read`, with the weights in the heading: cache write ×1.25 (5-minute cache) or ×2 (1-hour cache, mixed as observed in the session) and cache read ×0.1. Follow-up reads are weighted the same way and subtracted.

| Column | Meaning |
|---|---|
| `saved` | Cost-weighted tokens saved by shaped results |
| `follow-ups` | Cost-weighted tokens of follow-up reads |
| `net` | `saved` minus `follow-ups` |
| `later requests` | Average number of requests that carried a shaped result |
| `matched` | Events matched to a transcript / all shaped and follow-up events. Events without a transcript (deleted, or another agent) are not counted. |

The units are relative to the uncached input price, so they apply to any model. On a subscription plan they are a proxy for usage-limit consumption, because plans do not publish limits in tokens. With `--json`, each family has a `cost_weighted` object and the weights are in `cost_weights`.

### 6.4 By repo index (CodeGraph)

Some agent setups tell the agent to use CodeGraph (a code-graph index with an MCP tool and a `codegraph` CLI) instead of grep and whole-file reads when the repo has an index. AAES does not hook CodeGraph's MCP tool, and it passes `codegraph` shell output through unchanged. So in an indexed repo, AAES sees fewer calls it can shape.

To make this visible, AAES records for each event whether the working folder is inside a CodeGraph-indexed repo (`.codegraph/codegraph.db` in the folder or a parent folder). The report then shows calls, shaped results, saved tokens, follow-ups and net tokens for each group:

| Row | Meaning |
|---|---|
| `codegraph` | The working folder is in an indexed repo |
| `no index` | No index found |
| `unknown` | Events recorded before AAES stored this flag |

Lower savings in the `codegraph` row are expected: the agent used CodeGraph instead. AAES checks each folder once per server run, so after `codegraph init`, restart the server (`aaes serve --stop`) to update the flag.

### 6.5 Edit failures (MET-016)

This section shows the share of Edit/MultiEdit calls that failed, for example because the `old_string` was not found. A failed Edit can mean that the agent guessed text it did not see.

AAES reads these numbers from the Claude Code transcripts of the kept sessions (`~/.claude/projects`), not from hooks. Claude Code does not send a hook event for most Edit failures, because it rejects a bad `old_string` before the tool runs. AAES counts only edits made while it was active in the session, and keeps only the failure class, never the error text.

| Row | Meaning |
|---|---|
| `all` | All Edit/MultiEdit calls |
| `after shaped read` | Edits of a file that AAES showed shaped earlier in the session |
| `other files` | All other edits |

Compare the failure rate of `after shaped read` with `other files`. A clearly higher rate after shaped reads means read shaping hides text the agent needs. Failure classes: `not_found`, `not_unique`, `not_read`, `stale`, `no_change`, `other`. Edits the user rejected are counted separately.

### 6.6 By repo override

This table appears when `config.json` has per-repo overrides (section 9.1). It shows, for each repo and family:

- `shaped`: the shaped results.
- `net tok`: the net saving in tokens, after follow-ups.
- `net weighted`: the same net saving, cost-weighted (section 6.3).
- `per shaped`: follow-ups per shaped result.
- `shadow weighted`: the would-be saving in shadow mode.

The `advice` column says what to do with the family in that repo:

| Advice | Meaning |
|---|---|
| `collecting data` | Fewer than 10 shaped or shadow results |
| `keep` | The net saving is positive (cost-weighted, when the transcripts match) with at most 1 follow-up per shaped result |
| `turn off or raise the threshold` | The family costs more than it saves in this repo |
| `try it on (shadow saving is an upper bound)` | The family is in shadow mode and would save tokens |
| `no gain in shadow mode` | Shadow mode shows no saving |

AAES does not change the settings by itself. You decide, and you change `config.json`.

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

**Encryption at rest.** Stored outputs are encrypted with ChaCha20-Poly1305. The key is a random 256-bit key in `artifact-key`. The OS protects the key:

| OS | Where the key is |
|---|---|
| Windows | In `artifact-key`, encrypted by DPAPI for the current user. |
| macOS | In the login Keychain (item `aaes`, account `artifact-key-<id>`). `artifact-key` holds only the id. |
| Linux | In the kernel user keyring (`keyctl`, description `aaes:artifact-key:<id>`). `artifact-key` holds only the id. No desktop keyring service is needed, so this also works on servers and in WSL 2. The kernel drops the keyring at reboot (or when your last session ends). AAES then makes a new key, and older handles read as expired. |

If no keystore is usable (for example in a container that blocks `keyctl`, or with a locked or missing Keychain), the key stays in `artifact-key`, readable only by your user (mode 0600). AAES moves it into the keystore when one becomes usable. To keep the key in the file, set `AAES_KEY_STORE=file`.

The keystore protects the key in a disk image or a backup of your home folder. It does not protect against programs that run as your user. The Keychain item allows any application of your user to read it, so an AAES upgrade never shows a Keychain prompt.

If the key cannot be read (for example after a profile reset, or on another machine), AAES makes a new key, and older handles read as expired. If the keystore cannot answer at the moment (for example a locked Keychain), AAES keeps the key and passes outputs through unchanged until it can.

**Retention.** AAES deletes a session's state and stored outputs `retention_hours` after the session's last activity (default 24).

**Network.** AAES sends no telemetry. In `http` mode the server listens only on `127.0.0.1`.

**Limits.** Encryption protects against other OS users and against disk or backup leaks. It does not protect against a hostile process that runs as your own user. See the [threat model](../Implementation%20Docs/06_THREAT_MODEL.md).

## 9. Configuration

Put an optional `config.json` in the data folder. Missing keys keep their defaults.

| Key | Default | Meaning |
|---|---|---|
| `read_shaping` | false | Shape large whole-file reads (section 3.1) |
| `shadow` | `[]` | Families in shadow mode: `"read"`, `"search"` (section 3.4) |
| `read_threshold_tokens` | 2000 | Shape whole-file reads above this size (when `read_shaping` is on) |
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
| `repos` | `{}` | Per-repo overrides (section 9.1) |

Example:

```json
{"read_threshold_tokens": 6000, "search_max_lines_per_file": 20, "log_level": "info"}
```

### 9.1 Per-repo overrides

Different repos need different settings. For example, read shaping can pay off in a repo with large generated files and lose tokens in another one. Use `repos` to change settings for one repo:

```json
{
  "shadow": ["search"],
  "repos": {
    "D:/Work/big-app": { "read_shaping": true, "read_threshold_tokens": 4000 },
    "D:/Work/big-app/docs": { "shadow": ["read"] },
    "D:/Work/small-lib": {}
  }
}
```

- The key is the repo's root folder. Case and `/` or `\` do not matter.
- A tool call uses the override whose root is its working folder or the closest folder above it. In the example, `D:/Work/big-app/docs` wins over `D:/Work/big-app` for calls in `docs`.
- An override accepts these keys: `read_shaping`, `shadow`, `read_threshold_tokens`, `head_lines`, `search_threshold_tokens`, `search_max_lines_per_file` and `search_min_parsed`. Keys that you leave out keep the global value.
- Events from a repo with an override carry the override's key. `aaes report` then shows that repo in its own table (section 6.6). An empty override (`{}`) changes nothing, so use it to get the per-repo numbers only.
- `aaes replay` uses the same overrides, so you can see the effect of a threshold before you change it in the live hook.

The steps to tune a repo:

1. Run `aaes replay` and look at the top projects. They show where a family would save the most.
2. Add an override for the repo with that family in shadow mode, for example `{"shadow": ["read"]}`.
3. After some sessions, read the repo's advice in `aaes report`.
4. If the advice says "try it on", turn the family on for that repo only (for example `"read_shaping": true`, without `shadow`).
5. Keep it on only while the advice says "keep". If it says "turn off or raise the threshold", do one of those.

The server reads `config.json` when it starts. After a change, restart it: `aaes serve --stop`. The next Claude Code session starts it again.

## 10. Turn shaping off

| Method | Scope |
|---|---|
| `aaes disable` / `aaes enable` | All sessions, at once. Hooks stay installed; every result passes through. |
| `AAES_RAW=1` | `command` mode only: set it in the environment of Claude Code. |
| `AAES_KEY_STORE=file` | macOS and Linux: keep the artifact key in the 0600 file, not in the OS keystore. |
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

## 13. Replay the shapers on your history: `aaes replay`

`aaes replay` runs the current shapers over the tool results in your past Claude Code transcripts (read-only). It shows what each family would have saved. You do not need hooks installed, and nothing is changed or stored.

Claude Code stores each tool result in the transcript in the same form that the hook gets (`toolUseResult`). Replay sends it through the same decisions as the hook:

- A ranged read (`offset`/`limit`) passes through.
- A second whole read of the same file in one context passes through in full. This rule resets at compaction.
- Shell output that Claude Code saved to a file (`<persisted-output>`) passes through.
- The size thresholds are the same as in the hook.

```
aaes replay                        # thresholds from config.json
aaes replay --defaults             # built-in thresholds
aaes replay --since 2026-10-01 --project my-repo
aaes replay --json
```

The report has three parts:

1. **Families.** For `read`, `shell_read` and `search`, the report shows these columns:
   - `results`: the results replay looked at.
   - `would shape`: the results that the shaper would change.
   - `raw tok` and `saved tok`: the raw tokens and the saved tokens.
   - `cost-weighted`: the saving weighted by cache price, as in section 6.3.
   - `later req`: the average number of later requests that resend each shaped result.
2. **Pass-through reasons**: `small`, `repeat`, `ranged`, `unparsed`, `low saving`, `host truncated` and `shaped live`. `shaped live` counts results that AAES already shaped when they happened. The transcript holds only the shaped text, so their savings are in `aaes report`, not here.
3. **Top projects** by cost-weighted saving. Use this table to find the repos where a family is worth turning on.

The figures are an **upper bound**. In the history, the agent saw the full output, so the follow-up reads that a shaped result can cause are not counted. Compare with the follow-up figures in `aaes report`, or try the family in shadow mode first (section 3.4). Replay reads Claude Code transcripts only, not Codex transcripts.
