# ADR-0021: TLS for the CLI

- **Status:** Proposed
- **Date:** 2026-11-20

## Context

Which TLS transport and trust roots should an installed `cambium-cli` use for
HTTPS? [ADR-0019](0019-cli.md) leaves this open. The CLI is Apache-2.0 and
must satisfy the client dependency policy in [ADR-0003](0003-licensing.md).
The HTTP loopback default, token sources, exit statuses and machine response
bytes remain part of the CLI contract. Self-hosts may use a private CA.

## Options

All three trials kept reqwest 0.12.28, JSON and disabled default features. Each
selected one additional feature on the CLI dependency. The versioned
[reqwest feature graph](https://docs.rs/crate/reqwest/0.12.28/features)
identifies the provider and root source selected by each feature.

| Feature | Trust and deployment | License checker |
| --- | --- | --- |
| `native-tls` | Schannel on Windows, Secure Transport on macOS, OpenSSL on other supported native platforms. Linux packaging must account for its system OpenSSL dependency. [Provider documentation](https://docs.rs/native-tls/0.2.18/native_tls/) | Exit 0 |
| `rustls-tls-native-roots` | rustls and ring verify TLS certificates using roots loaded from the host. The CLI can use an administrator-provided CA without shipping that CA in its binary. This imports trust anchors and does not delegate all verification policy to the OS. [Root loader and platform support](https://github.com/rustls/rustls-native-certs) | Exit 0 |
| `rustls-tls-webpki-roots` | rustls and ring with Mozilla roots compiled into the binary. Trust updates require a new build. [Bundle documentation](https://docs.rs/webpki-roots/1.0.11/webpki_roots/) | Exit 1 |

### License evidence

On 2026-11-20, each trial started with Cambium `b19627bab4b1` and its original
`Cargo.lock` on a local scratch branch. In `crates/cambium-cli/Cargo.toml`,
replace `reqwest.workspace = true` with
`reqwest = { workspace = true, features = ["FEATURE"] }`, substituting one
row's feature. Resolve the candidate lock, inspect its CLI graph, then run
the same checker as CI:

```sh
cargo metadata --format-version 1 --all-features > /dev/null
cargo tree --locked -p cambium-cli --target all -e features
python3 scripts/check_licenses.py
```

The following excerpts are from the checker with cargo-deny 0.20.2 and Rust
1.99.1. Both passing runs reported `licenses ok` for the workspace and all
four Apache roots. The failing run stopped at the workspace check.

```text
native-tls (native-tls 0.2.18, openssl 0.10.82, openssl-sys 0.9.118)
License check: cambium-cli (deny-apache.toml)
licenses ok

rustls-tls-native-roots (rustls 0.23.46, ring 0.17.14, rustls-native-certs 0.8.5)
License check: cambium-cli (deny-apache.toml)
licenses ok

rustls-tls-webpki-roots (rustls 0.23.46, ring 0.17.14, webpki-roots 1.0.11)
License check: cambium (deny.toml)
error[rejected]: failed to satisfy license requirements
license = "CDLA-Permissive-2.0"
rejected: license is not explicitly allowed
licenses FAILED
```

The checks used the existing allow-lists and included build and development
dependencies. They assess the Cargo graphs. System libraries supplied by the
operating system are outside this check.

## Decision

Recommend `rustls-tls-native-roots` on the CLI dependency only. It supports
host-managed roots, passes the current policy, and avoids introducing a system
OpenSSL linkage requirement for Linux clients. Keep certificate-chain and
hostname verification enabled. A request to an HTTPS endpoint that fails
validation must return the transport-failure exit status. The implementation
should carry forward the existing HTTP default and CLI output contract.

## Consequences

The scratch CLI was built and run on macOS arm64. A temporary CA supplied
via `SSL_CERT_FILE` signed the loopback server's certificate. The valid hostname
returned the expected JSON bytes plus a newline and exit 0. An unrelated CA
and a mismatched hostname each returned exit 3 with empty stdout. No trust
settings on the host were modified.

The probe exercised an explicit CA file. Platform store loading on Windows,
macOS and Linux and compatibility with enterprise trust policies still need
implementation tests. The root loader's maintainers also document differences
between importing roots and delegating verification to the platform. A need
for native revocation or OS-specific constraints would warrant revisiting the
choice. This proposal leaves the implementation and the TLS debt entry open.
