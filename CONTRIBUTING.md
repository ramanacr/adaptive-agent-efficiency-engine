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

CI also runs `cargo deny check` (licenses, advisories, sources; policy in `deny.toml`).

## Releases
1. Set `version` in the root `Cargo.toml` (and the matching `version` of the internal path dependencies), commit, and push.
2. Run the contract test (above) against the release build.
3. Tag and push: `git tag v0.2.0 && git push origin v0.2.0`. The tag must equal the workspace version.
4. The `release` workflow builds all platforms and creates a **draft** GitHub Release with archives, `SHA256SUMS`, an SPDX SBOM and Sigstore build provenance. Check it, then publish it.

`workflow_dispatch` on the `release` workflow runs the same build and packaging without a release (the files are a workflow artifact).

### Windows code signing
The `sign-windows` job signs both Windows binaries (Authenticode, SHA-256, RFC 3161 timestamp) with a Certum SimplySign cloud certificate, using the pinned [`ssign`](https://github.com/Le-Syl21/ssign) CLI. It runs before packaging, so `SHA256SUMS`, the SBOM and the provenance cover the signed files. It is off until the maintainer turns it on:
1. In the repository settings, create the environment `signing`. Limit its deployments to the `main` branch and `v*` tags.
2. Add two environment secrets to `signing`: `CERTUM_EMAIL` (the SimplySign account e-mail) and `CERTUM_OTP` (the TOTP seed behind the SimplySign authenticator, base32 or `otpauth://` URI). The seed lets anyone sign as the certificate holder: never put it anywhere else, and re-issue the SimplySign QR code if it leaks.
3. Add the repository variable `AAES_SIGN_WINDOWS` = `true`.
4. Run the workflow manually and check that the "Verify signatures" step shows `Valid` with a timestamp.

When signing is on, a signing failure stops the release. To switch it off, delete the variable or set it to anything other than `true`. macOS binaries are not signed or notarized.

## Commit and PR conventions
- Small, focused PRs, with requirement/ADR IDs referenced where relevant (e.g. `FUN-022`).
- New parsers need golden fixtures for success, failure and truncated output, under both Bash and PowerShell where the command exists on Windows.
