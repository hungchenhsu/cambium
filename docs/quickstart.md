# Local container quickstart

This pre-alpha demo runs Cambium and the pinned patched Enroute on your own
machine. It binds the API to `127.0.0.1:8080` and Git to `127.0.0.1:8083`.
Docker Engine 28+, Docker Compose v2, Python 3.11+, and Git 2.50+ are required.

The walkthrough takes about five minutes **after the local images are built**.
There are no published Cambium images yet. A first source build downloads Rust
and compiles both services; budget additional time and disk space for that step.

## Build once and start

From the repository root:

```sh
docker compose build
python3 -m venv .venv-demo
.venv-demo/bin/python -m pip install -r examples/requirements.txt
```

Start the initialized services together:

```sh
docker compose up -d --wait
```

The one-shot initializer creates private keys and SQLite metadata. Repeating
`up` preserves them, and an interrupted first initialization can be retried.
The images run as UID 10001 with a read-only root filesystem.
Metadata, Enroute storage, and the shared hook key use separate named volumes.
Enroute can read only the hook signing key, not Cambium's database or response key.

Only the two authenticated public listeners have host port mappings. Backend gRPC
and signed hooks stay on a dedicated Docker bridge, and Enroute's internal
listener binds to container loopback. Keep these ports private. This configuration
is for a trusted local machine; remote hosting needs TLS and operational controls.
The Docker host and anyone allowed to attach a container to this network remain
trusted. The bridge uses Docker's default NAT/filtering rules; do not disable
them or enable unprotected routing. See Docker's [bridge isolation](https://docs.docker.com/engine/network/drivers/bridge/)
and [localhost port publishing](https://docs.docker.com/engine/network/port-publishing/)
contracts.

## Bootstrap and run the agent

Create a tenant and its first human credential. The file is ignored by Git;
keep it private, and do not put its token in a URL or shell argument.

```sh
umask 077
docker compose exec -T cambium cambium --data-dir /var/lib/cambium/state \
  --json admin bootstrap --tenant-slug demo --human-name Operator \
  > .cambium-bootstrap.json
.venv-demo/bin/python examples/agent-loop.py \
  --credential-file .cambium-bootstrap.json
```

Run bootstrap once per tenant. For another demonstration, rerun only the example
script: it creates a unique repository. If the one-hour human token expires,
issue a replacement with the host admin commands in
[local development](local-development.md).

The example creates a separate agent identity and a five-minute, repo-scoped
write token. Through the official MCP Python client it creates a commit, checks
idempotent replay, reads the file, creates a five-second scratch branch, writes
an experiment, waits for expiry and compaction, and confirms the discarded commit
is absent from the new generation. It then clones the surviving commit with
stock Git and prints a JSON result. The checkout is under `.cambium-demo/` and
contains `hello.txt`. It does not store the agent token in Git configuration.

To connect another MCP client, use `http://127.0.0.1:8080/mcp` with an
`Authorization: Bearer <token>` header. Credential provisioning is manual in
Phase 1a. All 20 operations and their request schemas are discoverable through
`tools/list`; see the [HTTP/MCP contract](api/operations.md).

## Observe the lifecycle

The compose demo deliberately uses fast timings: a one-second maintenance tick,
a five-second automatic compaction delay or one pending expiry, a one-second
drain, and Enroute's one-second sweep with three-second retention. It stores
layers as files so reclamation can be inspected. These are demonstration settings,
not the [service defaults](branch-lifecycle.md#recovery-and-timing).

`get_reclamation` reports the active generation, pending expiries, stage, and
conflict/rescue refs. A completed compaction means Cambium deleted the old backend
generation; Enroute's physical file sweep follows asynchronously. Retained refs
and rescue refs intentionally keep the objects they reach.

```sh
docker compose logs --tail 30 cambium enroute
docker compose exec -T enroute python3 -c \
  'from pathlib import Path; print("\n".join(str(p) for p in Path("/var/lib/enroute/objects/repos").rglob("*.layer")))'
```

For an automated check of container port mappings, credentials, key separation,
and physical old-generation cleanup, run the [compose verification script](../scripts/verify_compose.py)
after saving the example's JSON result:

```sh
.venv-demo/bin/python examples/agent-loop.py \
  --credential-file .cambium-bootstrap.json > /tmp/cambium-demo-result.json
python3 scripts/verify_compose.py --credential-file .cambium-bootstrap.json \
  --result-file /tmp/cambium-demo-result.json
```

## Stop and restart

```sh
docker compose down
docker compose up -d --wait
```

`down` keeps all data volumes. Startup recovers interrupted lifecycle operations.
To deliberately erase this disposable deployment, `docker compose down --volumes`
removes its database, keys, and every repository. Do not use that command on data
you intend to retain.

Use [local development](local-development.md) for running the binaries directly,
and [branch lifecycle](branch-lifecycle.md) for recovery and timing details.
