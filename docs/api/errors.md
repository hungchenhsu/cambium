# Error codes

The `/v1` API and MCP tools share this registry. Machine output is English and
does not depend on locale. The canonical definitions are in
[`cambium-protocol`](../../crates/cambium-protocol/src/errors.rs); a test checks
the table against the enum. See [ADR-0012](../adr/0012-api-transport-and-errors.md)
for transport and retry semantics.

Every failure contains `error.code`, `error.message`, `error.retryable`,
`error.details`, and `error.request_id`. Messages may change; clients branch on
the stable code. A retryable error does not prove that the backend applied no
change: retry the same request with the same idempotency key.

| Code | HTTP | Retryable | Details |
| --- | --- | --- | --- |
| `unauthenticated` | 401 | No | empty object |
| `permission_denied` | 403 | No | empty object |
| `not_found` | 404 | No | empty object |
| `invalid_request` | 400 | No | field, reason |
| `already_exists` | 409 | No | empty object |
| `ref_conflict` | 409 | No | ref, expected?, actual? |
| `branch_expired` | 410 | No | empty object |
| `payload_too_large` | 413 | No | limit_bytes |
| `unsupported_path` | 422 | No | path or path_b64 |
| `idempotency_key_reused` | 422 | No | empty object |
| `idempotency_key_in_use` | 409 | Yes | empty object |
| `contention` | 409 | Yes | empty object |
| `repository_busy` | 503 | Yes | empty object |
| `backend_unavailable` | 503 | Yes | empty object |
| `internal` | 500 | Yes | empty object |

`field` and `reason` are strings. `ref` is the full ref name; `expected` and
`actual` are full object IDs, omitted when the corresponding ref is absent.
`limit_bytes` is an integer. Path details contain exactly one of the UTF-8
`path` string or the standard padded-base64 `path_b64` string. Other detail
objects are empty in this version. Absent values are never serialized as `null`.

## Warnings

A successful response can carry `warnings`, a list of objects with `code`,
`message`, and `details`. Warning codes follow the same stability rules as error
codes, and clients ignore warning codes they do not know.

| Warning | Details |
| --- | --- |
| `path_collision` | groups |

`groups` is an array of path groups. Each group lists two or more stored paths
that differ only by Unicode normalization or case and may therefore collide on a
client's filesystem; each path is an object with exactly one of `path` or
`path_b64`. Cambium preserves every byte path. Changeset responses also carry
`path_collision_check_complete`; `false` means scanning was incomplete.

## Machine-readable contract

[`openapi.json`](openapi.json) is an OpenAPI 3.1 description of every
`/v1/operations/{name}` request, generated from the same request schemas that
MCP `tools/list` publishes. Its `ErrorResponse` schema gives the exact `details`
shape for each code, and `x-cambium-error-codes` and `x-cambium-warning-codes`
repeat these tables. A test fails when the published file differs from the
code; regenerate it with
`CAMBIUM_UPDATE_OPENAPI=1 cargo test -p cambium-server --lib openapi`.
Success bodies are described in [operations](operations.md); they are not yet
typed in the OpenAPI document.
