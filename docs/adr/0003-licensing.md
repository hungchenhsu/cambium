# ADR-0003: Licensing

- **Status:** Accepted
- **Date:** 2026-10-18

## Context

Cambium is fully open source under OSI-approved licenses, and a paid hosted service will be built on it. Clients, SDKs, and protocol definitions need a permissive license so anyone can adopt and embed them. The server needs a license that still counts as open source but requires anyone who offers a modified version as a network service to publish their changes.

## Decision

- **Apache-2.0** covers clients, protocol definitions, SDKs, and libraries meant to be embedded by other software.
- **AGPL-3.0-only** covers server components: binaries and crates that exist only to run the server.
- Files outside any crate, such as documentation, are Apache-2.0.
- Every crate declares its license in the `license` field of its `Cargo.toml`.
- An Apache-2.0 crate must never depend on an AGPL-3.0-only crate. Any binary or service built from such a crate would have to satisfy AGPL-3.0, which defeats the purpose of the permissive license.
- Storage backends run as separate programs. The current candidates, Enroute (Apache-2.0) and walgit (MIT), are permissively licensed, so running or bundling them is allowed.
- Third-party dependencies follow an allow-list enforced in CI:
  - Apache-2.0 crates may depend only on permissively licensed crates, such as Apache-2.0, MIT, BSD, ISC, Zlib, and Unicode-3.0.
  - AGPL-3.0-only crates may also depend on crates whose licenses are compatible with AGPL-3.0, such as LGPL and GPL-3.0.
- As project policy, do not copy code from copyleft projects into any Cambium crate, even where the licenses would be compatible. Sapling, which includes Mononoke, is GPL-2.0 at the repository level, and subprojects declare their own licenses (checked through the GitHub API on 2026-10-18). Studying published designs is fine, and the designs must be credited.
- Permissively licensed third-party code copied into the repository keeps its license header, and its attribution notices go into a `NOTICE` file.
- We chose AGPL-3.0-only over -or-later so that future license versions do not apply automatically.

## Consequences

- When the Cargo workspace is created, CI must check every crate's license, the internal dependency rule, and the third-party allow-list, for example with `cargo-deny`.
- AGPL-3.0 section 13 binds the project's own hosted service too. Private code for the hosted service, such as billing or a control plane, must run as a separate program that talks to the server through its API. Otherwise it must be published.
- With AGPL-3.0-only and no CLA, the server can never be relicensed, not even to a later AGPL version, without the consent of every contributor.
- External contributions are accepted under the [Developer Certificate of Origin 1.1](https://developercertificate.org/) (a sign-off on each commit), not a CLA. A contribution is licensed under the license of the crate or file it changes. A CI check enforces the sign-off.
- Contributors keep their copyright, and no CLA grants relicensing rights, so contributed server code cannot be relicensed into a proprietary edition. Every feature ships as open source, including enterprise features such as SSO and audit logs. The paid offering is managed hosting, support, and SLAs.
- The project's trademark, not its license, protects the hosted brand. The final product name will be registered before public launch. `cambium` is a code name only.
