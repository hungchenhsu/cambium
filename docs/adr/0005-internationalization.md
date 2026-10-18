# ADR-0005: Internationalization

- **Status:** Accepted
- **Date:** 2026-10-18

## Context

Cambium has three kinds of text: output for machines, output for humans, and user data such as file paths, commit messages, and timestamps. Each needs different treatment. Version control also has data-layer pitfalls that interface translation does not solve.

## Decision

1. **Machine output is never translated.** See ADR-0004, principle 2.
2. **Human-facing text uses message catalogs from the first string.** This covers CLI human-readable output, a future web dashboard, and documentation. English is the default locale and Traditional Chinese (zh-TW) is the second. For Rust, Project Fluent (`fluent-rs`) is the leading candidate, to be confirmed when the first human-facing output is written. Everything committed to the repository is in English: code, comments, documentation, commit messages, and pull request text. The only exception is the README, which has translations (`README.<locale>.md`, starting with `README.zh-TW.md`). A README translation is updated in the same change as the English README.
3. **The data layer preserves user data exactly.**
   - Paths are byte strings, as in git. They are not guaranteed to be UTF-8, and APIs must be able to represent paths that are not.
   - Cambium passes paths through exactly as received and never Unicode-normalizes them silently. The server detects paths that differ only by normalization (NFC vs NFD) or by case, and reports them, because they collide on macOS and Windows filesystems.
   - Commit timestamps keep their timezone offset.
   - Search and indexing must handle CJK text, which is not separated by spaces.

## Consequences

- A test suite with non-UTF-8, NFD, and case-colliding paths is needed before phase 1 ships.
- Adding a locale requires no code change, only a new catalog.
