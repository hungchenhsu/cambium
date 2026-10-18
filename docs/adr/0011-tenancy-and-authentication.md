# ADR-0011: Tenancy and authentication model

- **Status:** Accepted
- **Date:** 2026-10-18

## Context

Cambium serves both a multi-tenant hosted service and single-tenant deployments ([ADR-0001](0001-scope-and-phases.md)). One tenant must never read or write another tenant's data.

Identity separates humans from agents in the data model from the start, and an agent's approval never counts as a human's ([ADR-0004](0004-agent-first-interfaces.md), principle 8). Tokens are scoped and short-lived.

Enroute enforces no boundary inside a deployment ([ADR-0006](0006-enroute-spike.md)). The boundary is Cambium's API and its `authorize` hook ([ADR-0008](0008-git-traffic-authorization.md)).

## Decision

### Entities

| Entity | Fields that matter here |
| --- | --- |
| Tenant | ID, slug (lower-case letters, digits, and `-`; unique; used in git URLs), creation time |
| Identity | ID, tenant, kind (`human` or `agent`, fixed at creation), name, owner, disabled time |
| Token | ID, tenant, identity, parent token, SHA-256 hash, scopes, optional repository restriction, creation time, expiry time, revocation time |

- An agent identity names a human identity in the same tenant as its owner, the person responsible for it.
- A human identity has no owner.
- A single-tenant deployment is a deployment with one tenant.

### Tokens

- **Format.** `cmb_` followed by 52 lower-case base32 characters, encoding 32 random bytes. The prefix lets secret scanners find leaked tokens. Cambium stores only the hash ([ADR-0010](0010-metadata-store.md)) and shows the token once, when it is created.
- **Lifetime.** Every token has an expiry. The maximum for the identity's kind bounds every token, however it was minted. Defaults, all configurable:

  | | Default lifetime | Maximum lifetime |
  | --- | --- | --- |
  | Agent token | 1 h | 24 h |
  | Human token | 1 h | 30 days |

- **Scopes.** Each scope includes the ones before it, and `tenant:admin` includes them all:
  1. `repo:read`: read repositories, refs, files, trees, and diffs, and clone and fetch
  2. `repo:write`: write changesets, update refs, manage ephemeral branches, and push
  3. `repo:admin`: create and delete repositories
  4. `tenant:admin`: manage identities and tokens in the tenant
- **Repository restriction.** A token can be limited to a list of repositories. Other repositories do not exist for it.
- **Minting.**
  - Any token can mint a token for its own identity with a subset of its scopes, the same or a narrower repository restriction, and an expiry no later than its own. An agent can hand a narrower token to a sub-agent this way, but no token can raise its own privileges.
  - A token with `tenant:admin` can also mint tokens for agent identities in its tenant.
  - A token for a human identity is minted only by a token of that same identity, or by `cambium admin`. Otherwise an agent holding `tenant:admin` could act as a human, and Phase 3 would count its approvals as human ones.
  - In every case, a minted token's scopes are a subset of its parent's, its repository restriction is the same as its parent's or narrower, and its expiry is no later than its parent's.
  - Every minted token records its parent token.
- **Revocation.** Revoking a token also revokes every token minted from it, directly or through other minted tokens. Revocation takes effect on the next request. Cambium looks up every token, and the revocation of its parents, on every request and does not cache the result.

### Bootstrap and deployment administration

- Phase 1a has no network API for creating tenants.
- The operator runs `cambium admin` on the server host. It works directly on the metadata store to:
  - create a tenant and its first human identity
  - issue that identity a `tenant:admin` token, printed once
  - revoke tokens and disable identities
- A hosted service's control plane will be a separate program that talks to Cambium through an API ([ADR-0003](0003-licensing.md)). That API is a later decision.

### Where requests are checked

- The API, the MCP server, and the git hooks share one function. It resolves a token into a **principal**: tenant, identity, identity kind, scopes, and repository restriction.
- An unknown, expired, or revoked token, a disabled identity, or an internal token of a compaction ([ADR-0009](0009-ephemeral-branch-reclamation.md)) gets `unauthenticated`.
- Each operation states its required scope and checks it through one authorization function.
- The principal's tenant scopes every store call ([ADR-0010](0010-metadata-store.md)). A request never names its own tenant.
- A resource in another tenant, or outside the token's repository restriction, gets `not_found`, the same answer as a resource that does not exist.
- A resource the token can see but lacks the scope for gets `permission_denied`.

### Identity in records

Every record a request creates stores the acting identity's ID and kind ([ADR-0010](0010-metadata-store.md)). Phase 3's approval rules read the kind from there. An agent cannot change its kind, and no API turns an agent identity into a human one.

## Consequences

- Short lifetimes mean agents get new tokens often. A minted token never outlives its parent, so an orchestrator that issues agent tokens holds a `tenant:admin` token. Handing a narrower token down to a sub-agent needs no admin.
- Only that same identity or `cambium admin` can mint for a human, so a tenant's second human identity gets its first token from the operator on the server host. Onboarding people in Phase 1a therefore needs the operator.
- Revoking a token revokes everything minted from it. Before revoking an orchestrator's `tenant:admin` token, mint its replacement, or every agent token it issued stops working.
- Without a network API for tenants, the hosted service needs its control plane before it can onboard customers. That is intended: no endpoint exposed in Phase 1a can create or list tenants.
- The tests for this ADR, in addition to those in [ADR-0008](0008-git-traffic-authorization.md):
  - every API and MCP operation is called with another tenant's valid token, and must fail with `not_found` and change nothing
  - a minted token never exceeds its parent's scopes, repository restriction, or expiry, or the maximum lifetime for its kind
  - no token of an agent identity can mint a token for a human identity
  - revocation takes effect on the next request, for the token and every token minted from it
