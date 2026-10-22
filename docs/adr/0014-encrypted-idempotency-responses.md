# ADR-0014: Encrypted idempotency responses

- **Status:** Accepted
- **Date:** 2026-10-23
- **Supersedes:** ADR-0010 and ADR-0011's hash-only storage and show-once rule, only for the
  bounded replay of the response that originally minted a token

## Context

[ADR-0012](0012-api-transport-and-errors.md) requires every mutation, including
token minting, to return its original successful response when the client repeats
an idempotency key. [ADR-0011](0011-tenancy-and-authentication.md) says to retain
only token hashes and show a token once. A lost token-mint response cannot satisfy
both rules. Storing an unencrypted response would also leave a usable credential
in the metadata database.

## Decision

The authentication tables continue to store only SHA-256 token hashes. The
idempotency table stores all completed responses as encrypted envelopes, retained
for the 24-hour window in ADR-0012. This is the sole exception to hash-only token
storage: the encrypted original mint response can be replayed during that window.
There is no token-retrieval endpoint.

Use RustCrypto's XChaCha20-Poly1305 implementation with a random 192-bit nonce for
each envelope. Authenticate the envelope version, tenant, identity, idempotency
key, request fingerprint, and response status as associated data. An envelope
cannot be substituted into another identity's or operation's cache entry.

The deployment owns a separate random 256-bit response-encryption key. The
service composition must load it from a private file outside the database; on
Unix the directory is mode 0700 and the file is mode 0600. Missing or invalid key
material prevents startup. A new key is generated only by explicit deployment
initialization, never as a fallback when opening an existing store. Backups that
must preserve replay include this key through the operator's secret backup
procedure. Key loss makes existing responses unreadable; it does not invalidate
the separately hashed authentication tokens.

Authenticate and authorize the current request before reading its cached
response, including all token-delegation constraints. A narrower token of the
same identity cannot replay a response that minted a broader token. Encryption
does not replace tenant isolation or authorization.

HTTP and MCP also accept an optional `idempotency_key` JSON field, because an MCP
tool call has no per-tool HTTP header. HTTP still accepts `Idempotency-Key`; if
both are present, their values must match. Remove this transport field before
hashing the shared operation's method, canonical path, and JSON input. This is an
additive request field within `/v1`.

## Consequences

- A database-only disclosure does not reveal cached credentials without the
  deployment key. A compromise of the running service or of both files can.
- An authorized retry can receive the same token more than once, for at most the
  idempotency window. Its expiry and parent-token revocation still apply.
- SQLite WAL and backup retention can retain old encrypted pages after logical
  deletion. The key must remain separate from database exports.
- Each running reservation has an execution ID. Completion and reservation
  updates compare that ID, preventing an interrupted owner from overwriting a
  later retry. The single process recovers abandoned reservations at startup
  only after acquiring its deployment lock.

## References

- [RustCrypto chacha20poly1305](https://docs.rs/chacha20poly1305/0.11.0/chacha20poly1305/)
- [RFC 8439](https://www.rfc-editor.org/rfc/rfc8439)
