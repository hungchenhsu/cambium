# Technical debt register

Known gaps that were left out on purpose. Each entry says where it came from
and what would close it. Remove an entry in the pull request that resolves it.

| Item | Source | Why it is open | To close it |
| --- | --- | --- | --- |
| OpenAPI success bodies are untyped; `OperationResult` is a generic object | [#25](https://github.com/hungchenhsu/cambium/pull/25) | Responses are built as JSON values, not typed structs | Give each operation a response type with a JSON Schema, and generate it into `docs/api/openapi.json` |
| `cambium-cli` speaks plain HTTP only | [ADR-0019](adr/0019-cli.md) | TLS adds a cryptography dependency that the ADR-0003 allow-list must cover | Decide the TLS stack in an ADR, then enable it in `reqwest` |
| `cambium-cli` has only the `en` catalog | [ADR-0019](adr/0019-cli.md), [ADR-0005](adr/0005-internationalization.md) | The first release needed one locale | Add `locales/zh-TW/cli.ftl` and locale negotiation for human output only |
| A failed provenance record is not logged | [#26](https://github.com/hungchenhsu/cambium/pull/26) | The server crate has no logging yet | Log the store error with the request ID when `provenance_recorded` is false |
| Commits pushed with git carry no provenance | [ADR-0018](adr/0018-commit-provenance.md) | Hooks do not carry model or session claims | Decide whether to record the attested pusher per ref update |
| The git edge (ADR-0007 section 7) and the stock-git half of section 5 are tested per adapter, not by the conformance suite | [#27](https://github.com/hungchenhsu/cambium/pull/27) | They need a git endpoint and policy wiring that differ per backend | Add a git-edge harness to `cambium-backend-conformance` before a second backend lands |
