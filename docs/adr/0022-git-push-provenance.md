# ADR-0022: Attest the pusher of a landed Git ref update

- **Status:** Proposed
- **Date:** 2026-11-20

## Context

What can Cambium say about a commit brought into a repository by Git push?
A single push can move several refs, delete a ref without introducing an
object, or publish a commit someone else authored. Different people can push
the same object to different refs. [ADR-0018](0018-commit-provenance.md)
already defines first-record-wins provenance for API-created commits.
[ADR-0004](0004-agent-first-interfaces.md) separately defines human approval.

Today Cambium authenticates the pusher during `authorize` and rechecks the
grant in `pre_receive`. The pinned Enroute protocol's `PostReceiveRequest`
contains the repository, actor, echoed context and only the ref commands that
landed. Each command carries the ref and optional old and new IDs. It has no
durable delivery ID. Cambium currently ignores internal copy notifications and
uses user notifications for best-effort branch activity.
[Authorization contract](0008-git-traffic-authorization.md) ·
[Hook protocol](../../vendor/enroute/proto/enroute/hook/v1alpha1/hook.proto) ·
[Current policy](../../crates/cambium-server/src/git_policy.rs)

## Options

| Source | What it establishes | Limitation |
| --- | --- | --- |
| Record `pre_receive` or read commit author, signature and trailers | An attempted update, or a statement about a Git object. | A later CAS can refuse the update. An author or signer can differ from the authenticated pusher. Adding a trailer changes the object ID. [Git edge](0007-storage-backend-interface.md) |
| Record verified `post_receive` observations | An authenticated Git grant and a specific update the backend reported landed. | Delivery is best effort. A missing observation cannot identify the pusher or establish that the ref never moved. [ADR-0008](0008-git-traffic-authorization.md#post_receive) |
| Require a durable backend event for each landed update | An adapter could replay from a durable cursor, distinguish repeated moves and report gaps. | Both the event identity and the outcome must be committed at the mutation boundary. The existing hook has neither guarantee. The durable receipt for ephemeral ref creation covers a different operation. [ADR-0015](0015-durable-ref-creation-receipts.md) |

## Decision

Recommend the second option for optional, read-only history. Present a row as
“identity X pushed this ref from A to B in generation G.” Keep the existing commit-provenance
lookup scoped to API writes. Pushing a ref creates no human approval, model
claim, session claim or attestation about the author of any reachable commit.

- Generate a fresh opaque push-attempt ID for the user receive-pack request
  in Cambium's grant context. Bind it to the tenant, logical repository, backend
  incarnation and authenticated identity ID and kind. Preserve the existing
  authorization, pre-receive revalidation and signed hook checks.
- On a verified landed notification, match the backend repository and echoed
  user context to the original grant. Record one row per reported command,
  with the stable generation and backend incarnation, push-attempt ID, command
  position, exact ref name, optional old/new
  IDs, reported force flag, and observation time. The timestamp describes
  Cambium's observation and does not assert a backend commit time. Creates
  have no old ID and deletes have no new ID. Internal copies and refused
  commands produce no user rows.
- Insert the reported batch in a single metadata transaction. Key a row by
  tenant, logical repository, push-attempt ID and command position. Repeated
  delivery of an identical batch is a no-op. Check the immutable fields and
  batch length on duplicate delivery. A conflicting batch raises an internal
  integrity diagnostic and does not overwrite the original. The sequence
  A→B, B→A, A→B from three accepted pushes has three distinct attempt IDs.
- The proposed history writer records the grant that admitted the landed
  update. A token expiring or being revoked after the update does not change
  who pushed it. Recording this fact must never grant fresh repository access.
  Use a signed notification and its bound historical context for this separate
  write. The existing branch-activity path retains its live-token checks.
- Keep observed rows in Cambium's metadata store with the logical repository,
  including across generation switches. Drop late notifications for a deleted
  repository. Repository deletion removes its rows. Exact ref names are
  user-supplied and can themselves contain names or email addresses. They
  remain in history after ref deletion. Collect no separate personal name or
  email fields, credentials, raw hook bodies, commit messages or transcripts. A future
  paginated history operation must authorize current `repo:read` for the
  specific tenant and repository before returning rows.

## Consequences

A push admitted before a generation switch can land in the draining generation
while the active ref has already moved independently. Preserve that original
generation in reads, even if reconciliation later creates a rescue ref.
[Generation reconciliation](0017-durable-reconciliation-progress.md) ·
[Rescue refs](0016-multiple-rescue-values.md)

A push that succeeds while the notification or metadata store is unavailable
can leave a gap. Do not fail a landed Git push, infer the missing pusher from
the current ref, backfill API commit provenance, or make merge policy depend
on a complete list. If a complete audit trail becomes necessary, reconsider
the durable event option with explicit retention and replay guarantees per
adapter. Existing repositories have no historical observations to backfill.

Before implementation is accepted, exercise a partly refused multi-ref push,
a signed duplicate after a lost database acknowledgement, A→B→A→B, tag and ref
deletion, revocation between landing and observation, and internal compaction.
Verify that existing commit provenance and Git object IDs survive unchanged.
This proposal introduces no hook schema, metadata migration or public operation.
