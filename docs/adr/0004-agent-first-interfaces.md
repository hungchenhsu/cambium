# ADR-0004: Agent-first interface principles

- **Status:** Accepted
- **Date:** 2026-10-18

## Context

Cambium's primary users are AI agents and automated jobs. Humans are secondary users. Agents retry, run in parallel, pay per token, and parse output. Tools that parse git's human-readable output break when the output changes with locale or version.

## Decision

1. **API and MCP come first.** Every operation is a structured API call. MCP exposes the same operations. The CLI is a thin client over the same API.
2. **Machine output is stable and locale-independent.** Responses are JSON. Errors carry a stable `snake_case` code, an English message, and structured details. Machine output is never translated and never depends on `LANG` or `LC_*`.
3. **Mutations are idempotent.** Every mutating call accepts an idempotency key.
4. **Queries need no checkout.** Log, blame, path history, file-at-revision, diff, and search are API calls.
5. **Writes need no checkout.** A client can submit a changeset of paths and contents, and the server creates the commit.
6. **Workspaces are cheap and ephemeral.** Branches are cheap to create, carry a TTL, and are cleaned up automatically, including the storage they used.
7. **No global locks.** Ref updates are compare-and-swap. Conflicts come back as structured data, not as conflict markers in text.
8. **Identity separates humans from agents.** This lives in the data model from the start, although enforcement is a phase 3 feature. An agent's approval never satisfies a requirement for human approval. An approval applies to one specific commit. Tokens are short-lived and scoped.
9. **Provenance is first-class and optional.** A change can carry its agent, model, and session ID. Storing transcripts is opt-in, and transcripts can be redacted, because they may contain secrets.
10. **Clients subscribe instead of polling.** Repository events are available as streams or webhooks.
11. **Token cost is a design metric.** Output is compact and paginated by default.

## Consequences

- API design comes before CLI design. CLI features that the API cannot express are not allowed.
- Error codes are part of the public contract. Renaming or removing a code is a breaking change.
- Identity, tenancy, and provenance fields exist in the schema before the features that use them.
