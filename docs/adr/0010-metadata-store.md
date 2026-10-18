# ADR-0010: Cambium's metadata store

- **Status:** Accepted
- **Date:** 2026-10-18

## Context

Cambium's own state never lives in a storage backend ([ADR-0001](0001-scope-and-phases.md), [ADR-0002](0002-git-compatible-edge.md)). That state includes:

- tenants, identities, and tokens
- repositories and their backend generations
- ephemeral branches and their expiry
- idempotency records
- compaction state
- later, provenance and derived data

Tenant identity is part of every key from the first line of code. Phase 1a ships SQLite, so that a single binary can run with no other service. Phase 1b adds Postgres for production.

## Decision

### Access layer

- All persistent state is reached through a `Store` trait in the crate `cambium-store` (AGPL-3.0-only). Nothing else in Cambium writes SQL.
- Phase 1a implements it with SQLite through `sqlx` (MIT OR Apache-2.0), with the SQLite library bundled. Phase 1b adds a Postgres implementation of the same trait.
- One store test suite runs against every implementation, the way the backend conformance suite does ([ADR-0007](0007-storage-backend-interface.md)).
- Each method is one transaction. Methods that must be atomic together, such as the compaction switch ([ADR-0009](0009-ephemeral-branch-reclamation.md)), are one method.

### Tenancy in the schema

- Every table that holds tenant data has a `tenant_id` column that is `NOT NULL`. Every unique index on such a table starts with `tenant_id`.
- Deployment-level tables, such as the tenant list and the migration history, are the only exceptions, and each is named in a schema test.
- Every `Store` method that reads or writes tenant data takes a `TenantId` that comes from the authenticated principal ([ADR-0011](0011-tenancy-and-authentication.md)), never from request input. Every such query filters on it.
- Deployment-wide operations, such as the expiry scheduler, use separately named methods.
- A schema test fails if a tenant table lacks `tenant_id`. Store tests check that each tenant-scoped method returns nothing for another tenant's IDs.

### Records

- **IDs.** IDs are a type prefix and a ULID in lower case, such as `rep_01jabcd...`. The prefixes include `ten_`, `idn_`, `tok_`, `rep_`, and `brn_`. IDs are allocated by Cambium, never change, and are never reused. They fit within backend repository ID rules.
- **Actors.** Every record that a request creates or changes stores the acting identity's ID and kind (`human`, `agent`, or `system`), and when it happened.
- **Times.** Times are integer milliseconds since the Unix epoch, in UTC. Commit timestamps are git data and are not stored here.
- **Secrets.** Tokens are stored only as SHA-256 hashes. They are 256-bit random values, so a slow password hash is not needed.

### SQLite settings

- WAL mode, `foreign_keys = ON`, `synchronous = FULL`, and a busy timeout.
- One database file, set in the configuration.
- The Phase 1a server is one process. It opens a single connection for writes and a pool for reads.

### Migrations

- Migrations are numbered, forward-only, embedded in the binary, and applied at startup. Startup refuses to run if the database has a migration the binary does not know.
- A migration that has been released is never edited.
- SQLite and Postgres keep separate migration directories with the same numbering, so each step means the same change on both.

## Consequences

- SQLite needs nothing beyond the binary, but allows one writer at a time. That is enough for Phase 1a. Phase 1b's Postgres store is what allows several Cambium processes.
- Hiding SQL behind the trait costs some boilerplate. In return, every tenant boundary is in one crate and covered by its tests.
- Backups of the metadata store, and their restore, are a Phase 1b deliverable. Until then, the metadata store and the backend must be backed up together, because the metadata store maps names to backend repository IDs.
