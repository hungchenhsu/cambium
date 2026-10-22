# ADR-0009: Reclaiming storage from expired ephemeral branches

- **Status:** Accepted; final reconciliation, deletion, and timing rules are superseded by [ADR-0013](0013-sealed-generation-reclamation.md), recoverable branch creation by [ADR-0015](0015-durable-ref-creation-receipts.md), and rescue naming by [ADR-0016](0016-multiple-rescue-values.md)
- **Date:** 2026-10-18

## Context

Agents create branches with a time to live (TTL) and expect them, and the storage they used, to be cleaned up ([ADR-0004](0004-agent-first-interfaces.md), principle 6). An expired ephemeral branch whose data is not cleaned up is a P1 defect.

The Enroute spike ([ADR-0006](0006-enroute-spike.md)) found:

- Deleting a ref frees nothing. Its objects stay readable by ID through the API and through `git fetch <url> <object-id>`.
- Deleting a repository frees its storage after Enroute's sweep. Enroute's retention is 6 h by default.
- `PushToRemote` into a new repository copies only what the named refs reach, and keeps every object ID.

There are two designs:

- **A. A repository per ephemeral workspace.** The workspace starts as a copy of the base repository. Discarding it deletes the repository, so storage is freed promptly and nothing stays fetchable. But every workspace costs a copy of the whole history, which defeats cheap branches on large repositories.
- **B. Ephemeral branches are refs in the repository, and Cambium compacts the repository.** Creating a branch costs one ref update. Expiry deletes the ref at once, and a later compaction copies the live refs into a new repository and deletes the old one. Storage is freed with a bounded delay, and until then an expired branch's objects stay fetchable by ID by tokens that can read the repository.

## Decision

Cambium uses design B.

### Ephemeral branches

- A client creates an ephemeral branch through the API with a name under `refs/heads/`, a start commit, and a TTL. The TTL is capped by `branches.max_ttl` (default 7 days).
- The metadata store records the branch as ephemeral, with its expiry time.
- A client can extend the TTL, within the cap, or promote the branch to a permanent one.
- Commits that become reachable from a permanent ref, for example by fast-forwarding `main` to the branch, are no longer garbage when the ephemeral ref goes.

### Expiry

A scheduler in Cambium expires each branch at its expiry time. The branch's record in the metadata store is the source of truth, so a crash between steps repeats the remaining ones:

1. Mark the branch `expiring`.
2. Delete the ref with `update_refs`, expectation `any`. A delete of a ref that is already gone succeeds. The branch is gone from every ref listing and advertisement immediately.
3. Record an **unreclaimed expiry** against the repository, with the time it was recorded, and mark the branch expired.
4. Until that expiry is reclaimed, `pre_receive` refuses a push that would create a ref with the expired branch's name, so a stale clone cannot turn it into a permanent branch by accident. Creating a new ephemeral branch with the same name through the API lifts this block, and pushes to the new branch are ordinary updates.

### Generations

- Each Cambium repository maps to one **active generation**: a backend repository ID of the form `<tenant id>/<repository id>/g<number>`. Generation numbers only grow and are never reused.
- Cambium records a generation's ID in the metadata store before it creates the backend repository. A crash can then leave only generations the metadata store names, and a sweeper deletes the ones that are neither active nor part of a running compaction.
- Deleting a Cambium repository deletes every generation it has: active, draining, and one being built.
- At most one compaction runs per repository at a time.

### Internal copies

Compaction moves data with `copy_refs` ([ADR-0007](0007-storage-backend-interface.md)). On Enroute that is `PushToRemote` to Enroute's own git listener, which runs every hook ([ADR-0008](0008-git-traffic-authorization.md)). Such a push carries an internal token that names one destination generation. For that push:

- `authorize` grants exactly that generation, and nothing else.
- `visible_refs` returns every ref.
- `pre_receive` allows every command while the token is valid, and refuses every command otherwise. None of the policy rules for clients apply: not the active-generation check, not the fence, not reserved names, and not expired names.
- `post_receive` is ignored, so a copy never counts as activity on a branch.
- At the API and the MCP server, an internal token is not a valid token: it resolves to no principal, gets `unauthenticated`, and cannot mint tokens.

### Reserved refs

- Temporary refs live under `refs/cambium/<compaction id>/`. They are never advertised to clients, and clients cannot write them ([ADR-0008](0008-git-traffic-authorization.md)). Every way a compaction ends, whether it finishes, aborts, or is resumed after a crash, first deletes every ref under its own prefix in every generation it touched. Prepare, Catch up, and Verify ignore `refs/cambium/`. The orphan sweeper deletes refs under the prefix of any compaction that is not running.
- Rescue refs live under `refs/cambium-rescue/<compaction id>/<ref name>`. They are advertised, so clients can fetch them, but clients cannot write them. They can point at any object, including tags. Cambium creates each with expectation `absent`.
- These refs are copies of git data, not Cambium state, which stays in the metadata store.

### Compaction

A repository is compacted when it has unreclaimed expiries and either:

- `compaction.max_delay` (default 24 h) has passed since the oldest one, or
- the number of unreclaimed expiries reaches `compaction.max_expired` (default 100).

Compaction is a state machine recorded in the metadata store, so a crash at any step can be resumed or aborted. *g* is the active generation and *g+1* the new one.

1. **Prepare.**
   - Record the compaction, with its ID and the generation it will create.
   - Read `list_refs` of *g*, including the branch `HEAD` points at.
   - Record *g+1*, then create it with the same `HEAD` branch.
   - Copy every live ref of *g* into *g+1*. An empty set of refs copies nothing.
   - Writes to *g* continue.
