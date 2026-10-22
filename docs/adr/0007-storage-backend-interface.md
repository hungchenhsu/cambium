# ADR-0007: Storage backend interface and conformance suite

- **Status:** Accepted; durable seal is added by [ADR-0013](0013-sealed-generation-reclamation.md) and durable ref creation by [ADR-0015](0015-durable-ref-creation-receipts.md)
- **Date:** 2026-10-18

## Context

[ADR-0002](0002-git-compatible-edge.md) puts every storage engine behind one interface that must not leak Enroute specifics, and requires a conformance suite that runs against every backend. Moving a repository between backends must not change any git hash.

The Enroute spike ([ADR-0006](0006-enroute-spike.md)) showed what a backend can be trusted with:

- Raw git objects come back exactly.
- Listings and diffs built by the backend can lose path bytes and file modes.
- A backend may be unable to write some paths, create merge commits, or reclaim unreachable objects.

## Decision

### Shape

- The interface is a Rust async trait, `Backend`, in the crate `cambium-backend` (Apache-2.0), so third parties can implement a backend without taking on the AGPL. Backend adapters that exist only to run the server, such as the Enroute adapter, are AGPL-3.0-only ([ADR-0003](0003-licensing.md)).
- A backend stores repositories under a **backend repository ID** that Cambium allocates. It is opaque, at most 200 bytes, in `/`-separated segments of ASCII letters, digits, `-`, `_`, and `.`, each starting and ending with a letter or digit. Backends never see tenant names, repository names, or identities. Those stay in Cambium's metadata store ([ADR-0010](0010-metadata-store.md)).
- Object IDs are 40 lowercase hexadecimal characters (SHA-1). The interface carries the object format so that SHA-256 can be added without changing its shape.

### Required operations

| Operation | Contract |
| --- | --- |
| `create_repository(id, default_branch)` | Idempotent. A repeat with the same ID succeeds and changes nothing. |
| `delete_repository(id)` | Idempotent. The ID stops resolving when the call returns. Storage may be reclaimed later. |
| `list_repositories(prefix)` | Every repository ID that starts with the prefix, byte for byte, in pages. Used to find repositories that Cambium's metadata no longer names. |
| `list_refs(id, prefixes)` | Every ref under the prefixes, and the branch `HEAD` points at. |
| `update_refs(id, updates)` | Each update names a ref, an expectation (`absent`, a specific object ID, or `any`), and a new object ID or a delete. A delete with expectation `any` of a ref that does not exist succeeds. Each update succeeds or is rejected on its own, with a reason from a closed set: `stale`, `already_exists`, `missing_object`, `invalid_name`, `not_a_commit`. A backend that cannot tell two reasons apart reports the broader one: Enroute reports a branch pointed at a non-commit as a missing object. |
| `read_object(id, object_id)` | The object's kind and its exact bytes. The bytes hash to the object ID. |
| `is_ancestor(id, ancestor, descendant)` | Whether one commit is an ancestor of another, for fast-forward checks. A commit the repository does not hold makes the answer false, not an error. |
| `create_commit(id, request)` | One commit with at most one parent, on a branch, with compare-and-swap on the branch and changes that put or delete files. The parent is the commit the branch points at. Paths are byte strings. |
| `copy_refs(source_id, destination_id, copies, atomic)` | Each copy names a source ref, a destination ref name, and whether a non-fast-forward is allowed. Copies the refs and every object reachable from them into another repository of the same backend, keeping every object ID. Objects reachable from no named ref are not copied. An empty list copies nothing and succeeds. |

Cambium computes everything else from these operations: path lookup, tree listings, diffs, and log. It parses raw commits, trees, and tags itself, with a maintained permissively licensed parser such as `gix-object` (MIT OR Apache-2.0). It checks that the bytes of every commit and tree it reads hash to the requested ID. Objects never change, so it can cache them by ID.

### Git traffic

- A backend that serves stock git calls back into Cambium through a `GitPolicy` trait, also in `cambium-backend`. The trait has four calls, which match what every git server needs: authorize a request, filter advertised refs, judge each ref update, and report landed updates.
- The adapter turns the backend's own mechanism into these calls. For Enroute that mechanism is its signed hooks ([ADR-0008](0008-git-traffic-authorization.md)).
- A backend without such a callback cannot be exposed to git clients directly. It would need a Cambium proxy, decided in its own ADR.

### Capabilities

`capabilities()` reports what a backend can do. Cambium branches on capabilities, never on the backend's name.

| Capability | Meaning | Enroute 0.3.0 |
| --- | --- | --- |
| `object_format` | Hash algorithm | `sha1` |
| `max_commit_request_bytes` | Largest `create_commit` request | 32 MiB |
| `non_utf8_paths` | `create_commit` can write paths that are not valid UTF-8 | No |
| `merge_commits` | `create_commit` can take more than one parent | No |
| `atomic_ref_updates` | `update_refs` can apply several updates all or nothing | No |
| `reclaims_unreachable_objects` | Deleting a ref eventually frees the storage only it used | No |

### Errors

Backends return a closed `BackendError` enum. Each variant is marked retryable or not:

- `repository_not_found`
- `object_not_found`
- `stale_expectation`, with the actual value
- `invalid_request`, with the field and the reason
- `unsupported_path`, with the path
- `too_large`, with the limit
- `contention` (retryable)
- `unavailable` (retryable)
- `internal`

Backend-specific text goes into logs only. The API maps these variants to its own error codes ([ADR-0012](0012-api-transport-and-errors.md)).

### Conformance suite

The crate `cambium-backend-conformance` (Apache-2.0) is a library of async tests that takes a factory for fresh backend repository IDs. A backend conforms when every test passes. The suite covers:

1. **Lifecycle:** create and delete are idempotent, and a deleted ID can be created again empty.
2. **Refs:** each expectation, each rejection reason, outcomes reported per update in request order, and `is_ancestor` answering false for an unknown commit.
3. **Commits:**
   - regular, executable, and symbolic-link modes
   - nested directories, and deletes that leave a directory empty
   - compare-and-swap, including a stale expectation, which changes nothing
   - the root commit of a new branch
4. **Raw reads:** every object read back hashes to its ID.
5. **Exact paths:** the path corpus ([ADR-0005](0005-internationalization.md)) pushed with stock git reads back byte for byte, with modes, through Cambium's parser. If `non_utf8_paths` is false, `create_commit` rejects a non-UTF-8 path with `unsupported_path` and changes nothing.
6. **Copy:** `copy_refs` keeps every object ID, copies exactly the named refs under their destination names, honors the non-fast-forward flag, accepts an empty list, and leaves behind objects that no named ref reaches.
7. **Git edge:** a backend that serves git denies, hides, and refuses exactly what its `GitPolicy` says, and fails closed when the policy cannot be reached.

From Phase 1b, the suite runs in CI against every backend, with each backend pinned to a version.

## Consequences

- The interface is small. Cambium owns git object parsing, path handling, and every derived view, so their behavior does not change with the backend.
- A file read costs one object read per path segment, plus the blob. Phase 2's derived-data index can add faster paths later.
- Moving a repository to another backend is "copy every ref", using `copy_refs` within one backend, or a git push between backends. The conformance suite proves the copy keeps object IDs.
- A capability a backend lacks becomes a structured error, never a silent fallback. For example, a non-UTF-8 path in a changeset on Enroute fails with `unsupported_path`.
