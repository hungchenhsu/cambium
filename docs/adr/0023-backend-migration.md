# ADR-0023: Preserve Git object identity when moving a repository

- **Status:** Proposed
- **Date:** 2026-11-20

## Context

How can an operator move a logical repository to another storage backend and
prove that every live Git ref and every object reachable from it survived
with its original hash? [ADR-0002](0002-git-compatible-edge.md) requires a
tested exit from a backend. The [backend contract](0007-storage-backend-interface.md)
exposes raw objects, direct refs and the default branch. It does not expose
arbitrary symbolic refs beyond HEAD. An unsupported object format or ref
representation must stop migration before cutover.

Cambium currently holds one backend in its [service](../../crates/cambium-server/src/service.rs).
There is no per-repository backend switch in the running server. The following
control-plane steps are proposed. The Git transfer and verification below were
tested independently.

## Options

| Option | Where it applies | Limitation |
| --- | --- | --- |
| Copy the source engine's database or bucket | Same-version backup/restore using the backend's own format. [Local restore procedure](../quickstart.md) | The destination engine would need to interpret another engine's internal format. |
| Use `Backend::copy_refs` | Moving between repository IDs that a single configured backend can address. [ADR-0007](0007-storage-backend-interface.md) | The interface does not address two independent backend instances. |
| Transfer named refs using Git and verify from a new destination clone | Two adapters that expose compatible, authorized Git endpoints and agree on object format and ref representation. [Git mirror clone](https://git-scm.com/docs/git-clone) | Requires authoritative ref enumeration, a stable source and a fresh destination. Git does not contain Cambium's metadata. |

## Decision

Recommend an offline write cutover with durable migration state and a verified
Git transfer. Keep the logical tenant and repository IDs. Record backend
instance **and** incarnation for both generations. Public routing, internal
transfer grants and recovery must agree on that mapping. The migration job
uses Cambium-authorized endpoints and a grant limited to the designated
generations. Operators do not publish backend control listeners.

1. **Prepare.** Check compatible object formats and supported ref types,
   authorization wiring, capabilities and a verified backup of Cambium's
   metadata and separately managed keys. Serialize migration with branch
   creation, deletion and compaction. Resolve pending ref-creation receipts
   on their original incarnation. Complete outstanding reconciliation and
   clean its temporary refs. Preserve live rescue refs. Keep the TTL/deletion
   state, idempotency records and commit provenance in Cambium. A backend
   receipt cannot be reconstructed from an object ID.
   [Creation receipts](0015-durable-ref-creation-receipts.md) ·
   [Rescue refs](0016-multiple-rescue-values.md) ·
   [Reconciliation progress](0017-durable-reconciliation-progress.md)
2. **Fence and stage.** Persist a migration write gate for all writers, drain
   admitted API work and block new client pushes at policy admission. Use the
   backend to enumerate all live direct refs and the default branch. Require an
   empty, exclusive destination incarnation with that exact default branch.
   Copy the snapshot and compare it as described below. If an already-admitted
   source push changes the snapshot, retry. Reads may continue from the source.
3. **Switch behind the gate.** After the initial comparison succeeds, atomically
   persist the destination mapping, source draining state and checkpoint.
   Keep client writes gated. A quiet interval cannot prove a late source push
   is finished. Seal **only the inactive source**, retry an unacknowledged seal
   and then enumerate its final stable snapshot. Persist pending copy/CAS
   intent before each mutation. Apply late creates, changes and deletions to
   the destination against their staged values. Stop on an unexpected
   destination value and retain both copies. Re-run the full comparison against
   the sealed source before opening writes.
   [Seal contract](0013-sealed-generation-reclamation.md) ·
   [Generation lifecycle](0009-ephemeral-branch-reclamation.md)
4. **Recover and retire.** Before the mapping switch, abort by discarding the
   verified migration-owned staging incarnation and releasing the gate with
   the source active. After the switch or an uncertain seal, retain the gate
   and resume from the durable checkpoint. A sealed source cannot be reopened
   for writes. After new destination writes, a return move needs a new
   migration and a fresh incarnation. Drain existing readers and confirm the
   backup, final verification and checkpoint before retiring the old storage.
   Keep workers from sweeping either incarnation while the move is pending.
   Resume expiry/reclamation accounting from the durable metadata.

### Git transfer and independent verification

For a stable test snapshot, first compare the source adapter's unfiltered
`list_refs` (including the HEAD target) with the exact source advertisement.
Require an empty destination and set its default branch during creation. Verify
the source and destination adapter inventories again after the transfer. Use
the adapter for an unborn HEAD, which an empty advertisement may omit. Keep the
transfer identity authorized for the intended complete set. Hold the move if
visibility policy hides a source ref.

With `SRC_GIT` and `DST_GIT` set to these two Git endpoints, run in a new
private working directory with no Git object alternates, shallow clones or
shared object cache. The commands use Bash and network clones. The refspec
creates exactly the named refs in the empty destination. The later catch-up
for an already-staged destination is the separate CAS step above.

```bash
set -euo pipefail
export LC_ALL=C GIT_NO_REPLACE_OBJECTS=1
git clone --mirror "$SRC_GIT" source.git
git -C source.git for-each-ref --sort=refname \
  --format='%(refname) %(objectname)' > source.refs
if test -s source.refs; then
  git -C source.git push "$DST_GIT" 'refs/*:refs/*'
fi
git clone --mirror "$DST_GIT" destination.git
git -C destination.git for-each-ref --sort=refname \
  --format='%(refname) %(objectname)' > destination.refs
cmp source.refs destination.refs
for side in source destination; do
  git -C "$side.git" fsck --full --strict
  git -C "$side.git" rev-list --objects --all --no-object-names |
    sort -u > "$side.oids"
  git -C "$side.git" cat-file --batch < "$side.oids" > "$side.objects"
done
cmp source.oids destination.oids
cmp source.objects destination.objects
```

The sorted ID lists cover commits, tags, trees and blobs. The batch stream
compares each object's ID, type, length and original bytes. Raw trees encode
path bytes and file modes, and raw commits encode ordered parents. Compare
the annotated tag's own ID as well as its peeled target. Check HEAD against
the adapter's default branch separately. Unreachable objects, reflogs and
host-local hooks are outside this live-ref transfer. For larger repositories
budget space for the comparison streams or compare them incrementally.
[Object enumeration](https://git-scm.com/docs/git-rev-list) ·
[Raw batch output](https://git-scm.com/docs/git-cat-file)

### Two-instance evidence

On 2026-11-20, two pinned `0.3.0-cambium.2` test processes used different
file buckets, node directories and ports on a trusted local host. The fixture
had no live Cambium route. Both repositories were provisioned with
`refs/heads/trunk` as the default. The source was sealed twice before the
stable transfer. A deliberate copy of just trunk failed the unfiltered
adapter inventory comparison. Completing the transfer preserved all five
refs, including a branch and direct refs to a tag object, a tree and a blob.

Independent network mirrors contained the same twelve objects: four commits
including a two-parent merge, one tree, six blobs and one annotated tag.
The tree included regular, executable and symbolic-link modes, binary data,
a non-UTF-8 path and distinct NFC/NFD spellings. Each object rehashed to its
original Git SHA-1. The test also checked the HEAD target and peeled tag.
A separate empty repository retained zero refs, zero objects and the same
unborn default branch on both adapters. The [test server fixture](../../crates/cambium-server/tests/common/mod.rs)
and [pinned Enroute sources](../../vendor/enroute/README.md) define the
disposable server configuration.

## Consequences

This trial verifies raw Git data movement between two instances of the same
adapter. A second backend implementation, cross-instance routing, crash-safe
metadata cutover, lifecycle scheduling during maintenance, and rollback after
a real deployment switch still need implementation and integration tests.
A new adapter must pass [conformance](0007-storage-backend-interface.md)
and the same migration procedure before it can be a supported destination.
