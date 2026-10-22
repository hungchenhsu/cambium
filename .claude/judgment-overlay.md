# Judgment Overlay: cambium (last updated 2026-10-23)

Generic rules live in each agent's own global configuration. This file holds only cambium-specific judgment: caution domains, verification commands, permissions, and where lessons go.

## 1. Caution domains

- **Storage backends and durability:** backend adapters, moving repositories between backends, ref compare-and-swap, ephemeral branch cleanup, storage reclamation, and deletion. This is users' source code, and corrupted or lost data cannot be recovered.
- **Authentication, authorization, and tenant isolation:** tokens, git traffic authorization, network exposure of a backend's own API, directory permissions, human and agent identity, and approvals.
- **Licensing and third-party code:** license files, ADR-0003, crate `license` fields, new dependencies, imported code, and the DCO terms and workflow.
- **Public exposure:** secrets, credentials, personal data, and business information.
- **CI workflows** under `.github/workflows/`, because they run on contributors' pull requests.
- **Releases:** publishing to crates.io or container registries. Published names and versions cannot be reclaimed.
- **Deployment** of the hosted service, once it exists.

When unsure, anything that could make written data unreadable, let one tenant reach another tenant's data, or change anyone's license obligations is in a caution domain.

## 2. Verification commands

The Phase 0 workspace pins Rust 1.99.1 and cargo-deny 0.20.2. CI includes the DCO workflow, Rust checks, and the license policy. See `docs/development.md` for prerequisites and evidence boundaries.

| Change touches | Minimum verification | Source of truth |
| --- | --- | --- |
| Any commit | `git diff --check`; `git status --porcelain` lists only intended files; every commit has a `Signed-off-by` line | `.github/workflows/dco.yml` |
| `LICENSE-*` | Byte-for-byte comparison with the official text: `https://www.apache.org/licenses/LICENSE-2.0.txt`, `https://www.gnu.org/licenses/agpl-3.0.txt` | Official sites |
| `CODE_OF_CONDUCT.md` | Matches Contributor Covenant 2.1, except for the contact line | `https://www.contributor-covenant.org/version/2/1/code_of_conduct/code_of_conduct.md` |
| `README*.md` | Every translation states the same facts as `README.md`; links resolve | `README.md` |
| `docs/`, `ROADMAP.md` | Links and paths resolve | n/a |
| `.github/workflows/` | `actionlint` when available; third-party actions pinned to a full commit SHA | n/a |
| Rust workspace | `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`; `cargo test --workspace --all-features --locked` | `.github/workflows/ci.yml` |
| Crate licenses or dependencies | `python3 scripts/check_licenses.py` | ADR-0003, `deny.toml`, `deny-apache.toml` |
| License checker or policy | `python3 -m unittest discover -s scripts/tests -v` plus the license check above | `scripts/tests/test_license_policy.py` |

## 3. Permissions

- **Allowed directly:** adding a new ADR, fixing typos, and checking off ROADMAP items with evidence.
- **Ask the owner first:** changing the decision of an accepted ADR (supersede it with a new ADR).
- **Ask the owner first, and stop the PR at ready for the owner to merge:** license files, ADR-0003, the DCO terms in `CONTRIBUTING.md`, and `CODE_OF_CONDUCT.md`.

## 4. Where lessons go

- **Reusable operating rules:** `.claude/skills/<name>/SKILL.md`. None exist yet; create one when a rule is needed a second time.
- **Technical debt:** `docs/TECH_DEBT_REGISTER.md`, created with the first entry.
- **Architecture decisions:** `docs/adr/`.
