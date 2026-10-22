# Roadmap

Cambium is built in phases ([ADR-0001](docs/adr/0001-scope-and-phases.md)). Each phase ships something usable on its own and has exit criteria. Items are checked off in the pull request that completes them.

## Phase 0: Foundations

Decisions and scaffolding before any code.

**Decisions**

- [x] Scope, phases, and deployment models ([ADR-0001](docs/adr/0001-scope-and-phases.md))
- [x] Git protocol at the edge, storage through pluggable backends ([ADR-0002](docs/adr/0002-git-compatible-edge.md))
- [x] Licensing ([ADR-0003](docs/adr/0003-licensing.md))
- [x] Agent-first interface principles ([ADR-0004](docs/adr/0004-agent-first-interfaces.md))
- [x] Internationalization ([ADR-0005](docs/adr/0005-internationalization.md))
- [x] Spike: run Enroute locally and check its quickstart, gRPC API, hooks, and limitations against the Phase 1a needs below ([ADR-0006](docs/adr/0006-enroute-spike.md))
- [x] Storage backend interface and its conformance test suite ([ADR-0007](docs/adr/0007-storage-backend-interface.md))
- [x] How git traffic is authorized: Enroute's hooks, no proxy ([ADR-0008](docs/adr/0008-git-traffic-authorization.md))
- [x] Reclaiming storage from expired ephemeral branches on Enroute, which reclaims storage only when a repository is deleted ([ADR-0009](docs/adr/0009-ephemeral-branch-reclamation.md))
- [x] Cambium's metadata store, SQLite first, then Postgres ([ADR-0010](docs/adr/0010-metadata-store.md))
- [x] Tenancy and authentication model ([ADR-0011](docs/adr/0011-tenancy-and-authentication.md))
- [x] API transport and error code registry ([ADR-0012](docs/adr/0012-api-transport-and-errors.md))
- [x] Durable seal before final reconciliation and deletion ([ADR-0013](docs/adr/0013-sealed-generation-reclamation.md))
- [x] Encrypted idempotency response replay, including token minting ([ADR-0014](docs/adr/0014-encrypted-idempotency-responses.md))
- [x] Multiple late-write rescue values ([ADR-0016](docs/adr/0016-multiple-rescue-values.md))
- [x] Durable ref creation receipts for unambiguous branch ownership ([ADR-0015](docs/adr/0015-durable-ref-creation-receipts.md))

**Scaffolding**

- [x] Cargo workspace, pinned toolchain (`rust-toolchain.toml`), crate layout ([development guide](docs/development.md))
- [x] CI: `cargo fmt`, `cargo clippy`, `cargo test` ([workflow](.github/workflows/ci.yml))
- [x] CI license policy: every crate declares a license, no Apache-2.0 crate depends on an AGPL-3.0-only crate, and third-party dependencies pass the ADR-0003 allow-list ([checker](scripts/check_licenses.py))
- [x] Path test corpus: non-UTF-8, NFC vs NFD, and case-colliding paths (ADR-0005; [corpus](crates/cambium-backend-conformance/src/paths.rs))

The scaffolding checks can be run locally as described in the development guide.
The CI workflow must pass on GitHub before the workspace-in-CI exit criterion is
met. The Code of Conduct contact and final product name below remain open.

**Open-source readiness**

