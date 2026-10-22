# ADR-0013: Seal an old generation before final reconciliation

- **Status:** Accepted; amends ADR-0006, ADR-0007, and ADR-0009 as specified below; reconciliation progress and recovery ordering are amended by [ADR-0017](0017-durable-reconciliation-progress.md)
- **Date:** 2026-10-23

## Context

[ADR-0009](0009-ephemeral-branch-reclamation.md) uses a fixed drain duration,
followed by one final ref read and deletion. A push that passed `pre_receive`
before the fence can still commit between that read and deletion. Waiting longer
does not close this interval. Even a short push can be delayed by a stalled hook
response or ledger operation.

In Enroute 0.3.0 (`bc0dc6efce9cbbce9a5368e594a9b28f18a12d40`),
`crates/compute/src/push.rs` ingests the pack, asks `pre_receive`, and then commits
the updates. Deletion independently commits a tombstone through
`crates/ledger/src/ledger.rs`. The public API has no operation that stops writes
while keeping the last committed refs and objects readable.

The following ordering loses a successfully accepted change under ADR-0009:

1. A push is allowed by `pre_receive` in the old generation.
2. Compaction takes its final snapshot, before that push commits.
3. The push commits successfully in the old generation.
4. Compaction deletes the old generation without copying that commit.

Also, the 33-hour maximum in ADR-0009 omits scheduler delay, copy and verification
time, reconciliation time, and retries. It cannot be an unconditional upper
bound.

## Decision

Keep generation compaction, including the existing fence, switch, rescue refs,
and periodic reconciliation. Amend its final drain and delete sequence:

1. Wait for the configured drain interval after switching generations. This
   interval remains a courtesy to existing readers, not a write-safety barrier.
2. **Seal** the inactive generation through the backend, and record success in
   the compaction state. Never seal the active generation. Repeating this step
   after a crash is safe.
3. Read the sealed generation's final refs and reconcile every difference with
   the switch snapshot, using ADR-0009's existing apply/rescue/skip rules. No
   write can change that final snapshot after sealing.
4. Delete temporary refs in writable generations, then delete the sealed old
   generation only after final reconciliation succeeds. Its temporary refs, if
   any, disappear with the generation; sealing forbids changing them too.
5. Mark only the expiries recorded before the fence as reclaimed, as before.

### Backend contract

Add `seal_repository(id)` to [ADR-0007](0007-storage-backend-interface.md).
It is a durable, idempotent, irreversible barrier for an existing repository:

- Writes ordered before the seal have completed and are visible to subsequent
  reads when the call returns.
- Every later content or ref mutation is refused, including an already admitted
  git push, `create_commit`, `update_refs`, and a copy into the repository.
- Reads and copying **from** the repository remain available. Deletion remains
  allowed. Deleting and recreating an ID creates an empty, unsealed repository.
- The seal survives process restart, log replay, and persisted snapshots.
- A missing repository returns `repository_not_found`. A write to a sealed
  repository returns a non-retryable backend `repository_sealed` error. At the
  Cambium operation boundary, a stale generation is retried through the active
  mapping or reported as the existing retryable `repository_busy` API error.
- A lost response is not evidence of success: retry the seal before proceeding.

Capabilities gain `durable_seal`. A backend without it cannot run destructive
generation compaction. Cambium fails startup when configured to use compaction
with a backend lacking this capability; it never silently promises reclamation.
A backend that reclaims unreachable objects itself needs no generation
compaction and no seal for that purpose.

### Enroute integration

Amend the version decision in [ADR-0006](0006-enroute-spike.md): Phase 1a uses
Enroute 0.3.0 plus a Cambium-maintained, attributed Apache-2.0 patch, identified
as `0.3.0-cambium.1`. The build pins the upstream commit and patch checksum.
The patch adds an authenticated-by-network-isolation gRPC `SealRepository`
operation and a seal command/state flag at Enroute's replicated ledger boundary.
The check must happen where commits are applied, not only before a hook or in
the HTTP handler.

Enroute still runs as a separate program. No upstream patch submission or rights
assignment is implied. Cambium keeps the patch and attribution in its repository.
The official 0.3.0 binary is not advertised as supporting compaction. Mixed
patched/unpatched backend nodes and downgrades that ignore the seal state are
unsupported; restore into patched binaries or migrate live refs into a fresh
backend instead.

### Timing and failure behavior

Replace ADR-0009's unconditional maximum with a normal-operation estimate:

`trigger delay + scheduling + prepare/fence/catch-up/verify + drain + final
seal/reconcile + backend reclamation delay`.

The former approximately 33-hour figure includes only configured trigger,
drain, and default backend sweep delays; it is not a bound. Failures retry without
deleting unreconciled data. The service reports overdue reclamation and its
current stage so the operator can act. It cannot promise a finite completion
time when storage or the backend remains unavailable.

### Verification

- Reproduce the read/late-commit/delete ordering above at the state-machine level.
- A write ordered before seal remains readable and enters final reconciliation.
- Writes ordered after seal fail, including pushes admitted before the fence.
- Repeat seal, restart the backend, and replay a persisted snapshot: no write
  becomes allowed again.
- Test every write path, deletion, and creation of a new incarnation.
- Compaction cannot delete a generation before confirmed seal and final
  reconciliation; inject crashes and lost responses at both transitions.
- Retained refs, object IDs, and `HEAD` remain unchanged after compaction, apart
  from documented rescue refs.

## Consequences

The storage reclamation design no longer relies on a fixed sleep for correctness.
The project takes on a small backend patch until a backend provides the same
barrier. The patch changes backend persisted metadata, so packaging, recovery,
and version checks must treat it as part of the backend contract.

All other decisions in ADR-0006, ADR-0007, and ADR-0009 remain in effect.
