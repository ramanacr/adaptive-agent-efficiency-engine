# AAES compatibility

This page lists the Claude Code versions, platforms and toolchains that AAES supports, and how each one was checked. Updated 2026-11-11 for AAES 0.1.x.

## 1. Claude Code versions

AAES depends on the Claude Code hook protocol: the `PostToolUse` input fields and `updatedToolOutput`. Claude Code does not version this protocol separately, so AAES checks it in three ways:

| Check | What it proves | Where | Versions |
|---|---|---|---|
| Live contract test | Shaped text reaches the model, and facts survive (29 checks) | `scripts/contract-test.ps1`, run by hand | **2.1.233** on Windows 11, 2026-11-07: 29/29 pass |
| Observed payload shapes | Every field that AAES reads is present | Tool results in local transcripts (same shape as the hook input) | **2.1.209 to 2.1.321**, 29 versions, Windows, July to November 2026 |
| Offline payload contract | Each observed shape gets the right decision | `tests/contract/payloads.json` + `crates/aaes-runtime/tests/payload_contract.rs`, in CI | The shapes above |

Supported range: **Claude Code 2.1.209 or later**. Older versions are not tested.

The Claude desktop app has its own copy of Claude Code. Its version can be newer than the `claude` CLI on the same machine. For example, on 2026-11-10 the app ran 2.1.321 and the CLI ran 2.1.233. Hooks in desktop sessions run under the app's version. The live contract test checks only the CLI version. The payload shapes of the app's version are covered by the observed shapes and the offline contract.

### 1.1 Fields AAES reads

| Tool | Input fields | Response fields |
|---|---|---|
| all | `hook_event_name`, `session_id`, `cwd`, `agent_id`, `agent_type`, `source` (SessionStart) | |
| Read | `file_path`, `offset`, `limit` | `type`, `file.content`, `file.numLines`, `file.startLine`, `file.totalLines`, `file.truncatedByTokenCap` |
| Grep | `path`, `-A`, `-B`, `-C` | `mode`, `content`, `filenames`, `numFiles` |
| Bash, PowerShell | `command` | `stdout`, `stderr`, `interrupted`, `isImage`, `persistedOutputPath` |

A shaped result replaces only the text field (`file.content`, `content` or `stdout`) and keeps every other field. Claude Code ignores a replacement that does not match the tool's schema, so the offline contract checks that no field is lost.

### 1.2 Optional fields and how AAES handles them

| Field | Seen in | Handling |
|---|---|---|
| `file.truncatedByTokenCap` (Read) | 2.1.217 to 2.1.281 | Pass through (`host_truncated`). Claude Code already cut the file, so an outline would look complete but miss the rest. |
| `persistedOutputPath`, `persistedOutputSize` (Bash, PowerShell) | 2.1.209 to 2.1.308 | Pass through (`host_truncated`). The hook gets only a preview. |
| `backgroundTaskId` (Bash, PowerShell) | 2.1.209 to 2.1.321 | Not a search or a read, so it is recorded as `other` and passes through. |
| Read with `type: image` (`file.base64`) | 2.1.209 to 2.1.321 | Pass through, no event. |
| Grep `mode: count`, `numMatches` | 2.1.260 to 2.1.321 | Pass through (`not_content_mode`). |
| Grep `appliedLimit` | 2.1.209 to 2.1.309 | Shaped like any content result. The totals in the summary line count the lines that Claude Code returned, not the lines the limit cut. |
| `noOutputExpected`, `returnCodeInterpretation`, `timedOutAfterMs`, `gitOperation`, `staleReadFileStateHint` | various | Ignored and kept. |

### 1.3 When Claude Code changes the protocol

AAES fails open. If a required field is missing, it passes the output through unchanged and logs `unexpected tool payload shape` with the field names (never values). Run `aaes logs --errors` to see it.

To add a new Claude Code version:

1. Run the live contract test with the new CLI: `pwsh scripts/contract-test.ps1 -Aaes target/release/aaes -Auth subscription`.
2. If it passes, record the version with `-UpdatePin`.
3. If the version has a new tool-result shape, add a case to `tests/contract/payloads.json` with its `observed` range, and run `cargo test -p aaes-runtime --test payload_contract`.
4. Update the tables on this page.

## 2. Platforms

| Platform | Release archive | CI (build and unit tests) | Live contract test | Hook mode default |
|---|---|---|---|---|
| Windows x64 | yes | yes | yes (2.1.233) | `http` |
| Windows arm64 | yes (cross-built, not run) | no | no | `http` |
| Linux x64 (musl) | yes | yes | no | `command` |
| Linux arm64 (musl) | yes | no (release build runs `aaes --version`) | no | `command` |
| macOS (universal: x64 + arm64) | yes | yes (arm64 runner) | no | `command` |

The open clean-machine tests are macOS, Linux arm64 and Windows arm64. Encryption at rest uses DPAPI on Windows and a mode-0600 key file on macOS and Linux (see the user guide, section 8).

## 3. Rust toolchain

| Toolchain | Check |
|---|---|
| stable | CI: `fmt`, `clippy -D warnings`, `test` on Windows, Linux and macOS |
| 1.85 (MSRV, `rust-version` in `Cargo.toml`) | CI: `cargo check --workspace --locked` |

## 4. Transcript formats (`aaes mine`, `aaes replay`, `aaes report`)

| Agent | `aaes mine` | `aaes replay` | `aaes report` cost and Edit sections |
|---|---|---|---|
| Claude Code 2.1.209 or later | yes | yes | yes |
| Codex CLI | yes | no | no |

These commands read the transcript files only. They fail soft on lines they cannot parse and count them as bad lines.
