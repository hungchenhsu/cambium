# ADR-0017: Persist reconciliation progress before backend mutations

- **Status:** Accepted; amends ADR-0009's reconciliation baseline and recovery ordering, including the final reconciliation specified by ADR-0013
- **Date:** 2026-10-23

## Context

The switch snapshot alone cannot describe repeated reconciliation. If an old ref
moves from A to B, a periodic pass applies B to the active generation, and the old
ref returns to A before sealing, comparing only against the snapshot skips A and
incorrectly retains B. Similarly, a ref created and applied during draining can
later disappear from both the original snapshot and the final old ref listing.

Saving progress only after a backend CAS also leaves an ambiguous outcome when
the response or metadata save fails. A conflict has a similar gap if its object
is copied before the rescue intent is saved: a crash can leave only a temporary
ref, which startup cleanup removes after the old ref has changed again.

## Decision

Maintain a durable baseline for each ref: the last value applied by this
compaction, or the switch snapshot value if no application has completed. An
applied absence is a recorded value. Reconciliation includes names from this
progress map as well as both snapshots, so late creation followed by deletion is
still evaluated.

For an old value differing from that baseline:

1. If the active value equals the baseline, apply the old value with CAS. This
   takes precedence over ancestry checks, so rollbacks and deletions propagate.
2. Otherwise, retain independent active writes. If they already hold the old
   value or a commit descendant, no rescue is needed. Other values produce a
   conflict and, for a non-deletion, a rescue under ADR-0016.

Before copying or issuing an active-ref CAS, save a pending intent containing the
ref name, expected value, and desired value. Finish it before evaluating newer
old-generation changes. If the active value already equals the desired value,
recover the applied baseline even when the CAS acknowledgement was lost. If it
still equals the expected value, retry the CAS. If an independent writer won,
retain that writer and rescue the intended old value when needed. Clear the
intent and save the applied baseline together. Expired branch owners remain
excluded, including pending intents whose names a new branch has reused.

Save each conflict's rescue intent before its first backend mutation, including
temporary source anchors and copies. Recovery can then recopy its exact object
ID even after the original old ref has moved. Complete pending applications and
rescues before temporary cleanup and before entering the sealing stage. Once
sealed, new observations use stable old ref names as copy sources. No deletion
is allowed until all pending work and final reconciliation have completed.

## Consequences

Metadata grows with the refs this compaction has applied and the conflicts it
has observed. This state remains in Cambium, not in backend Git metadata. No new
backend capability or public API field is required. The seal barrier, generation
ownership, expiry rules, and rescue naming remain unchanged.

Regression tests exercise A-to-B-to-A with a lost CAS response and restart, late
creation followed by deletion, and process loss after a conflict copy completes
but before its rescue ref exists. They also retain the stale-CAS, tag, deletion,
and multiple-conflict recovery cases.
