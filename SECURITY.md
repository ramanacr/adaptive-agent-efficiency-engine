# Security Policy

AAES runs on every tool call a coding agent makes and stores raw tool output, so we treat security reports as top priority.

## Reporting a vulnerability
Please **do not open a public issue**. Report privately via GitHub's [private vulnerability reporting](https://github.com/ramanacr/adaptive-agent-efficiency-engine/security/advisories/new).

Include affected version/commit, OS, agent (Claude Code / Codex / MCP) and version, reproduction steps, and impact. We aim to acknowledge within 3 business days.

## Scope
See [`Implementation Docs/06_THREAT_MODEL.md`](Implementation%20Docs/06_THREAT_MODEL.md) for what each isolation tier does and does **not** protect against. In particular, the Standard tier is not a security boundary against a hostile agent running as the same OS user. Reports showing a gap between the documented guarantees and actual behaviour are in scope.

## Supported versions
Pre-1.0: only the latest release receives fixes.
