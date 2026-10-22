# ADR-0012: API transport and error code registry

- **Status:** Accepted
- **Date:** 2026-10-18

## Context

[ADR-0004](0004-agent-first-interfaces.md) sets the rules for machine interfaces:

- Responses are JSON.
- Errors carry a stable `snake_case` code, an English message, and structured details.
- Every mutation accepts an idempotency key.
- Output is compact and paginated.

[ADR-0005](0005-internationalization.md) adds that machine output never depends on locale, and that paths are byte strings that APIs must be able to represent even when they are not UTF-8. The MCP server and the CLI expose the same operations as the API.

The candidates were JSON over HTTP and gRPC. gRPC needs generated clients and HTTP/2. Agents, shell scripts, and MCP all speak JSON, and every language has an HTTP client.

## Decision

### Transport

- **Protocol.** The API is JSON over HTTPS, with resource paths under `/v1/`. Requests and responses use `application/json` in UTF-8.
- **Output.** Responses are not pretty-printed. Absent values are omitted, never `null`.
- **Request fields.** A request with a field the server does not know fails with `invalid_request`, so that a typo or a newer client is not ignored silently.
- **Response fields.** Clients ignore response fields they do not know.
- **Authentication.** `Authorization: Bearer <token>` ([ADR-0011](0011-tenancy-and-authentication.md)).
- **Request IDs.** Every response carries a `Request-Id` header.
- **Locale.** Machine output never reads `Accept-Language`, `LANG`, or `LC_*`.
- **OpenAPI.** An OpenAPI description is published with the full error code registry in Phase 1b.

### Versioning

- The version is the path prefix.
- **Additive changes stay in `/v1`:** new endpoints, new optional request fields, new response fields, and new error codes. Clients treat an unknown error code by its HTTP status and its `retryable` flag.
- **Breaking changes need `/v2`:** removing or renaming an endpoint, a field, or an error code, or changing what one means. The old version stays available for a deprecation period announced in the release notes.

### Errors

Every error response has this shape:

```json
{"error":{"code":"ref_conflict","message":"refs/heads/main is at 2d4f6a8c0e1b3d5f7a9c2e4b6d8f0a1c3e5b7d9f, not 9f2c1b7e4a8d3c5f6b0e2a1d4c7f8b3e5a6d9c0f","retryable":false,"details":{"ref":"refs/heads/main","expected":"9f2c1b7e4a8d3c5f6b0e2a1d4c7f8b3e5a6d9c0f","actual":"2d4f6a8c0e1b3d5f7a9c2e4b6d8f0a1c3e5b7d9f"},"request_id":"req_01jabcd..."}}
```

- `code` is what clients branch on.
- `message` is English, for people and logs. It is never translated and may change.
- `details` has a documented shape for each code.
- Retryable errors also send `Retry-After` when the server knows a useful delay.
- A successful response can carry `warnings`, a list of objects with `code`, `message`, and `details`. Warning codes come from the same registry.

### The registry

- Codes are defined once, as a Rust enum in the crate `cambium-protocol` (Apache-2.0). The enum records each code's HTTP status, whether it is retryable, and the shape of its details.
- `docs/api/errors.md` lists every code, and a test fails when the document and the enum disagree.
- Renaming or removing a code is a breaking change ([ADR-0004](0004-agent-first-interfaces.md)).

Phase 1a starts with these codes:

| Code | HTTP | Retryable | Meaning |
| --- | --- | --- | --- |
| `unauthenticated` | 401 | No | No token, or a token that is unknown, expired, or revoked |
| `permission_denied` | 403 | No | The token can see the resource but lacks the scope |
| `not_found` | 404 | No | No such resource, or one the token may not see |
| `invalid_request` | 400 | No | A malformed or invalid request. `details` names the field and the reason |
| `already_exists` | 409 | No | The name is taken |
| `ref_conflict` | 409 | No | A compare-and-swap expectation did not match. `details` has the ref, the expected value, and the actual value |
| `branch_expired` | 410 | No | The ephemeral branch has expired |
| `payload_too_large` | 413 | No | The request exceeds a limit. `details` has the limit in bytes |
| `unsupported_path` | 422 | No | The storage backend cannot store this path ([ADR-0007](0007-storage-backend-interface.md)) |
| `idempotency_key_reused` | 422 | No | The idempotency key was used for a different request |
| `idempotency_key_in_use` | 409 | Yes | A request with this idempotency key is still running |
| `contention` | 409 | Yes | Other writers kept changing the same ref while the server retried |
| `repository_busy` | 503 | Yes | The repository is fenced for compaction ([ADR-0009](0009-ephemeral-branch-reclamation.md)) |
| `backend_unavailable` | 503 | Yes | The storage backend did not answer |
| `internal` | 500 | Yes | An unexpected server error. The request ID identifies it in the logs. Retry with the same idempotency key |

