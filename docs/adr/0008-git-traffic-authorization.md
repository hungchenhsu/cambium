# ADR-0008: How git traffic is authorized

- **Status:** Accepted
- **Date:** 2026-10-18

## Context

Stock git clients must clone, fetch, and push ([ADR-0002](0002-git-compatible-edge.md)). No request may reach a backend without Cambium's authorization.

Enroute terminates git smart HTTP and asks the application through four signed hooks ([ADR-0006](0006-enroute-spike.md)):

- `authorize`, once per HTTP request, with the URL path and the request headers, including credentials
- `visible_refs`, before each ref advertisement
- `pre_receive`, which must judge every ref command
- `post_receive`, best-effort and at most once

Enabled hooks fail closed. Enroute's gRPC listener authenticates nobody, and any caller that reaches it can read, write, and delete every repository.

There are two designs:

- **Hooks:** git clients talk to Enroute's git listener, and Cambium decides in the hooks.
- **Proxy:** Cambium terminates git HTTP itself and forwards it to Enroute.

## Decision

Git traffic goes to Enroute's git listener and is authorized by Cambium through Enroute's hooks. Cambium does not proxy git traffic in Phase 1a.

### URLs and credentials

- A repository's git URL is `<git base URL>/<tenant slug>/<repository name>.git`. The same path without `.git` names the same repository, so a repository name cannot itself end in `.git`.
- Clients send a Cambium token ([ADR-0011](0011-tenancy-and-authentication.md)) as the password of HTTP Basic credentials (any username), or as `Authorization: Bearer <token>`. The quickstart shows a credential helper, not a token in the URL.
- Enroute runs with `git.create_on_push = false`. Repositories are created only through Cambium's API, so the metadata store stays the source of truth.

### `authorize`

1. No credential, or a token that is unknown, expired, or revoked: deny with `DENIAL_UNAUTHORIZED` and the challenge `Basic realm="cambium"`. This happens before any repository lookup, so an anonymous caller learns nothing about which repositories exist.
2. The tenant slug is not the token's tenant, the repository does not exist in that tenant, or the token is restricted to other repositories: deny with `DENIAL_NOT_FOUND`.
3. A push with a token that may only read: deny with `DENIAL_FORBIDDEN`.
4. Otherwise grant:
   - `repo`: the repository's active backend repository ID ([ADR-0009](0009-ephemeral-branch-reclamation.md))
   - `actor`: the identity ID, never a name, an email address, or a token
   - `context`: the token ID, identity ID, tenant ID, repository ID, and backend repository ID

### `visible_refs`

The `refs/cambium/` namespace is reserved for the temporary refs of compaction ([ADR-0009](0009-ephemeral-branch-reclamation.md)) and is never advertised to clients. Every other ref, including the rescue refs under `refs/cambium-rescue/`, is visible to any token that may read the repository. Per-ref visibility is a Phase 3 policy.

### `pre_receive`

Cambium judges every command of a client push. It refuses:

- every command, when the token in `context` has been revoked or has expired since `authorize` (a large push can take minutes)
- every command, when the backend repository ID in `context` is no longer the repository's active one, or the repository is fenced for compaction. The reason tells the client to retry.
- any command under `refs/cambium/` or `refs/cambium-rescue/`
- a command whose ref name contains U+FFFD. Enroute's hook messages carry ref names as protobuf strings, so a name that was not valid UTF-8 cannot be judged by its real bytes ([ADR-0012](0012-api-transport-and-errors.md)).
- a command that creates a ref with the name of an ephemeral branch whose expiry has not been reclaimed yet ([ADR-0009](0009-ephemeral-branch-reclamation.md))

It allows every other command. Branch protection and merge policy arrive in Phase 3 and plug in here.

Pushes made by Cambium's own internal copies are judged by a separate rule, described below.

### `post_receive`

Cambium records the landed updates, for example to track the last activity on ephemeral branches. The call is best-effort, so nothing whose correctness matters depends on it. Before such decisions, such as compaction, Cambium reads refs with `list_refs`.

### Hook endpoint

- Cambium serves the hooks on a listener of its own, separate from its API, at the path `/api/enroute/hooks`.
- Before it reads the body, Cambium verifies each request as RFC 9421 with Ed25519:
  - the covered components are exactly `@method`, `@authority`, `@path`, and `content-digest`
  - `created` is within 300 s of the current time
  - `keyid` names a configured public key
  - `Content-Digest` is checked against the raw body bytes
  - `@authority` and `@path` come from the configured endpoint URL, not from the request
- A request that fails any check gets HTTP 401 and is not processed. Enroute then fails the git request.
- Each deployment generates its own signing key. Enroute holds the private key, and Cambium is configured with the public key. During rotation, Cambium accepts two keys.
- A replayed hook request within the 300 s window gets an answer, but the answer goes back to the replayer and grants nothing on the git connection. The listener is still private (below).

### Network exposure

| Listener | Exposure |
| --- | --- |
| Cambium API | Public, behind TLS termination |
| Enroute `listen.git` | Public, behind TLS termination |
| Enroute `listen.api` (gRPC) | Reachable only by Cambium. Never published |
| Cambium hook listener | Reachable only by Enroute. Never published |

The container compose file puts Cambium and Enroute on an internal network and publishes only the Cambium API and Enroute's git listener.

### Cambium as a git client

Compaction copies refs with Enroute's `PushToRemote` toward Enroute's own git listener on the internal network ([ADR-0009](0009-ephemeral-branch-reclamation.md)). This requires `git.allow_private_remotes = true`.

- Cambium mints an internal token for each copy. The token is valid only for the destination backend repository ID and only for the duration of the copy. It is never stored in plain text and never given to a client.
- `authorize` recognizes such tokens and grants exactly the destination.
- For such a push, `visible_refs` returns every ref, `pre_receive` allows every command while the token is valid and refuses every command otherwise, and `post_receive` is ignored. At the API and the MCP server, an internal token resolves to no principal. The client rules above do not apply: a compaction must be able to write to a generation that is not active yet, while the repository is fenced, and under reserved names.
- Cambium never passes a user-supplied URL to `PushToRemote`, so the setting cannot be used to reach other hosts.

### Rejected: a Cambium git proxy

A proxy would give one public endpoint and a place for rate limits. But it would put long-running git streams through Cambium and duplicate what Enroute was built to do, for no security gain over signed hooks that fail closed. It remains an option for a backend that offers no callback ([ADR-0007](0007-storage-backend-interface.md)), or if rate limiting cannot be done in `authorize`.

## Consequences

- Each git operation costs at least two `authorize` calls. Token lookup must be fast, and Phase 1a does it with an indexed lookup by token hash.
- Enroute has no rate limits or quotas. Until Cambium adds its own, `authorize` is the only place to throttle git traffic.
- The tests for this ADR are:
  - no credential, and an invalid, expired, or revoked token: 401
  - cross-tenant clone and push: 404
  - push with a read-only token: 403
  - a push under `refs/cambium/`: refused
  - an internal token used for any other destination than the one it was minted for: refused
  - a hook request with a bad signature, a changed body, or a stale `created`: rejected
  - Enroute's gRPC port and Cambium's hook port are not reachable from the host in the compose setup
