# Agent adapter (Codex, Cursor, Gemini, and other coding agents)

This file is a thin adapter. The canonical entry point is `CLAUDE.md` at the repository root. Read it in full first, then read the files it points to for your task.

- Do not copy rules into this file; change `CLAUDE.md` instead. The review guidelines below are the one exception, because Codex code review reads only this file.
- Do not create skill copies under `.agent/`, `.codex/`, or `.cursor/`.

## Review guidelines

- Report only problems you can reproduce. State the triggering input or state and the wrong result. Skip edge cases that real inputs cannot reach.
- **P0/P1:**
  - data loss or corruption
  - a git hash that changes when a repository moves between storage backends
  - one tenant able to read or write another tenant's data
  - any path that lets a request reach a storage backend without Cambium's authorization
  - an expired ephemeral branch whose data is not cleaned up
  - weakened authentication or authorization
  - an agent approval counted as a human approval
  - machine output that changes with locale
  - an Apache-2.0 crate that depends on an AGPL-3.0-only crate
  - code copied from a GPL-licensed project
  - committed secrets or personal data
  - a breaking change to a published API or error code
- **P2:** real correctness problems with limited impact.
- Do not comment on wording, naming, formatting, style preferences, or readability refactors. On documentation changes, flag only factual contradictions with code or other authoritative documents, and broken links or paths.
- From the second round on, review only what new commits change. Do not reopen resolved threads unless a new commit makes the problem real again.
- Write reviews in English.
