# Ephemeral branches and storage reclamation

The Phase 1a lifecycle uses the pinned Enroute `0.3.0-cambium.2` backend with
[durable creation receipts](adr/0015-durable-ref-creation-receipts.md) and
[durable seals](adr/0013-sealed-generation-reclamation.md). Unpatched Enroute is
not compatible. The deployment runs one Cambium process per SQLite store.

## Branch ownership and expiry

`create_ephemeral_branch` creates an absent branch at a commit and returns its
`branch_id`. The default TTL is one hour; the configured maximum is seven days.
A permanent Git branch cannot be adopted by this operation. Cambium records the
creation intent before the backend call; Enroute atomically retains the winning
or losing result under that branch ID. Losing the response cannot turn another
writer's permanent branch into an ephemeral branch, even at the same commit.
Pending creation returns `repository_busy` for name-based reads and writes until
the worker resolves its receipt.

`extend_branch` sets the deadline to the later of its current deadline and the
first attempt's time plus `ttl_seconds`. Replaying its idempotency key never adds
more time. `promote_branch` keeps the ref and removes its expiry. Both require a
still-active branch; neither revives one whose deadline has passed. Activity is
informational and never extends a TTL. `list_branches` lists lifecycle records,
including expired and promoted records; it is not a list of every Git ref.

From the deadline until reclamation or explicit name reuse, name-based reads and
writes return `branch_expired` (HTTP 410).
Git advertisement hides the ref and pre-receive rejects writes to it. The worker
resolves pending ownership, marks the record expiring, deletes the ref, and
records an unreclaimed expiry. It repeats deletion if a previously admitted push
recreates the old ref. An explicit new branch creation may reuse an expired name;
its new branch ID prevents the earlier expiry from deleting the new owner.

Raw object IDs can still be read while their objects remain in a generation.
Expiry is a storage lifecycle feature, not secret erasure or object-level access
control. Objects reachable through retained branches, tags, or rescue refs remain.

## Generation changes

Compaction starts when either the oldest unreclaimed expiry reaches 24 hours or
there are 100 unreclaimed expiries. A repo administrator can call
`request_compaction` sooner, including on a repository without expiries. An
existing running compaction is returned; retrying a request retains its assigned
compaction ID even if it finishes between attempts.

1. **Prepare:** allocate a never-reused destination generation and copy live refs
   while ordinary API and Git writes continue.
2. **Catch up:** fence new writes, record the exact expiry IDs and branch owners,
   copy again, and verify every live ref's object ID and symbolic HEAD.
3. **Switch:** atomically activate the new generation and remove the write fence.
   New Git authorizations use it immediately.
4. **Drain:** reconcile writes from Git requests authorized on the old generation.
   The default drain lasts one hour, with reconciliation every minute.
5. **Seal:** durably refuse all further writes to the old generation, reconcile
   its final state, then delete it. A missing seal acknowledgement never permits
   deletion. Enroute subsequently sweeps unreferenced layer files.

Copies target hidden temporary refs under `refs/cambium/`; final active refs are
changed with compare-and-swap. A late backend response cannot overwrite a newer
active ref. Temporary refs are cleaned after completion and on later sweeps. Orphan discovery
uses one prefix-filtered ref listing per repository at most once per minute,
with its schedule persisted across restarts. It looks up only compaction IDs
actually found in that listing, rather than scanning all historical records.
Periodic discovery remains necessary because a previously admitted copy can
land after an earlier cleanup.
If the active ref moved independently, the late value is retained under
`refs/cambium-rescue/<compaction-id>/<sha256-of-original-ref>/<object-id>`.
Multiple late values receive separate refs. These are ordinary visible, fetchable
refs but clients cannot write them. A conflicting deletion is recorded without
inventing a value. See [ADR-0016](adr/0016-multiple-rescue-values.md).

`get_reclamation` reports `active_generation`, `fenced`, `unreclaimed_expiries`,
`oldest_expiry_at` when present, `overdue`, and the latest `compaction`. Its
conflict entries map original refs and old/active values to rescue refs. Follow
`compaction.next_page_token` using the same operation and repository; `limit`
defaults to 100, with a maximum of 1000. A cursor retains the chosen compaction
when a newer one starts. Supply `compaction_id` to inspect older rescue records.

Expiry records created after the catch-up fence stay pending for another
compaction. A branch captured at that fence and later expired is excluded from
late-write reconciliation, even if another branch has since reused its name.

## Recovery and timing

Startup revokes internal copy credentials. It aborts any pre-switch compaction,
seals and deletes its abandoned destination, releases the old write fence, and
never reuses that generation number. This avoids trusting copies whose outcome
was lost when the process stopped. A post-switch compaction resumes draining,
sealing, final reconciliation, or deletion. Rescue intents are durable before
their refs are created and complete before temporary refs are cleaned.

Internal copy tokens expire after two minutes, bind one compaction and destination,
and are revoked after each copy, including failures. They cannot authenticate to
the public API or MCP. Both copy authorization and later signed hooks revalidate
the scope and expiry. Only hashes are stored.

Defaults can be changed in `cambium.toml`:

```toml
maintenance_interval_ms = 1000

[branches]
max_ttl_seconds = 604800

[compaction]
max_delay_ms = 86400000
max_expired = 100
settle_ms = 60000
drain_ms = 3600000
```

`overdue` becomes true when the oldest expiry is at least `max_delay_ms + drain_ms`
old. This is an operational signal, not a wall-clock completion guarantee. Copy
size, backend availability, retries, scheduling, and Enroute's sweep retention
add time. During outages Cambium retains the old generation rather than deleting
unreconciled data. Keep all backend nodes on the pinned patch; do not downgrade
through a binary that ignores durable seals.

## Evidence

[Lifecycle integration tests](../crates/cambium-server/tests/lifecycle.rs) use the
real patched backend, including signed internal copies, all durable recovery
stages, conflicting commits and annotated tags, stale CAS, name reuse, failed
verification, and deletion racing a copy. They compare live hashes and HEAD,
check discarded objects are absent in the new generation, and observe old layer
files disappearing while live generation files remain. The physical-file test
uses a 100 ms sweep and 300 ms retention for an accelerated disposable deployment;
its timing is not the default deployment's reclamation bound.

```sh
ENROUTE_BIN="$PWD/target/enroute/target/debug/enroute" \
  cargo test --locked -p cambium-server --test lifecycle -- --ignored
```
