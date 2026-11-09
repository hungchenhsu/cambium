# ADR-0020: The first Postgres metadata store keeps one writer

- **Status:** Proposed; amends ADR-0010's consequence that Postgres allows several processes
- **Date:** 2026-11-10

## Context

[ADR-0010](0010-metadata-store.md) adds a Postgres implementation of the `Store`
traits in Phase 1b, with migrations in a separate directory under the same
numbers as SQLite, and one store test suite run against every implementation.
It also says Postgres "is what allows several Cambium processes".

The SQLite store relies on having exactly one writer. It opens a single write
connection, and the server takes an exclusive lock on its data directory, so
every store method runs alone. Many invariants depend on that:

- token chain revalidation before a mutation;
- idempotency reservations;
- branch name reuse;
- the compaction fence, switch, and seal sequence (ADR-0009, ADR-0013,
  ADR-0017);
- the deployment-wide scheduler.

Running several active processes against one Postgres database would let these
methods interleave. Each method would need its own row locks or serializable
transactions with retries, and each would need a fresh analysis and fresh
crash tests. Porting the SQL and changing the concurrency model in one step
would make both harder to review.

## Decision

- **Port first, scale later.** The first Postgres store keeps the SQLite store's
  concurrency model. It uses one write connection and a read pool, and every
  method is one transaction with the same statements, translated to Postgres
  syntax. Where SQLite reads insertion order from `rowid`, the Postgres tables
  have an identity column `seq`.
- **One active server per database.** At startup the server takes a
  session-level `pg_try_advisory_lock` on a key derived from the deployment, and
  holds it on the write connection. It refuses to start when another process
  holds the lock, as the SQLite server does with its directory lock. Losing the
  connection releases the lock. A replacement write connection must take the
  lock again; until it can, each write fails after a short acquire timeout. The
  server therefore never writes without the lock.
- **Same schema, same numbers.** `migrations/postgres` mirrors
  `migrations/sqlite` step by step. Millisecond times and counters are `BIGINT`,
  flags are `BOOLEAN`, and byte values are `BYTEA`. The schema test applies to
  both: every tenant table has a non-null `tenant_id`, and every unique index
  starts with it.
- **One test suite.** The store tests run against SQLite always, and against
  Postgres when `CAMBIUM_TEST_POSTGRES_URL` names a disposable database. CI
  provides one.
- **Several active processes** need a separate ADR. That ADR must analyze each
  invariant listed above, choose row locks or serializable retries per method,
  and add tests that interleave methods across processes.

## Consequences

- Postgres gives managed durability, backups, and point-in-time recovery
  without changing behavior, but not yet horizontal scale.
- A second server refuses to start while the first holds the lock. Automatic
  takeover by a waiting standby needs its own decision.
- The SQL exists twice. The shared test suite, not review alone, keeps the two
  implementations equivalent.