- [x] Contribution policy: DCO 1.1, not a CLA ([ADR-0003](docs/adr/0003-licensing.md))
- [x] `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1), `SECURITY.md`
- [x] Issue templates, pull request template, `CODEOWNERS`
- [x] DCO sign-off check in CI
- [x] Turn on GitHub private vulnerability reporting (`SECURITY.md` depends on it)
- [x] Turn on GitHub Discussions for workload reports and design questions
- [x] Dated comparison with existing projects ([docs/comparison.md](docs/comparison.md))
- [ ] Publish the Code of Conduct contact email (replace the placeholder)
- [x] Protect `main`: require pull requests and the DCO check
- [x] Require sign-off on commits made in the GitHub web interface
- [x] Open a labelled issue for each open Phase 0 item, with the question, constraints, and acceptance criteria ([`phase-0` issues](https://github.com/hungchenhsu/cambium/issues?q=label%3Aphase-0))
- [ ] Choose the final product name and run trademark, crates.io, and domain checks before public launch

**Exit criteria:** every open ADR above is accepted, the workspace builds in CI, the license policy check is enforced, and every open-source readiness item is done.

## Phase 1a: A slice people can try

The smallest Cambium that shows the agent layer working over Enroute.

- [x] Single Cambium binary plus Enroute, started together with one command (container compose file), with Enroute pinned to a version tag
- [x] Cambium implements Enroute's hooks (authentication, visibility, pre-receive, post-receive) and verifies that hook calls come from Enroute
- [x] SQLite metadata store, including branch lifecycle and compaction recovery
- [x] Tenants and scoped, short-lived tokens; tenant and identity fields (human or agent) in every record ([store tests](crates/cambium-store/tests/authorization.rs); transport isolation remains below)
- [x] API: create and delete repositories
- [x] API: write a changeset (paths and contents) without a checkout, with an idempotency key
- [x] API: read a file at a revision, list a tree, diff two revisions
- [x] Ephemeral branches with a TTL and cleanup, with storage reclaimed by the method chosen in the reclamation ADR
- [x] Stock git clone, fetch, and push through Enroute, authorized by Cambium, with a documented and tested minimum git version (Enroute requires Git 2.50 or later)
- [x] Structured errors with stable codes for every call above
- [x] MCP server exposing the same operations ([transport and SDK checks](docs/local-development.md#verify-the-transport-and-mcp-client))
- [x] Cross-tenant denial tests for the API, MCP, and git traffic, plus a check that Enroute's gRPC listener is not reachable from outside ([compose verification](scripts/verify_compose.py))
- [x] Five-minute quickstart after the initial local image build, plus an example script that drives an agent loop against Cambium ([walkthrough](docs/quickstart.md); published images remain Phase 1b)

**Exit criteria:**

- Following only the quickstart, a new user can start Cambium, connect an agent through MCP, and have it create a repository, write and read changes, and clone them with stock git.
- An expired ephemeral branch is cleaned up, and its storage is reclaimed by the method chosen in the reclamation ADR.
- Requests without a valid token are rejected everywhere, including git traffic.
- A valid token for one tenant cannot reach another tenant's repositories through the API, MCP, or git.

## Phase 1b: Production basics

- [ ] Postgres metadata store
- [ ] Object storage configuration guide for Enroute (S3-compatible)
- [ ] Full error code registry, published as part of the API contract
- [ ] Provenance fields on changes (agent, model, session ID)
- [ ] CLI as a thin client over the API, with `--json` output and a message catalog for human output (`en`)
- [ ] Cross-tenant isolation tests extended to every endpoint, run in CI
- [ ] Backend conformance test suite, run in CI
- [ ] Tested procedure for moving repositories to another backend
- [ ] Crash-recovery and durability tests, and a documented backup and restore procedure
- [ ] Self-host packaging: release binaries and container images
- [ ] Published benchmarks against Enroute alone, walgit, and a stock git server (ephemeral branch creation, write latency, clone time)

**Exit criteria:** a single-tenant deployment can be self-hosted and backed up by following the docs alone, tenant isolation and conformance tests pass in CI, and the benchmarks are published.

## Phase 2: History and scale

- [ ] Derived-data index: values computed from parent commits, eager or deferred
- [ ] Log, path history, and blame APIs built on the index
- [ ] Search API, including CJK text
- [ ] Repository event streams and webhooks
- [ ] Second storage backend (walgit is the first candidate)
- [ ] Decide, in an ADR, whether a Cambium-owned Scion-style storage engine is needed

**Exit criteria:** history queries need no clone, and two backends pass the conformance suite.

## Phase 3: Review and merge policy

Policy for what lands, with humans and agents treated differently.

- [ ] Enforce the human/agent identity distinction
- [ ] Approvals bound to a specific commit and dismissed when new commits arrive
- [ ] An agent's approval never satisfies a human-approval requirement
- [ ] Directory-level permissions
- [ ] Risk tiers determined by the change itself
- [ ] Policy hooks that external review tools can report into
- [ ] Merge gate driven by policy, without a global lock

**Exit criteria:** a repository can require human approval on chosen paths, accept agent-only merges on low-risk changes, and prove which commit each approval covered.
