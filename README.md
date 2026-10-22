# cambium

> **Code name, pre-alpha.** Cambium has a tested SQLite authorization store, an Enroute adapter, and shared operations with durable idempotency, but no runnable service yet.
> The final product name will be chosen before a public launch.

[繁體中文](README.zh-TW.md)

Cambium is an open-source, agent-first layer for git repositories. Agents get an API and an MCP server to create repositories, write changes without a checkout, and use short-lived branches that clean up after themselves. Stock git keeps working. Storage is handled by pluggable open-source git storage engines, starting with [Enroute](https://github.com/enroute-sh/enroute).

## Why Cambium

AI coding agents create repositories, branches, and commits faster than git hosting was built to handle. Two kinds of open-source servers exist today, and each covers part of what agent fleets need:

- **Forges** such as Gitea, Forgejo, and GitLab offer commit APIs and authentication. Gitea and GitLab also offer MCP servers, and GitLab offers service accounts. All of them keep each repository as git on server disks.
- **Object-storage engines** such as Enroute and walgit scale better. They leave authentication, tenancy, and policy to the application built on them.

Early agent-focused projects add some of the missing pieces, such as agent accounts or expiring repositories. Each is tied to a single storage design.

Cambium is that application layer for agent fleets, over storage engines you can swap:

- tenant isolation
- separate identities for humans and agents
- ephemeral branches whose storage is reclaimed
- an agent API and MCP server
- provenance
- merge policy

Server-side history built on derived data comes in Phase 2. Mononoke publishes that capability but describes itself as still in development.

[docs/comparison.md](docs/comparison.md) has the details and sources, as of October 2026.

## Goals

- **Agent-first API.** Create repositories, write changes, and read history through an API and MCP, without a checkout.
- **Git-compatible.** Stock `git` can `clone`, `fetch`, and `push`. With Enroute as the backend, this requires Git 2.50 or later.
- **Safe for agent fleets:**
  - tenant isolation and scoped, short-lived tokens
  - separate human and agent identities
  - ephemeral branches with a TTL and cleanup
  - idempotent writes and structured errors
  - machine output that does not depend on locale
- **Pluggable storage.** Storage engines sit behind one interface, and the first is Enroute.
- **Self-hostable.** The same software runs as a multi-tenant hosted service or as a single-tenant deployment you operate yourself.

## Roadmap

1. **Agent layer.** Phase 1a is a slice you can try in five minutes: tenants and tokens, an API and MCP server, changesets without a checkout, ephemeral branches, and git access, all over Enroute. Phase 1b adds production basics.
2. **History and scale.** A derived-data index for log, path history, blame, and search, event streams, and a second storage backend.
3. **Review and merge policy.** Separate human and agent identities, approvals bound to commits, risk tiers, and policy hooks for external review tools.

The full checklist is in [ROADMAP.md](ROADMAP.md). All design decisions: [docs/adr](docs/adr/README.md).

To build and verify the workspace, see [Development](docs/development.md).

## Prior art

Cambium builds on ideas from these projects:

- Meta's [Sapling](https://github.com/facebook/sapling) and its server, [Mononoke](https://github.com/facebook/sapling/blob/main/eden/mononoke/README.md), especially the Bonsai changeset data model and derived data.
- Google's Piper.
- Anthropic's Scion, described in [Redesigning source control](https://www.anthropic.com/engineering/redesigning-source-control) (2026).
- [Jujutsu](https://github.com/jj-vcs/jj), especially first-class conflicts.

Cambium does not contain code from any of these projects.

## License

- Clients, protocol definitions, SDKs, and embeddable libraries: Apache-2.0 ([LICENSE-APACHE](LICENSE-APACHE)).
- Server components: AGPL-3.0-only ([LICENSE-AGPL](LICENSE-AGPL)).

Everything else in this repository, including documentation, is Apache-2.0. Each crate declares its license in `Cargo.toml`. See [ADR-0003](docs/adr/0003-licensing.md).

## Contributing

Contributions are welcome, including ones written with AI coding agents. Every commit needs a [DCO](https://developercertificate.org/) sign-off (`git commit -s`). See [CONTRIBUTING.md](CONTRIBUTING.md). Report security issues privately as described in [SECURITY.md](SECURITY.md).