2. **Fence.** Record the fence time and mark the repository fenced. Then:
   - API writes wait for in-flight writes to finish, then fail with the retryable error `repository_busy`.
   - `pre_receive` refuses client pushes with a retry message.
   - Reads continue from *g*.
3. **Catch up.** Read `list_refs` of *g* as the **switch snapshot**. Make *g+1* match it: copy every ref that is new or changed since step 1, always allowing a non-fast-forward because nothing else writes *g+1*, and delete every ref that disappeared.
4. **Verify.** `list_refs` of *g+1* must equal the switch snapshot: the same refs, the same object IDs, and the same `HEAD` branch. Otherwise abort: delete *g+1*, lift the fence, and leave *g* active.
5. **Switch.** In one metadata transaction:
   - make *g+1* the active generation
   - store the switch snapshot
   - mark *g* as draining
   - lift the fence
6. **Reconcile.** A push that `pre_receive` allowed before the fence can still land in *g* later, because a large push uploads its data before the refs move. Reconcile carries such landings into the active generation.
   - It runs `compaction.settle` (default 60 s) after the switch, which catches small pushes. It then runs every `compaction.settle` during the drain, and a final time at its end, which catches large pushes whose data is still uploading.
   - It compares `list_refs` of *g* with the switch snapshot. A differing ref that names an ephemeral branch with an unreclaimed expiry is skipped: it needs neither a copy nor a rescue. For each other ref that differs, it reads the active generation's value of that ref and acts by the first row that matches:

   | The active generation holds | Action |
   | --- | --- |
   | *g*'s new value, or a descendant of it | Nothing: already reconciled. `is_ancestor` in the active generation answers false for a commit it does not hold, so a value that only *g* holds falls through to the next rows. |
   | The snapshot value | Apply *g*'s change to the active generation: copy the new value to `refs/cambium/<compaction id>/<ref name>`, move the ref with compare-and-swap from the snapshot value, then delete the temporary ref. A delete in *g* becomes a compare-and-swap delete. If the compare-and-swap is stale, because a client moved the ref meanwhile, evaluate this ref again from the first row. |
   | Anything else | A conflict. Copy *g*'s value into the active generation as a rescue ref, record the conflict, and alert the operator. A delete in *g* that conflicts needs no rescue. |

   Every differing ref must reach an end state: already reconciled, applied, rescued, or skipped. No data is lost, and a conflict never keeps *g* alive.
7. **Drain.** Wait `compaction.drain` (default 1 h) after the switch, so that clones and fetches already streaming from *g* can finish. Then run the final reconcile.
8. **Delete.** `delete_repository(g)`. Then mark reclaimed every unreclaimed expiry that was recorded before this compaction's fence time. Expiries recorded later are left for the next compaction, because their objects are in *g+1*.

### Rules that keep data safe

- A generation is deleted only after step 4 passed and the final reconcile brought every differing ref to an end state.
- Every client write path checks the fence:
  - API writes, inside the same lock that admits the write
  - git pushes, in `pre_receive`
  - Cambium's own writers, other than the internal copies of the compaction that holds the fence
- Phase 1a runs one Cambium process, so the fence is a per-repository lock in that process plus the fenced flag in the metadata store. Running several Cambium processes (Phase 1b) needs a lease in the shared metadata store first.
- Compaction also drops objects that other operations left unreachable, such as force-pushed or deleted permanent branches. This is what `git gc` does too.

### Bounds and visibility

- The longest time from expiry to the backend freeing storage is `compaction.max_delay` + `compaction.drain` + the backend's own delay.
- Enroute frees a deleted repository's files once they have gone unnamed for `layers.sweep_retention`, as measured by its sweeps, which can each run late by up to `layers.sweep_interval`. With the defaults (6 h and 1 h) that is about 8 h. The spike measured 30 to 40 s with 15 s and 10 s ([ADR-0006](0006-enroute-spike.md)).
- With Cambium's defaults on Enroute, storage is freed about 33 h after the oldest expiry at most. Cambium documents the sum and measures it in its tests.
- Until the old generation is deleted, a token that can read the repository can fetch an expired branch's objects if it knows their IDs. Such a token could already read the branch before it expired. To remove a secret pushed by mistake, an operator command runs compaction at once.

### Verification

Phase 1a tests:

- An expired branch's ref is gone at once.
- After compaction:
  - a blob that only the expired branch reached cannot be read from the active generation
  - every other ref, object ID, and the `HEAD` branch is unchanged
- With a shortened backend sweep, the old generation's storage is freed.
- A repository whose only refs were ephemeral compacts to an empty generation.
- A push that lands after the switch is carried into the active generation by reconcile, in each case of the table in step 6, including a stale compare-and-swap and a rescued tag.
- A crash at any point leaves no ref under `refs/cambium/` once the compaction is resumed or aborted.
- An expiry recorded during the drain is still unreclaimed after the compaction ends.
- Crash recovery is tested at every step of the state machine.

## Consequences

- Ephemeral branches cost one ref update, whatever the size of the repository.
- Compaction copies a repository's live objects, so its cost grows with repository size. The triggers bound how often it runs.
- Writes to a repository pause for the duration of the fence, steps 2 to 5. The bulk copy happens before the fence.
- Rescue refs are the one way compaction can add a visible ref. Each needs an operator to resolve it, and the operator command lists them.
- A backend with `reclaims_unreachable_objects` ([ADR-0007](0007-storage-backend-interface.md)) can skip compaction. Expiry still deletes the ref at once.
- Design A remains an option for workspaces that need prompt, guaranteed removal. It would be added as a separate kind of workspace, not as a change to this one.
