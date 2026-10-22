# Operations over HTTP and MCP

The current service exposes the same strict JSON requests through
`POST /v1/operations/{name}` and MCP tools at `/mcp`. Send
`Authorization: Bearer <token>` with every request. The authenticated token selects
the tenant; no request accepts a tenant ID. Unknown request fields fail. See the
[error registry](errors.md), [transport ADR](../adr/0012-api-transport-and-errors.md),
and [local setup](../local-development.md).

Every mutation accepts an optional `idempotency_key` string of 1–255 printable
ASCII characters. HTTP also accepts `Idempotency-Key`; if both are present they
must match. Reuse the same key and input after a connection failure. The scope is
the tenant and identity, and the 24-hour response cache is shared across HTTP and
MCP. Authentication and current authorization precede every replay.

## Request fields

Fields shown with `?` are optional. Types and defaults are also published in the
MCP `tools/list` input schemas. IDs are strings; repository IDs have the `repo_`
prefix. List limits default to 100 and cannot exceed 1000.

| Operation | Input | Scope |
| --- | --- | --- |
| `create_repository` | `name`, `default_branch?` (main) | repo:admin |
| `get_repository` | `repository_id` | repo:read |
| `list_repositories` | `limit?`, `page_token?` | repo:read |
| `delete_repository` | `repository_id` | repo:admin |
| `write_changeset` | `repository_id`, `message`, `changes`, `branch?` (main), `expected?`, `author?`, `committer?` | repo:write |
| `read_file` | `repository_id`, `path` or `path_b64`, `revision?` (HEAD), `always_base64?` (false) | repo:read |
| `list_tree` | `repository_id`, `revision?` (HEAD), `recursive?` (true), `limit?`, `page_token?` | repo:read |
| `diff` | `repository_id`, `from`, `to`, `limit?`, `page_token?` | repo:read |
| `create_identity` | `kind` (human or agent), `name`, `owner_id?` (required for agent) | tenant:admin |
| `get_identity` | `identity_id` | tenant:admin |
| `mint_token` | `identity_id`, `scope`, `repositories?`, `ttl_seconds?` (3600) | Own identity with narrower privileges; tenant:admin may also mint for agents |
| `revoke_token` | `token_id` | tenant:admin |
| `disable_identity` | `identity_id` | tenant:admin |

An absent token repository restriction means all repositories; an empty array
means none. Token expiry cannot exceed the parent's expiry or the configured
maximum for the identity's kind. An agent cannot mint a human credential.
Choose `ttl_seconds` below the parent's remaining lifetime; for example, delegate
1800 seconds from a freshly bootstrapped one-hour token. An omitted lifetime is
3600 seconds and is rejected when the parent has less than that remaining.

## Write and read

`write_changeset` accepts puts and deletes. A put contains `action: "put"`, a
`path`/`path_b64` pair, a `contents`/`contents_b64` pair, and optional `mode`
(`regular`, `executable`, or `symlink`). A delete contains `action: "delete"` and
a path pair. Send one member of each pair; `_b64` uses padded standard base64.
Paths are never normalized, and reads never follow a symlink.

```json
{"repository_id":"repo_01...","message":"Write a greeting","changes":[{"action":"put","path":"hello.txt","contents":"hello\n"}],"idempotency_key":"greeting-1"}
```

`expected` is `{"kind":"absent"}` or
`{"kind":"at","object_id":"<40 lowercase hexadecimal characters>"}`. When
omitted, the first attempt captures the branch tip before writing and retains
that expectation across retries. A lost backend response can therefore yield
`ref_conflict` on retry even when the original commit landed; it cannot append
the same write a second time. A completed response replays the original IDs.

An explicit author or committer contains `name`, `email`, and RFC 3339 `time`.
Successful writes return commit/tree IDs and `path_collision_check_complete`.
False means warning scanning was incomplete; it does not invalidate the commit.

Reads accept a full object ID, `HEAD`, a full ref, or a short branch name.
`read_file` returns exact contents in `contents` or `contents_b64`. Tree and diff
responses use `entries`, `truncated`, and optional `next_page_token`; cursors are
bound to the repository, query, and resolved revision. Follow a cursor with the
same query. No request returns a partial tree as though it were complete.

## MCP transport

Configure an MCP client with a Streamable HTTP URL such as
`http://127.0.0.1:8080/mcp` and the Bearer authorization header. Credentials are
provisioned by the operator; this pre-alpha service does not implement OAuth
discovery. It supports protocol versions `2025-11-25`, `2025-06-18`, and
`2025-03-26`, and advertises tools only. It has no sessions or server-initiated
events; authenticated GET/DELETE requests return 405, and accepted notifications
return 202 with no body. Each POST is authenticated independently.

Tool success and failure both contain the operation JSON as `structuredContent`
and a JSON text content block. Failures set `isError: true`. MCP framing errors
use JSON-RPC errors; the JSON-RPC envelope uses `id: null` when the request ID is
unknown. Cambium operation payloads omit absent values.

Requests carrying an `Origin` must match an exact `allowed_origins` value in
the deployment configuration. The default permits no browser origins. This
check applies even with a valid credential. Public listeners require TLS
termination outside the trusted local development setup.
