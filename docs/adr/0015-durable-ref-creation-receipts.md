# ADR-0015: Durable receipts for ref creation

- **Status:** Accepted; amends ADR-0006, ADR-0007, and ADR-0009 as specified below
- **Date:** 2026-10-23

## Context

Ephemeral branch creation spans Cambium metadata and a backend ref mutation.
Recording `creating`, issuing an absent-ref CAS, and recording `active` cannot
recover a lost response unambiguously. A Git push authorized before the metadata
reservation can race that CAS. If the ref holds the requested object ID, Cambium
cannot tell which writer created it. Adopting it can later delete a permanent
branch; rejecting every ambiguous result can leave an ephemeral ref untracked.

A process mutex cannot serialize Git writes after `pre_receive` returns. A
receipt in a separate backend call has the same crash interval. Putting lifecycle
records in Git refs would violate ADR-0002's state boundary.

## Decision

Add `create_ref_once(repository, operation_id, name, oid)` and the capability
`durable_ref_creation`. The backend atomically applies an absent-ref CAS and
records its outcome at the replicated mutation boundary:

- The opaque operation ID is scoped to a repository incarnation. The same ID
  and parameters return the original applied/refused outcome after ref movement
  or deletion, without applying another mutation. Different parameters are invalid.
- A refused creation is remembered too: removing the conflicting ref does not
  turn a retry into a new creation.
- Ref and receipt survive log replay, snapshots, and restart atomically. Receipts
  are never evicted during the repository incarnation. Deletion removes them;
  a new incarnation starts empty. Sealing refuses new creations but may answer
  an existing receipt without changing state.
- Receipts are backend request-deduplication state: opaque ID, ref, object ID,
  and outcome. Tenant policy, identity, TTL, branch lifecycle, and compaction
  decisions remain only in Cambium's store. Receipts are not Git objects or refs,
  do not change object IDs, and are not copied by Git transport.
- A backend without this capability cannot create recoverable ephemeral branches
  through this design.

Cambium durably records the branch ID, creation operation ID, source object,
destination generation, and expiry before the call. Only a confirmed creation
activates the ephemeral branch. A refusal removes the pending reservation without
adopting the existing ref. Recovery repeats the same operation ID. A generation
cannot switch with an unresolved creation naming it. Repository deletion discards
pending creations with all recorded generations.

Enroute gains a `CreateRefOnce` gRPC operation, a replicated command, and a receipt
key space. The pin becomes `0.3.0-cambium.2`, retaining ADR-0013's durable seal.
The adapter probes both capabilities before serving. Mixed versions and
downgrades that discard receipts are unsupported.

## Consequences

Creation has an unambiguous owner across response loss and concurrent Git writes.
Receipt storage grows with create attempts until generation deletion. Normal
ephemeral reclamation drops old receipts; promoted-only workloads may retain them
until operator-requested compaction. No time or size eviction may weaken the contract.

Tests cover both race orderings with identical object IDs, lost replies, movement
and deletion before retry, ID reuse, refusal followed by conflict removal,
sealing, snapshot installation, restart, and a fresh incarnation. Other ADR-0009
rules remain unchanged.
