# Development

The workspace includes API and backend contracts, a SQLite metadata store,
token delegation and authorization, CI, license checks, and reusable git path
fixtures. The Enroute adapter, signed-hook translation, and shared operation
layer are implemented, together with the HTTP/MCP binary and signed Git policy.
The generation lifecycle and container composition are still in progress.
See the [local setup](local-development.md) to run the current service.

## Prerequisites

- Rust through rustup. `rust-toolchain.toml` pins **1.99.1**, including rustfmt and
  Clippy. The workspace uses edition 2024 and resolver 3.
- Python **3.11 or later**, using only the standard library for policy checks.
- Git for the path corpus tests. Git **2.50 or later** is the project baseline
  because Phase 1a's Enroute integration requires it (ADR-0002).
- `cargo-deny` **0.20.2**: `cargo install --locked cargo-deny --version 0.20.2`.

Run commands below from the repository root. Cargo uses the pinned toolchain
automatically; `rustup show` installs it if necessary. Commit `Cargo.lock` when
dependencies change. All crates have `publish = false` during scaffolding.

## Workspace

| Crate | License | Responsibility |
| --- | --- | --- |
| `cambium-protocol` | Apache-2.0 | API types and the error registry (ADR-0012) |
| `cambium-backend` | Apache-2.0 | Backend and git policy contracts (ADR-0007) |
| `cambium-backend-conformance` | Apache-2.0 | Shared fixtures and the future backend conformance suite |
| `cambium-backend-enroute` | AGPL-3.0-only | Enroute adapter and hook translation (ADR-0006, ADR-0008) |
| `cambium-store` | AGPL-3.0-only | Tenant-scoped metadata access and all SQL (ADR-0010) |
| `cambium-server` | AGPL-3.0-only | Shared operations, HTTP/MCP, signed Git policy, and host administration |

The protocol crate validates typed IDs and byte-field pairs and owns the
[error registry](api/errors.md). The backend crate defines object-safe storage
and git policy traits, including the durable seal required by ADR-0013. The
store crate implements SQLite migrations and tenant-scoped metadata and token
operations. The server crate implements strict shared requests, repository and
identity operations, changesets, raw-object reads, and durable idempotency. The
`cambium` binary mounts those operations through HTTP and MCP and implements
private signed-hook policy over the same principal resolution.

Server crates may use the Apache crates. Apache crates must not depend, directly
or transitively, on AGPL crates. Inherit shared package settings and lints, but
declare each crate's license explicitly so its boundary is visible in its own
manifest. New workspace crates belong under `crates/`.

## Verification

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
python3 scripts/check_licenses.py
python3 -m unittest discover -s scripts/tests -v
git diff --check
```

The [CI workflow](../.github/workflows/ci.yml) runs Rust checks on Linux and macOS,
and license checks on Linux. Run `actionlint` after changing workflows if it is
available. Branch protection requires the DCO, Linux and macOS Rust, and license
policy checks.

## License policy

Run `scripts/check_licenses.py`, not just `cargo deny check licenses`:

1. Cargo metadata resolves every workspace member with all features and no target
   filter. A structural check requires explicit Apache-2.0 or AGPL-3.0-only
   licenses and rejects transitive Apache-to-AGPL workspace dependencies.
2. `cargo-deny` checks the complete workspace with [deny.toml](../deny.toml), which
   allows permissive and explicitly listed AGPL-compatible dependencies.
3. Each Apache crate is then checked as a graph root with
   [deny-apache.toml](../deny-apache.toml), which allows only the listed permissive
   licenses. SPDX expressions are evaluated by cargo-deny, including `OR` and
   `AND`; they are not parsed by a custom license parser.

Both passes include build and dev dependencies and unpublished crates. No target
filter is used, so another platform's dependency is still checked. The checker
does not skip optional dependencies. `--offline` is available after
`cargo fetch --locked` has downloaded dependencies for **all** targets.

The policy tests construct local fixture workspaces and execute the actual
checker. They cover missing licenses, forbidden direct and indirect dependencies,
optional and target-specific dependencies, build and dev dependencies, rejected
third-party licenses, and an allowed permissive alternative in a dual license.

Adding a license to either allow-list requires checking it against
[ADR-0003](adr/0003-licensing.md). An unlisted license fails closed. These checks
cannot establish the provenance of copied source; the ADR's no-copyleft-copying
rule still applies. The graph-root behavior is documented in
[cargo-deny's common options](https://embarkstudios.github.io/cargo-deny/cli/common.html),
and license expression handling in its
[license configuration](https://embarkstudios.github.io/cargo-deny/checks/licenses/cfg.html).

## Path corpus

[paths.rs](../crates/cambium-backend-conformance/src/paths.rs) stores paths as byte
slices, paired with exact blob contents and git modes. It contains invalid UTF-8,
NFC and NFD forms of the same name, a case collision, CJK, tab/newline characters,
nested paths, an executable, and a symbolic link.

The integration test creates a bare SHA-1 repository in a temporary directory,
writes blobs and an index through git plumbing, and compares a NUL-delimited tree
listing with the original bytes. It verifies blob contents and modes, hashes the
raw tree, and rebuilds the tree to verify the object ID is unchanged. It never
checks paths out onto the host filesystem, where normalization or case folding
could erase the very cases under test. No developer repository is modified.

These are reusable input fixtures and a stock-git round trip, not evidence of
Enroute conformance, Cambium path parsing, or backend migration correctness.
Those checks belong to the implementations described in ADR-0007.

## Pinned Enroute patch

[Enroute 0.3.0-cambium.2](../vendor/enroute/README.md) adds the durable seal
required by ADR-0013 and atomic ref creation receipts required by ADR-0015. Its source is pinned by upstream commit and patch SHA-256,
with the Apache-2.0 license and upstream attribution retained. The preparation
script applies only that patch to a fresh checkout and verifies source drift.
The [patch workflow](../.github/workflows/enroute.yml) tests replicated state,
restart and snapshot persistence, real git writes racing with a seal, and
storage reclamation of late uploads. This verifies the backend primitive;
Cambium's generation lifecycle still requires its own integration tests.

The adapter implements the storage operations and verifies every raw object's
size and SHA-1. It uses the pinned protobuf sources and signature verifier.
`GitPolicy` receives only ref names for visibility (the hook provides no object
IDs), and receives Enroute's force classification for push commands. The hook
adapter verifies signatures before decoding or invoking policy and refuses
incomplete policy results. The service mounts it on its separate private listener
and implements tenant, token-chain, generation, and reserved-ref checks.

Run the real adapter tests after building the patched backend:

```sh
ENROUTE_BIN="$PWD/target/enroute/target/debug/enroute" \
  cargo test --locked -p cambium-backend-enroute --test backend -- --ignored
