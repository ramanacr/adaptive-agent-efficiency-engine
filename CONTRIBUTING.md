# Contributing

Thanks for helping make coding agents cheaper and sharper.

## Ground rules
1. **No private data in this repository, ever.** That means no proprietary source, no real agent transcripts, no raw tool output from non-public repos, and no secrets. Fixtures must be **synthetic** or derived from **public OSS repositories** with their license noted. Keep private benchmark material in the git-ignored `bench-private/` folder. CI runs a secret scan on every push.
2. **Fidelity over compression.** A change that drops an exit code, error, path or line number from a qualifying golden fixture is rejected, however many tokens it saves.
3. **The hot path stays fast.** Hook/run binaries are started once per agent tool call. Avoid heavy start-up work, large dependency trees and async runtimes in hot-path crates. Report cold-start numbers for changes that touch them.
4. `unsafe` is denied workspace-wide. The only exception is a minimal, commented OS call that std can't express (currently two, both Windows: clearing handle inheritance in `aaes-cli/src/ops.rs`, and DPAPI key wrapping in `aaes-runtime/src/crypto.rs`), with a `// SAFETY:` justification.
5. Design changes go through the docs in `Implementation Docs/`. Changes to an ADR or requirement ID need a change-log entry.

## Development
```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
```
CI runs the same on Linux, Windows and macOS with warnings treated as errors.

Changes to hook handling or the hook response format also need the contract test against real Claude Code. It needs a Claude Code login, so it is not in CI:
```bash
pwsh scripts/contract-test.ps1 -Aaes target/release/aaes -Auth subscription
```
See [docs/spikes/M1-contract-test.md](docs/spikes/M1-contract-test.md).

## Commit and PR conventions
- Small, focused PRs, with requirement/ADR IDs referenced where relevant (e.g. `FUN-022`).
- New parsers need golden fixtures for success, failure and truncated output, under both Bash and PowerShell where the command exists on Windows.
