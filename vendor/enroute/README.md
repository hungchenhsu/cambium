# Enroute 0.3.0-cambium.1

This directory distributes an Apache-2.0 patch to
[Enroute 0.3.0](https://github.com/enroute-sh/enroute/tree/bc0dc6efce9cbbce9a5368e594a9b28f18a12d40).
The upstream copyright and attribution are retained in `LICENSE` and `NOTICE`.
Cambium's patch is also licensed under Apache-2.0. It has not been submitted to
upstream, and upstream does not maintain this variant.

`manifest.toml` pins the upstream commit, patched version, and patch SHA-256.
The [preparation script](../../scripts/prepare_enroute.py) verifies all three
source boundaries: upstream HEAD, the exact staged patch, and absence of
unstaged or untracked source changes. It never overwrites an existing directory.

```sh
python3 scripts/prepare_enroute.py
cd target/enroute
cargo build --locked
cargo test --locked -p enroute-ledger --lib
cargo test --locked -p enroute-shared state::tests
cargo test --locked -p enroute --test seal
```

Enroute's own `rust-toolchain.toml` pins Rust 1.98.1. Run Cargo from that directory
to use the upstream pin; Cambium's workspace uses its own toolchain. From the
Cambium root, `python3 scripts/prepare_enroute.py --verify` checks an existing
prepared source tree. Ignored build output under `target/` is allowed.

## Contract

The patch implements [ADR-0013](../../docs/adr/0013-sealed-generation-reclamation.md).
`enroute.api.v1alpha1.RepositoryService/SealRepository` orders a seal in the
repository's replicated log and returns after it has been applied. The seal
is idempotent and irreversible for that incarnation. Reads and deletion remain
available. A missing repository returns gRPC `NOT_FOUND`.

The ledger refuses every later `Commit`, including ref-only updates, layer-only
publication, API changesets, and a git push admitted before sealing. API writes
return `FAILED_PRECONDITION` with `repository sealed`; git receive-pack reports
the same refusal for its refs. No client-side fence is needed for this guarantee.
Deleting and recreating the key creates an empty, unsealed incarnation.

The persisted representation adds protobuf fields and one flag in the existing
ledger counters. Tests cover followers, state-store encoding, log replay,
installed snapshots, restart, late git pushes, reads, and new incarnations.
The snapshot is unchanged by a refused write. Upload generation advancement
and deletion remain possible; layer replacement is a `Commit` and is refused.
An already uploaded but uncommitted layer is not published into the repository.
For each deleted sealed incarnation, the sweeper retains a small trash marker
even after the object prefix becomes empty. This lets a later sweep discover
and reclaim an upload that was issued before sealing but completed after an
empty sweep. The marker is cleanup metadata, not retained repository content.
Unsealed deletion keeps upstream's existing marker-removal behavior.

Every node must run this patch. Mixed patched/unpatched nodes and downgrades
that ignore the seal are unsupported. Restore with patched binaries, or migrate
retained refs to a fresh backend. The gRPC listener still has no authentication
of its own and must remain reachable only by Cambium.
