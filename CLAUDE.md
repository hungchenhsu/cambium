# Cambium: agent entry point

This file is the single source of truth for repository rules and the repository map. Claude Code reads it directly; other agents reach it through `AGENTS.md`. Keep it short: decisions belong in `docs/adr/`, plans in `ROADMAP.md`.

## 1. What this is and where it stands

- `cambium` is the code name for an open-source, agent-first layer for git repositories, written in Rust, over pluggable git storage engines (Enroute first). The final product name will be chosen before public launch.
- **The gap it fills:** forges and object-storage git engines each cover part of what agent fleets need ([docs/comparison.md](docs/comparison.md)). Cambium combines the rest into one lifecycle over storage engines that can be swapped: tenant isolation, human and agent identities, ephemeral branches whose storage is reclaimed, an agent API and MCP server, provenance, and merge policy. Judge every scope decision against this gap. Do not rebuild what a storage backend already does well.
- **Stage:** Phase 1a in progress. Protocol/backend contracts and the SQLite metadata and authorization store exist. The network service and Enroute adapter are not implemented yet. See `ROADMAP.md`.
- The repository is public. Contributions follow `CONTRIBUTING.md` and require a DCO sign-off.

## 2. Language

Everything committed to this repository is in English: code, comments, documentation, commit messages, pull request text, and issue and review replies. The only exception is README translations (`README.<locale>.md`), which change in the same commit as `README.md`. For anything committed here, this rule overrides personal agent defaults.

## 3. Repository map

- `README.md`, `README.zh-TW.md`: public introduction. Keep the translation in sync.
- `ROADMAP.md`: phase checklist. Check items off in the pull request that completes them.
- `docs/comparison.md`: dated comparison with existing projects. Update it, with sources, when the landscape changes.
- `docs/adr/`: architecture decision records, indexed in `docs/adr/README.md`. An accepted ADR's decision is never rewritten; a new ADR supersedes it, and only the old ADR's Status line changes.
- `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`: community policies.
- `.github/`: issue and pull request templates, `CODEOWNERS`, and the DCO workflow.
- `crates/`: protocol, backend contracts, conformance fixtures, SQLite store, and Enroute/server scaffolds. Layout and boundaries: `docs/development.md`.
- `scripts/check_licenses.py`, `deny*.toml`: workspace license boundaries and dependency allow-lists.
- `LICENSE-APACHE`, `LICENSE-AGPL`: verbatim license texts. Never edit them. ADR-0003 says which license applies where.
- `AGENTS.md`: thin adapter for other agents, plus the Codex review guidelines.
- `.claude/judgment-overlay.md`: caution domains, verification commands, permissions, and where lessons go.

## 4. Environment

Rust **1.99.1** is pinned in `rust-toolchain.toml` (edition 2024, resolver 3). All crates live under `crates/`; `docs/development.md` records their responsibilities and license boundaries. Python 3.11+, Git 2.50+, and cargo-deny 0.20.2 are required for the checks.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
python3 scripts/check_licenses.py
python3 -m unittest discover -s scripts/tests -v
git diff --check
```

## 5. Project rules

- **Licensing (ADR-0003).** Every crate declares `license`. Apache-2.0 crates never depend on AGPL-3.0-only crates, and third-party dependencies follow the ADR-0003 allow-list. Never copy code from copyleft projects such as Sapling and Mononoke, even where the licenses would be compatible.
- **Prior art.** When a design borrows from another project, credit it in the README's "Prior art" section or in the relevant ADR.
- **Storage backends (ADR-0002).** Cambium's own state never lives in a backend. A backend's own API, such as Enroute's gRPC listener, is never reachable by anything except Cambium.
- **Machine output.** API, MCP, and CLI `--json` output is never translated and never depends on locale (ADR-0004, ADR-0005).
- **Commits.** Use Conventional Commits in English (`type(scope): description`) and sign off with `git commit -s`. Branch names follow `<type>/<short-description>`.
- **Public repository.** Never commit secrets, credentials, personal data, internal URLs, pricing, customer information, or business strategy beyond the public model in ADR-0003 (open source plus paid hosting and support).

## 6. Reading order for every task

This file, then `.claude/judgment-overlay.md`, then the ADRs relevant to the task, then `ROADMAP.md` for scope.
