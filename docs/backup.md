# Backup and restore

Cambium's state has two parts that must be backed up **together**: the metadata
store and the storage backend. The metadata store maps tenant and repository
names to backend repository IDs and generations ([ADR-0010](adr/0010-metadata-store.md)),
and also holds the deployment keys. A backup of one part without the other
cannot be restored.

This procedure takes a **cold** backup of the local compose deployment
([quickstart](quickstart.md)). Both services stop while their volumes are
archived, so the archives are consistent with each other. Online backup is not
supported yet.

## What is backed up

| Volume | Contents |
| --- | --- |
| `metadata` | SQLite metadata store (including its WAL), `responses.key`, the hook signing key pair, and `cambium.toml` |
| `enroute-data` | Enroute's node state and object storage |

The `hook-key` volume is not archived. Initialization republishes it from the
restored metadata.

The archives contain every repository's source code and the deployment's private
keys. Store them encrypted, with access limited like the deployment itself.

## Back up

```sh
python3 scripts/compose_backup.py backup ~/cambium-backups/2026-11-10
```

The script stops `cambium` and `enroute` and streams each volume as a tar
archive from a one-off container that uses the deployment's own image and mount
points. It then writes `manifest.json` with each archive's SHA-256 and starts
the services again. Add `--no-restart` to leave them stopped. The target
directory must be new or empty.

## Restore

A restore writes only into empty volumes, so it never mixes a backup into
existing state. To replace a deployment, remove it deliberately first. This
erases its current data:

```sh
docker compose down --volumes
python3 scripts/compose_backup.py restore ~/cambium-backups/2026-11-10
```

The script checks the archives against the manifest, refuses to continue while
any container of the deployment exists, creates the volumes, refuses again if
any is not empty, extracts each archive with ownership
and permissions preserved, and starts the deployment. Startup recovers any
lifecycle operation that was interrupted when the backup was taken.

A restore returns every token, identity, and revocation to its state at backup
time. A token revoked **after** the backup is valid again until it expires.
Revoke or disable affected identities again right after restoring.

## Crash recovery

The metadata store runs SQLite in WAL mode with `synchronous = FULL`. On restart,
Cambium marks interrupted operations retryable; a retry with the same
idempotency key returns the stored result, or `ref_conflict` when the commit
landed but its result was never recorded ([ADR-0012](adr/0012-api-transport-and-errors.md)).
Interrupted repository creation, deletion, and compaction resume at startup
([branch lifecycle](branch-lifecycle.md)).

Cambium does not itself guarantee the backend's durability. The tests below kill
both processes after acknowledged writes and require every write to be present
after restart. They are evidence for the pinned versions, not a guarantee for
other storage configurations.

## Evidence

The [compose workflow](../.github/workflows/compose.yml) runs the agent loop,
then:

1. kills both services with `SIGKILL`, starts them, and repeats the isolation
   and content checks;
2. backs up with this script, removes the deployment and its volumes, restores
   into fresh volumes, and repeats the same checks with the credential and
   repository created before the backup.

The [durability test](../crates/cambium-server/tests/durability.rs) runs in the
[patch workflow](../.github/workflows/enroute.yml) against the pinned Enroute,
with every layer in the object bucket. It kills Enroute with `SIGKILL` after
acknowledged writes and restarts it on the same directory. It then copies the
stopped metadata and Enroute directories, starts a deployment on the copy with
new ports, and requires the same file contents, idempotent replays, and
provenance, plus new writes that leave the original untouched. As a self-test,
leaving out Enroute's node state or its object bucket from the copy makes the
test fail.
