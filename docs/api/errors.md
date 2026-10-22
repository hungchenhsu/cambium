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

Successful writes may include a `path_collision` warning. Its `details.paths`
is an array of path pairs with exactly one member of each plain/base64 pair.
It means the stored byte paths differ only by Unicode normalization or case
and may collide on a client's filesystem. Cambium preserves both byte paths.
