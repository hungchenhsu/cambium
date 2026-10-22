# Run the development service locally

This setup runs the current Phase 1a service on one trusted development host.
It requires the [development prerequisites](development.md), including Git 2.50
or later. All listeners bind to loopback. The container quickstart and ephemeral
branch reclamation are still being implemented; this is not a production setup.

## Build and initialize

From the repository root:

```sh
python3 scripts/prepare_enroute.py
(cd target/enroute && cargo build --locked --bin enroute)
cargo build --locked -p cambium-server --bin cambium
target/debug/cambium init
```

Initialization creates `.cambium/` with mode 0700, its files with mode 0600, and
an empty SQLite store. It refuses an existing directory. It generates a separate
response-encryption key and an Ed25519 hook signing key; neither is committed.
Keep the response key with the deployment's secret backups, separate from database
exports. The service fails if the key is missing, malformed, symlinked, or publicly
readable. It never replaces a missing key during startup.

## Start both processes

In one terminal, run the pinned patched Enroute build. The configuration expands
environment variables; the private signing key is not an argument or repository
file:

```sh
CAMBIUM_DATA_DIR="$PWD/.cambium" \
ENROUTE_HOOK_SIGNING_KEY="$(cat .cambium/hook-signing.pem)" \
  target/enroute/target/debug/enroute \
  --config "file://$PWD/examples/enroute-local.toml" serve
```

In another terminal:

```sh
target/debug/cambium serve
```

The service loads `.cambium/cambium.toml`. It takes an exclusive process lock
before recovering interrupted repository operations or idempotency reservations.
Only one service process may open the same deployment. Host administration can
run while it serves requests; token revocation is checked on every new request.

| Listener | Local address | Intended caller |
| --- | --- | --- |
| JSON API and MCP | `127.0.0.1:8080` | Authenticated clients |
| Signed hooks | `127.0.0.1:8081` | Enroute only |
| Backend gRPC | `127.0.0.1:8082` | Cambium only |
| Git HTTP | `127.0.0.1:8083` | Authenticated Git clients |
| Enroute internal | `127.0.0.1:8084` | Enroute only |

Loopback restricts remote access, but does not isolate other processes on the
same host. Do not publish gRPC, internal, or hook ports. Remote deployments must
isolate them on private networking and terminate TLS for public API and Git.
Keep all four hooks enabled and `git.create_on_push = false`.

## Create a tenant and call the API

The host-only admin command creates the tenant, its first human identity, and
an expiring `tenant:admin` token. Save its single output privately:

```sh
umask 077
target/debug/cambium --json admin bootstrap \
  --tenant-slug demo --human-name Operator > .cambium/bootstrap.json
export CAMBIUM_TOKEN="$(python3 -c 'import json; print(json.load(open(".cambium/bootstrap.json"))["token"])')"
curl --fail-with-body http://127.0.0.1:8080/v1/operations/create_repository \
  -H "Authorization: Bearer $CAMBIUM_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"example","idempotency_key":"example-create-1"}'
```

Use the returned `repository_id` with `write_changeset`, then clone
`http://127.0.0.1:8083/demo/example.git` with stock Git. Git accepts the token as
the HTTP Basic password (any username), or in an `Authorization: Bearer` header.
Do not embed credentials in the clone URL. See [operations](api/operations.md)
for request shapes and MCP configuration.

Bootstrap and host-issued tokens default to one hour. `--ttl-seconds` can request
a lifetime within the configured human limit. An existing human can obtain a
replacement through `admin issue-human-token --tenant-id ... --identity-id ...`.
`admin revoke-token --tenant-id ... --token-id ...` revokes a token and its
descendants. `admin disable-identity --tenant-id ... --identity-id ...` disables
an identity. These commands act directly on the local store and have no network
equivalent for tenant creation or human credential recovery.

## Verify the transport and MCP client

```sh
ENROUTE_BIN="$PWD/target/enroute/target/debug/enroute" \
  cargo test --locked -p cambium-server --test transports -- --ignored
```

The test starts separate disposable servers and checks API/MCP errors and replay,
signed hooks, Basic/Bearer Git credentials, clone/fetch/push interoperability,
cross-tenant and read-only denial, reserved refs, and revocation after authorize.
It does not modify the deployment above.

To include an independent official Python MCP client:

```sh
python3 -m venv /tmp/cambium-mcp-client
/tmp/cambium-mcp-client/bin/python -m pip install 'mcp==2.3.0'
MCP_CLIENT_PYTHON=/tmp/cambium-mcp-client/bin/python \
ENROUTE_BIN="$PWD/target/enroute/target/debug/enroute" \
  cargo test --locked -p cambium-server --test transports -- --ignored --nocapture
```

The SDK check negotiates MCP, lists tool schemas, creates a repository, writes and
replays a changeset, reads it, verifies a structured error, and deletes the test
repository. Its test credentials are passed only in the subprocess environment.
