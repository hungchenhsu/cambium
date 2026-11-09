# ADR-0018: Provenance on commits written through the API

- **Status:** Accepted
- **Date:** 2026-11-10

## Context

[ADR-0004](0004-agent-first-interfaces.md) makes provenance first-class and
optional: a change can carry its agent, model, and session ID. ADR-0001 and
ADR-0010 keep that state in Cambium's metadata store, not in a backend. Phase 1b
adds the fields.

Two kinds of fact are involved. Cambium knows which identity wrote a commit,
because the token proves it ([ADR-0011](0011-tenancy-and-authentication.md)).
It cannot verify which model produced the change or which session it belonged
to; only the client knows. Mixing the two would let a client forge the part that
reviewers and the Phase 3 merge policy must be able to trust.

`write_changeset` creates the commit in the backend before Cambium can record
anything keyed by its ID. A metadata write after the commit can fail, and a
retry of the same request fails with `ref_conflict` once the commit has landed
([ADR-0012](0012-api-transport-and-errors.md)).

Git commit trailers would be atomic with the commit, but they are client text:
anyone can write them, they cannot be redacted, and they change the object ID.

## Decision

- **Scope.** Provenance is recorded for every commit created by
  `write_changeset`. Commits that arrive through git push carry none in this
  version.
- **Attested fields.** Cambium records the writing identity's ID and kind
  (`human` or `agent`) from the authenticated token, and the time. The client
  cannot set them.
- **Claimed fields.** `write_changeset` accepts an optional
  `provenance` object with `model` and `session_id`. Each is 1 to 255 printable
  ASCII characters. They are stored as given and are always presented as claims
  of the writer, never as verified facts.
- **Storage.** One row per tenant, repository, and commit ID in the metadata
  store. The first record for a commit ID wins: an identical commit written again
  later does not replace it. The row is written in a transaction that
  re-authorizes the token for `repo:write` on the repository. Repository deletion
  removes the rows. Compaction does not, because it preserves object IDs; a row
  can outlive a commit that compaction reclaimed.
- **Failure.** Recording happens after the commit lands. A failure never turns
  the applied commit into an error: the response carries
  `provenance_recorded: false`, as `path_collision_check_complete` does for
  warnings. The flag is also false when an identical commit already had a
  record, so it is true only when this request's record was stored. A request
  that is retried after its commit landed fails with `ref_conflict`, as before,
  and records nothing.
- **Reading.** `get_commit_provenance` takes a repository and a full commit ID,
  needs `repo:read`, and returns `not_found` when nothing is recorded.
- **Transcripts** stay out of scope. ADR-0004 makes them opt-in and redactable;
  they need their own decision on retention and redaction.

## Consequences

- Reviewers and later merge policy can rely on who wrote a commit through the
  API, and can see, but must not rely on, which model and session the writer
  claimed.
- A commit can lack provenance: it was pushed with git, its recording failed
  (and the response said so), or its response was lost and the retry returned
  `ref_conflict`.
- Moving a repository to another backend keeps provenance, because the rows are
  keyed by object ID and live in Cambium's store.
- The Postgres store needs the same table under the same migration number.
