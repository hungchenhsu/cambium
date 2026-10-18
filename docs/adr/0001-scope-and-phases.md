# ADR-0001: Scope, phases, and deployment models

- **Status:** Accepted
- **Date:** 2026-10-18

## Context

AI coding agents multiply the number of repositories, branches, commits, and checkouts. Git's client model assumes offline-capable clients that each hold a full copy and compute locally. Agent fleets and CI jobs are always online and usually touch a small part of a repository.

Anthropic's Scion (2026) shows that a server-centric design keeps git's conceptual model while removing full local clones, push, and packfile maintenance. Scion is not available outside Anthropic. Meta's Mononoke pioneered much of that design and is open source, including a git server and derived data, but its README says it is still in development and omits some functions.

By October 2026, two kinds of open-source servers each cover part of what agent fleets need:

- **Forges** (Gitea, Forgejo, GitLab) offer commit APIs and authentication. Gitea and GitLab also offer MCP servers, and GitLab offers service accounts. They keep each repository as git on server disks.
- **Object-storage engines** (Enroute, Apache-2.0; walgit, MIT) leave authentication, tenancy, and policy to the application built on them.

Early agent-focused projects add some of the missing pieces, such as agent accounts or expiring repositories, each tied to a single storage design.

No open-source project we know of combines the following into one lifecycle over storage engines that can be swapped:

- tenant isolation
- separate human and agent identities
- ephemeral branches whose storage is reclaimed
- an agent API and MCP server
- provenance
- merge policy

See [docs/comparison.md](../comparison.md).

## Decision

Cambium is an agent-first layer over pluggable git storage engines. It does not build its own git storage first. It is built in three phases, and each phase ships something usable on its own.

1. **Agent layer**, over the first storage backend, Enroute.
   - **Phase 1a, a slice people can try:**
     - tenants and scoped tokens
     - an API and an MCP server to create repositories, write a changeset with an idempotency key, and read files, trees, and diffs
     - ephemeral branches with a TTL and cleanup
     - git clone and push through the backend, under Cambium's authorization
     - a five-minute quickstart
   - **Phase 1b, production basics:**
     - Postgres
     - the full error code registry
     - provenance
     - a CLI
     - durability and backup procedures
     - cross-tenant isolation tests
     - published benchmarks
2. **History and scale**
   - A derived-data index for log, path history, blame, and search.
   - Event streams.
   - A second storage backend.
   - A Cambium-owned, Scion-style storage engine is considered here as another backend, but only if existing backends limit the product.
3. **Review and merge policy**
   - Separate human and agent identities.
   - Bind approvals to a specific commit.
   - Add risk tiers and policy hooks that external review tools can use.

**Deployment models.** The same software supports both:

- a multi-tenant hosted service, and
- a single-tenant deployment operated by the customer.

**Non-goals for now:** implementing git storage or the git wire protocol, replacing the git client, a virtual filesystem, a forge UI (issues, pull request pages), and CI.

## Consequences

- Tenant identity is part of every Cambium key and every API call from the first line of code. Retrofitting isolation later is not an option.
- Cambium's own state lives in Cambium's metadata store, not in the backend. That state includes tenants, tokens, identities, TTLs, provenance, and derived data. Keeping it out of the backend keeps backends replaceable.
- Phase 1 ships much sooner than it would if Cambium built storage first, but it depends on a young backend. [ADR-0002](0002-git-compatible-edge.md) covers how that dependency is contained.
- Without a virtual filesystem, agents that need files on disk get sparse materialization of only the paths they request.
