# Comparison

As of 2026-10-18, based on each project's public documentation. Corrections are welcome: open an issue with a link to the source.

This page explains the gap Cambium targets. It is not a ranking. Several of these projects are good choices for what they were built to do, and Enroute is Cambium's first storage backend.

## Open-source servers

| Project | License | Stock git clone and push | Write without a checkout | Storage | Gaps for agent fleets, per its docs |
| --- | --- | --- | --- | --- | --- |
| [Enroute](https://github.com/enroute-sh/enroute) | Apache-2.0 | Yes, smart HTTP | Yes, the `CreateCommit` gRPC call | Object storage plus a replicated log per repository | The gRPC API does not authenticate callers (git access goes through hooks); unreachable branch objects are not garbage-collected; no compatibility between 0.x releases ([limitations](https://enroute.sh/docs/reference/limitations)) |
| [walgit](https://github.com/tobi/walgit) | MIT | Yes, smart HTTP | No: its JSON API manages repositories and settings, but content writes go through `git push` ([API.md](https://github.com/tobi/walgit/blob/main/web/API.md)) | Object storage bucket | No commit or content-write API |
| Gitea, Forgejo | Open source (licenses vary) | Yes | Yes, a multi-file commit endpoint ([Gitea](https://docs.gitea.com/api/1.23/operations/repo-change-files/)) | A git repository per project on server disks | Gitea has an official MCP server ([gitea-mcp](https://gitea.com/gitea/gitea-mcp)); no documented ephemeral branches with automatic cleanup |
| GitLab | Open source core | Yes | Yes, a multi-file commit endpoint ([docs](https://docs.gitlab.com/api/commits/)) | A git repository per project on server disks (Gitaly) | Has service accounts for non-human identities ([docs](https://docs.gitlab.com/user/profile/service_accounts/)) and an MCP server ([docs](https://docs.gitlab.com/user/model_context_protocol/mcp_server/)); no documented ephemeral branches with automatic cleanup |
| [Mononoke](https://github.com/facebook/sapling/blob/main/eden/mononoke/README.md) | GPL-2.0 (Sapling repository) | A git server over HTTP, besides Sapling clients ([architecture](https://github.com/facebook/sapling/blob/main/eden/mononoke/docs/1.3-architecture-overview.md)) | Not checked | MySQL, SQLite, file, or S3 backends | Has a derived-data service. Its README says it is still in development and omits some functions, such as Thrift-based APIs |
| [Oak](https://oak.space/docs/git) | Apache-2.0 (CLI) | Clone and fetch return a single snapshot commit; push fails | Through Oak's own client | Oak's own format | Not git-compatible for writes |

## Other agent-focused open-source projects

- [agent-git-service](https://github.com/ngaut/agent-git-service) (Apache-2.0, Go) describes itself as a reimplementation of GitHub for agents.
- agentgit.co ([source](https://github.com/Zabaca/zbc), MIT) is an early git host for agents, which its authors describe as built on walgit.

## Closed or not yet available

- **Hosted, closed:** Cloudflare Artifacts and Pierre's code.storage offer git-compatible storage with an API, and code.storage offers ephemeral branches. Cursor Origin is a hosted forge for agents. None can be self-hosted.
- **Announced:** Entire has said it will open-source its git backend ([announcement](https://entire.io/news/entire-launches-distributed-git-network-for-the-agent-era)). East River Source Control (ERSC) is in private beta.
- **Internal:** Anthropic's Scion is not offered outside Anthropic ([post](https://www.anthropic.com/engineering/redesigning-source-control)).

## What Cambium adds

Each project above covers some of these pieces. Cambium's aim is to combine them into one lifecycle for agent fleets, over storage engines that can be swapped.

| Capability | Where it lives |
| --- | --- |
| Authentication, tenant isolation, scoped short-lived tokens | Cambium, Phase 1a |
| Human and agent identity fields | Cambium, Phase 1a (enforced in Phase 3) |
| Provenance | Cambium, Phase 1b |
| Ephemeral branches with a TTL, cleanup, and storage reclamation | Cambium, Phase 1a |
| Agent API and MCP server, idempotent writes, structured errors | Cambium, Phase 1a |
| Server-side log, path history, blame, and search | Cambium's derived-data index, Phase 2 |
| Review and merge policy | Cambium, Phase 3 |
| Git protocol, object storage, durability | The storage backend (Enroute first) |
