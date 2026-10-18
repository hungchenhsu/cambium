# ADR-0002: Git protocol at the edge, storage through pluggable backends

- **Status:** Accepted
- **Date:** 2026-10-18

## Context

Every client, CI system, and agent already speaks git, and models have learned git from large amounts of training data. Systems that replace the git client face an adoption problem, and early agent-focused ones were criticized for it (for example in the [Show HN thread for Oak](https://news.ycombinator.com/item?id=48631726)).

Git storage and the git wire protocol are the largest and riskiest parts of a git server, and open-source engines now implement them. Enroute stores repositories in object storage behind a replicated log and terminates git smart HTTP. It offers a gRPC API, including commits without a checkout, and calls back into an application through hooks for authentication, visibility, and ref updates. Its gRPC API does not authenticate callers, and its docs leave policy to the application.

## Decision

- Stock git clients keep working: clone, fetch, and push go over git smart HTTP.
- Cambium does not implement git storage or the git wire protocol. It talks to storage engines through a single backend interface.
- The first backend is Enroute. Cambium acts as the application that Enroute expects:
  - It implements Enroute's hooks for authentication, visibility, and ref updates.
  - It is the only client of Enroute's gRPC API.
- Only Cambium may be able to reach Enroute's gRPC listener. A deployment that exposes it bypasses all of Cambium's authorization.
- A separate ADR will settle how git traffic reaches the backend: directly, authorized through hooks, or through a Cambium proxy.

## Consequences

- **Backend interface.** It must not leak Enroute specifics, and a conformance test suite runs against every backend.
- **Backend maturity.** Enroute is early. Its 0.x releases are not compatible with each other, and it has changed its storage format before without a migration path. In response, Cambium:
  - pins backend versions
  - keeps its own state out of the backend
  - keeps a tested path for moving repositories between backends, for example by pushing every ref to the new backend
- **Upstream contributions.** Enroute does not accept pull requests. It takes emailed patches. Contributors assign to OpenCanopy GmbH all rights in each patch that they can assign, and grant a perpetual license, including the right to relicense, for any rights they cannot. Cambium therefore works around backend gaps in its own layer where it can, and decides case by case whether to send a patch upstream.
- **Future licensing.** These terms let Enroute's owner change the license of future versions. Released Apache-2.0 versions stay Apache-2.0, so a fork remains possible.
- **Vendor overlap.** Enroute's authors plan to make money by hosting Enroute. A hosted Cambium would compete with the vendor of its first backend. This is another reason to keep backends replaceable.
- **Storage reclamation.** Enroute does not garbage-collect unreachable branch objects, and it reclaims storage only when a repository is deleted. Expiring ephemeral branches would leave storage behind. The reclamation ADR decides how Cambium handles this, for example by giving each ephemeral workspace its own short-lived repository.
- **Merges.** Enroute creates no merge commits. Cambium computes any merges it needs, such as for the Phase 3 merge gate, and writes them as ordinary commits.
- **Hash formats.** SHA-256 support depends on the backend. Enroute supports SHA-1 only.
- **Client versions.** Enroute needs Git 2.50 or later to receive thin packs, and older clients can fail. Cambium documents a minimum git version and tests it in the quickstart.
- **Other backends.** walgit (MIT) is a candidate second backend. A Cambium-owned, Scion-style engine remains an option as another backend ([ADR-0001](0001-scope-and-phases.md), Phase 2).
