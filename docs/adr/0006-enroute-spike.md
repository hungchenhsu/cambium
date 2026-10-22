# ADR-0006: Enroute 0.3.0 spike results

- **Status:** Accepted; the Phase 1a backend version is amended by [ADR-0013](0013-sealed-generation-reclamation.md) and [ADR-0015](0015-durable-ref-creation-receipts.md)
- **Date:** 2026-10-18

## Context

[ADR-0002](0002-git-compatible-edge.md) makes Enroute the first storage backend. Before Phase 1a depends on it, the ROADMAP asks for a spike: run Enroute locally and check its quickstart, gRPC API, hooks, and limitations against the Phase 1a needs.

What was run:

- Enroute 0.3.0, the `aarch64-apple-darwin` release archive, checked against the release's `SHA256SUMS`. Its source is tag `v0.3.0` (commit `bc0dc6e`).
- Git 2.54.0 as the client, on macOS.
- Three local nodes:
  - `enroute dev` with no hooks
  - `enroute dev --hooks <url>` with all four hooks, answered by a throwaway hook server that verified every signature
  - `enroute serve` with a configuration that shortened `layers.sweep_interval` to 10 s and `layers.sweep_retention` to 15 s, to observe storage reclamation
- gRPC calls through `grpcurl`, using the server's reflection service.

Everything below was observed, unless it is marked as taken from Enroute's documentation.

## Decision

Enroute 0.3.0 is adequate for Phase 1a, and Phase 1a pins it. Cambium covers the gaps in the last column of this table.

| Phase 1a need | Enroute 0.3.0 | Observed | Cambium's response |
| --- | --- | --- | --- |
| Cambium and Enroute started together | Image `ghcr.io/enroute-sh/enroute:0.3.0` | The tag exists for `linux/amd64` and `linux/arm64` | Pin the tag |
| Hooks | `authorize`, `visible_refs`, `pre_receive`, `post_receive`, as signed HTTP calls | All four worked, and every call carried a valid Ed25519 signature | Hook endpoint ([ADR-0008](0008-git-traffic-authorization.md)) |
| Create and delete repositories | `CreateRepository`, `DeleteRepository` | Both idempotent, as documented, and confirmed with repeated calls. After a delete, the key no longer resolves | Cambium maps its repositories to keys ([ADR-0010](0010-metadata-store.md)) |
| Write a changeset without a checkout | `CreateCommit` | Works, with compare-and-swap on the branch | Branch creation, idempotency, path checks (below) |
| Read a file, list a tree | `GetObject`, `ListTree` | `ListTree` loses data (below). `GetObject` returns exact raw objects | Cambium parses raw tree objects ([ADR-0007](0007-storage-backend-interface.md)) |
| Diff two revisions | `DiffCommit` | Path-level only, with the same data loss as `ListTree` | Cambium diffs trees from raw objects |
| Ephemeral branches | `UpdateRefs` | Creating a ref is cheap. Deleting it reclaims no storage | Compaction ([ADR-0009](0009-ephemeral-branch-reclamation.md)) |
| Stock git clone, fetch, push | Git smart HTTP | Works with Git 2.54.0 | Document and test a minimum Git version |
| Tenant isolation | None inside a deployment | As documented | `authorize` hook and API checks ([ADR-0011](0011-tenancy-and-authentication.md)) |
| Structured errors | gRPC status codes, per-ref rejection reasons | See below | Cambium's own codes ([ADR-0012](0012-api-transport-and-errors.md)) |

### Observations

**Round trips.** A pushed commit cloned back with the same ID. A commit made with `CreateCommit` was fetched by stock git, and its object matched what the call returned.

**Path corpus.** A tree built with `git mktree` held these entries:

- a Latin-1 name (`caf` + byte `0xE9`), which is not valid UTF-8
- an NFC name and an NFD name that render the same
- `README` and `readme`
- an executable file, a symbolic link, and a subdirectory

It pushed, and it cloned back with the same commit and tree IDs. Enroute stores git objects byte for byte.

**`ListTree` and `DiffCommit` are lossy.**

