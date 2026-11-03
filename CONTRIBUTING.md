# Contributing

Thanks for helping make coding agents cheaper and sharper.

## Ground rules
1. **No private data in this repository, ever.** That means no proprietary source, no real agent transcripts, no raw tool output from non-public repos, and no secrets. Fixtures must be **synthetic** or derived from **public OSS repositories** with their license noted. Keep private benchmark material in the git-ignored `bench-private/` folder. CI runs a secret scan on every push.
2. **Fidelity over compression.** A change that drops an exit code, error, path or line number from a qualifying golden fixture is rejected, however many tokens it saves.
3. **The hot path stays fast.** Hook/run binaries are started once per agent tool call. Avoid heavy start-up work, large dependency trees and async runtimes in hot-path crates. Report cold-start numbers for changes that touch them.
4. Design changes go through the docs in `Implementation Docs/`. Changes to an ADR or requirement ID need a change-log entry.

## Development
```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
```
CI runs the same on Linux, Windows and macOS with warnings treated as errors.

## Commit and PR conventions
- Small, focused PRs, with requirement/ADR IDs referenced where relevant (e.g. `FUN-022`).
- New parsers need golden fixtures for success, failure and truncated output, under both Bash and PowerShell where the command exists on Windows.
