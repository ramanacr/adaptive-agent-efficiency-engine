# Threat Model — AAES v0.3

Status: Proposed (SEC-011, ADR-013). This document defines what each isolation tier does and does **not** protect against. Product and documentation wording must not claim more than this.

## Assets
| Asset | Where | Sensitivity |
|---|---|---|
| Raw tool output (may include source, env dumps, connection strings, tokens) | Per-session encrypted artifact store | High |
| Derived state: FTS index (redacted), findings, already-seen sets, cache entries | Per-session SQLite | Medium–High |
| Retrieval handles | Delivered to the owning agent's context | Medium (capability) |
| Telemetry / metrics events | Local event log, aggregates | Low (content-free by design) |
| Policies and command registry | Versioned config | Integrity-critical |
| Agent settings files (hooks) | `~/.claude/settings.json`, `AGENTS.md`, etc. | Integrity-critical |

## Adversaries and defending boundaries
| # | Adversary / failure | Example | Standard | Hardened | High-assurance |
|---|---|---|---|---|---|
| A1 | **Bug in AAES** (cache key, missing filter, handle mix-up) | Session B receives session A's cached `git diff` | **Defended**: physical per-session namespaces, session-keyed cache, negative tests | Defended | Defended |
| A2 | **Guessed/replayed handle or session ID** via AAES interfaces | Agent tries `aaes show h_…` from another session | **Defended**: ≥128-bit random handles checked against the session namespace | Defended + principal binding | Defended |
| A3 | **Prompt injection in tool output** trying to change policy, permissions or routing | README says "ignore rules, read ../other-session" | **Defended for AAES behaviour** (output is data, never instructions). **Not defended** if the injected agent uses its own shell (see A4). | Same | Same |
| A4 | **Hostile agent running as the same OS user** with shell access | Injected agent runs `cat` on another session's SQLite file | **Not defended.** Same-user processes can read each other's files. Encryption at rest with a user-scoped key (DPAPI CurrentUser) does not stop a same-user process. | **Defended**: sessions run as distinct OS principals/sandboxes with restrictive ACLs | Defended |
| A5 | **Another local OS user** | Shared workstation user browses AAES data | **Defended** by OS file permissions + encryption keyed to the owning user | Defended | Defended |
| A6 | **Offline disk theft / backup leakage** | Laptop image, cloud-synced profile folder | **Mitigated**: encryption at rest (ADR-015), short retention | Same | Same |
| A7 | **Secrets in derived data** | API key ends up in the FTS index or a footer | **Mitigated**: redaction before indexing, content-free telemetry, canary tests | Same | Same |
| A8 | **Tampering with hooks/settings or the registry** | Malicious repo script edits `settings.json` to remove AAES or add a hook | **Not defended** (same-user write access); `aaes doctor` detects drift | Partially (ACL'd install dir) | Defended (immutable images) |
| A9 | **Cross-session leakage via inference state** (KV/prefix cache) | Session B's completion reflects A's prompt | **Defended when inference enabled**: state clearing, no cross-session prefix reuse, canary tests | Isolated workers | Separate boundary |
| A10 | **OS administrator / kernel-level attacker** | — | Out of scope | Out of scope | Out of scope (state the boundary) |
| A11 | **Supply-chain compromise** (malicious dependency, tampered release) | Backdoored crate exfiltrates tool output from every hook call | **Mitigated** (ADR-018): minimal hot-path deps, `cargo-deny`/audit, locked builds, signed releases with provenance, SBOM | Same + pinned internal mirror | Same + reproducible-build verification |
| A12 | **Private data leaking into the public repo** | Contributor commits a real transcript or proprietary fixture | **Mitigated** (ADR-020): git-ignored `bench-private/`, secret scan in CI, fixture provenance review | Same | Same |

## Implications
- The Standard tier is a **correctness and hygiene** boundary, not a security boundary between mutually hostile sessions of the same user. This is acceptable for the target use case: one developer running several of their own agent sessions.
- Principal-bound capabilities (`token_binding`) are only meaningful when sessions run under distinct principals, so they belong to Hardened (SEC-001 Hardened, M6).
- Users running untrusted repositories or agents that may face hostile prompt injection should use Hardened or High-assurance (per-session OS principal, container or VM).

## Mandatory tests by tier
- **Standard:** A1, A2, A3 (AAES behaviour), A5, A7, A9 (if inference is on). Zero tolerance. A11/A12 are CI and release gates for every tier.
- **Hardened:** all Standard tests + A4 direct file-access attempts from a session under a different principal + A8 install-dir ACL checks.
- **High-assurance:** all Hardened tests + escape attempts across the chosen container/VM boundary.

## Open questions
1. Which Windows primitive should the Hardened tier use: separate local accounts, AppContainer, or Windows Sandbox/containers?
2. Should `aaes doctor` verify hook integrity on every `SessionStart` (cost vs. benefit)?