- `ListTree` is recursive, breadth-first, and streamed over several messages.
- Both calls return paths as protobuf strings. The non-UTF-8 name came back with `0xE9` replaced by U+FFFD (bytes `EF BF BD`).
- `ListTree` returns no file mode, so an executable file, a symbolic link, and a regular file look the same.
- `GetObject` on the tree returned the raw tree object, with exact names and modes.

**`CreateCommit`.**

- Without `expected_old_commit`, a branch that does not exist is created with a root commit, not a commit on top of the default branch. To branch from an existing commit, create the ref with `UpdateRefs` first.
- Paths are protobuf strings, so a changeset cannot write a path that is not valid UTF-8. Git push can.
- Observed errors:
  - a stale or `absent` expectation: `FAILED_PRECONDITION` naming the current tip
  - a put over a directory: `INVALID_ARGUMENT`
  - an unknown repository: `NOT_FOUND`
- From the documentation, not tested: requests over 32 MiB fail with `OUT_OF_RANGE`, and continuous contention without an expectation fails with `ABORTED`.

**`UpdateRefs`.** Each update gets its own outcome:

- creating an existing ref: `REASON_ALREADY_EXISTS`
- a stale `old_object_id`: `REASON_NON_FAST_FORWARD`, not a separate stale-value reason
- a missing object: `REASON_MISSING_OBJECTS`

Neither `UpdateRefs` nor `CreateCommit` called any hook.

**Storage reclamation.**

- Deleting a branch that held the only reference to an 8 MiB blob left the repository at the same size 40 s later.
- The unreachable blob stayed readable through `GetObject`.
- The unreachable commit could be fetched by ID with `git fetch <url> <commit-id>` by any client with read access.
- Copying only the live refs into a new repository with `PushToRemote`, aimed at Enroute's own git listener, kept the same commit IDs. The unreachable blob was `NOT_FOUND` in the new repository, which used 16 KiB.
- Pushing to plain `http` and to a loopback address needs `git.allow_private_remotes = true`. `enroute dev` sets it.
- After `DeleteRepository` on the old repository, its storage went from 8,212 KiB to 12 KiB between 30 s and 40 s later, with the shortened sweep settings. The defaults are a 1 h sweep interval and a 6 h retention.

**Hooks.**

- Signatures followed RFC 9421 with Ed25519, covering `@method`, `@authority`, `@path`, and `content-digest`. The `keyid` was the RFC 7638 thumbprint of the key. The documented test vector also verified.
- Header names arrive in lower case.
- `authorize` runs once per HTTP request. A clone or push is two requests, plus a first unauthenticated request when the client has not yet sent credentials.
- `context` came back byte for byte on the later hooks of the same request.
- `visible_refs` hid `refs/heads/hidden/*` from the ref advertisement that `git ls-remote` printed.
- `pre_receive` refused one ref of a three-ref push, and the other two landed.
- `post_receive` messages reached the client as `remote:` lines.
- HTTP Basic credentials (token as password) and `Authorization: Bearer` both reached `authorize`.
- With the hook server stopped, a clone failed with HTTP 500: the hooks fail closed.
- `PushToRemote` with a bearer token went through all four hooks, with user agent `enroute/0.3.0`.

**Not tested.** Git clients older than 2.50, which Enroute documents as unable to receive thin packs; S3-compatible buckets; multi-node deployments.

## Consequences

- Phase 1a pins Enroute 0.3.0. 0.x releases are not compatible with each other, so moving to a later release is a deliberate change with its own migration test ([ADR-0002](0002-git-compatible-edge.md)).
- Cambium reads trees and commits as raw objects and parses them itself. That is the only way to report paths and modes exactly ([ADR-0005](0005-internationalization.md)).
- Enroute's API cannot express a path that is not valid UTF-8, because `PathChange.path` is a protobuf string. Cambium rejects such a changeset with `unsupported_path` before calling the backend ([ADR-0007](0007-storage-backend-interface.md)). Such paths can still be pushed with git.
- Deleting a ref does not make its objects unreadable. Anything that must disappear needs compaction ([ADR-0009](0009-ephemeral-branch-reclamation.md)).
- The deployment must keep `listen.api` private, because it authenticates nobody and serves gRPC reflection ([ADR-0008](0008-git-traffic-authorization.md)).