```

These tests start disposable, loopback-only Enroute processes. They cover
lifecycle, ref expectations, changeset CAS, sealed reads and writes, copying
from and into sealed repositories, and byte-exact reads of the full path corpus
after stock-git push and backend copy. The patch workflow runs them in CI.
They do not establish multi-tenant transport isolation or lifecycle reclamation.

## Shared operation tests

```sh
ENROUTE_BIN="$PWD/target/enroute/target/debug/enroute" \
  cargo test --locked -p cambium-server --test service -- --ignored
```

The service test uses a real Enroute process with fault injection around its
adapter. It checks cross-tenant denial at the operation layer, token restriction
checks before replay, byte-field output, pagination, path-collision warnings,
CAS after a lost response, and completion after caller cancellation. Ordinary
tests also check raw git parsing against the path corpus, mode-only diffs, and
encryption that binds a cached response to its identity and request.

Changeset success includes `path_collision_check_complete`. If warning scanning
hits a tree limit or a backend read fails after the commit lands, this field is
false and the successful commit IDs are still returned and cached. The absence
of a collision warning then makes no claim about unscanned paths. A real-backend
regression writes a tree with 100,001 entries to exercise that boundary.

`App` accepts a deployment response key and uses encrypted idempotency envelopes
([ADR-0014](adr/0014-encrypted-idempotency-responses.md)). The network service
loads that key from private storage and acquires the single-process deployment
lock before invoking `recover_at_startup`. Recovery resumes recorded repository
creation/deletion and makes abandoned idempotency reservations retryable.

The separate [transport test](../crates/cambium-server/tests/transports.rs) starts
real HTTP/MCP and signed-hook listeners with Enroute, then exercises stock Git
clone/fetch/push, cross-tenant denial, reserved refs, and token revocation.
An optional official MCP SDK client checks interoperability as described in the
local setup. The Enroute CI job pins Git 2.50.0 to upstream commit
`16bd9f20a403117f2e0d9bcda6c6e621d3763e77` and also runs the official MCP SDK
2.3.0 check. The binary test verifies process ownership, encrypted replay after
restart, and immediate host-admin revocation. Container exposure and ephemeral
reclamation require later tests.

## Metadata and authorization tests

`cargo test -p cambium-store` creates disposable SQLite databases. It tests
cross-tenant denial for the implemented store methods, token delegation limits,
human/agent separation, parent revocation, disabled identities, restart
persistence, and unknown or modified migrations. A schema test requires every
tenant table and unique index to start with the tenant boundary. The named
deployment exceptions are `tenants` and `_sqlx_migrations`.

`SqliteStore::open` uses a single writer, a separate read pool, WAL, foreign keys,
FULL synchronization, and a five-second busy timeout. Mutations re-resolve the
token and all ancestors in the same transaction as the write. The deployment
bootstrap and human-token issuance methods are intended only for host admin
commands. There is no network tenant creation operation.

These store tests are distinct from the real-backend transport tests above.