Phase 1a starts with one warning code:

| Warning | Meaning |
| --- | --- |
| `path_collision` | A path in the resulting tree differs from another path only by Unicode normalization or by case, so the two collide on macOS and Windows filesystems ([ADR-0005](0005-internationalization.md)). `details` lists the paths |

### Idempotency

- Every mutating request (`POST`, `PUT`, `PATCH`, `DELETE`) accepts an `Idempotency-Key` header of 1 to 255 printable ASCII characters. This follows the IETF HTTPAPI draft, *The Idempotency-Key HTTP Header Field*.
- A key is scoped to the tenant and the identity.
- Every request is authenticated and authorized for its operation and resource before the idempotency store is consulted. A replay therefore never shows a stored response to a token that could not have made the request, such as a narrower token of the same identity.
- Cambium stores, for 24 hours, the key, a SHA-256 fingerprint of the method, path, and body, and the final response.
- A repeat with the same key and fingerprint returns the stored response with the header `Idempotent-Replayed: true`, and does nothing else.
- A repeat with a different fingerprint fails with `idempotency_key_reused`. A repeat while the first request is still running fails with `idempotency_key_in_use`.
- Only outcomes that a retry of the same request cannot change are stored: successes, and the errors `invalid_request`, `already_exists`, `ref_conflict`, `branch_expired`, `payload_too_large`, and `unsupported_path`. Nothing final is stored for other errors, such as `unauthenticated`, `permission_denied`, `not_found`, and every retryable error, so the client can fix the cause and retry with the same key.
- A retryable error can hide a change that was applied, for example when the backend wrote a commit and Cambium then failed to record the result. So every mutation that moves a ref runs with a compare-and-swap expectation. When the client gives none, Cambium uses the tip it read before the call. The reservation keeps that expectation after a retryable error, and a retry with the same key runs against it. A change that was applied therefore cannot be applied twice: the retry fails with `ref_conflict`, whose details show the current tip.
- A mutation that moves no ref, such as minting a token, can run twice after a retryable error. The result is a harmless duplicate, such as an extra token that expires.

### Bytes, paths, and text

- **Fields that may not be UTF-8.** Every field that carries git data that may not be valid UTF-8 has a twin with the suffix `_b64`, holding standard base64 with padding. This covers paths, file contents, commit messages, and author and committer names and emails.
  - The server sends exactly one field of each pair: the plain field when the bytes are valid UTF-8, the `_b64` field otherwise.
  - In requests, `_b64` twins are accepted for paths and file contents, and a request may send either field of a pair. Sending both fails with `invalid_request`. Commit messages and author and committer fields are UTF-8 strings in requests, because Enroute's API cannot carry other bytes there ([ADR-0006](0006-enroute-spike.md)).
  - A read can ask for `_b64` always, for example for binary files.
- **Paths.** Paths are passed through exactly. They are never normalized.
- **Ref names.** Ref names are UTF-8 strings. Git allows other bytes, but Enroute's API and hooks cannot carry them, so a push that names such a ref is refused ([ADR-0008](0008-git-traffic-authorization.md)).
- **IDs.** Object IDs are 40 lowercase hexadecimal characters. Cambium IDs use the prefixes in [ADR-0010](0010-metadata-store.md).
- **Times.** Cambium's own timestamps are RFC 3339 in UTC with `Z`. Commit timestamps are RFC 3339 with the commit's own offset, such as `2026-10-18T18:25:31+08:00`, so the offset survives.

### Lists

- Lists take `limit` (default 100, maximum 1000) and an opaque `page_token`. A response carries `next_page_token` only when another page exists.
- A result that a limit cut short is marked `"truncated": true` and is never presented as complete.

### MCP

The MCP server calls the same operation layer as the HTTP API. Its tools take and return the same JSON shapes. A failed call returns the same error object, marked as a tool error.

## Consequences

- Any HTTP client can call Cambium. No generated code is needed.
- The pairs of `_b64` fields make responses slightly more complex. In return, the common case stays compact and nothing is lost for byte paths.
- Rejecting unknown request fields means a newer client learns when a server is too old for a feature, instead of having the field ignored.
- The registry is a public contract from the first release. Codes are added with care, because they cannot be taken back without `/v2`.
