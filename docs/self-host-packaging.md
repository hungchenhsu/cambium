# Self-host packaging proposal

The [packaging workflow](../.github/workflows/package.yml) builds candidate
native archives and both container images for pull requests that change a
packaging input. It runs on the PR's test merge commit with a read-only
repository token and checkout credential persistence disabled. The jobs use
local files and the runner's local Docker image store. There is no publish
job or tag trigger. The runners discard the candidate packages after the
checks finish.

## Native archives

From the checkout to package, use the [development prerequisites](development.md)
and Python 3.11.4 or later. Run:

```sh
python3 scripts/package_self_host.py target/packages
```

The script builds the locked `cambium` server and `cambium-cli` client in release
mode. It prepares or verifies the pinned, patched Enroute checkout and builds
it with its own toolchain. The archive name contains the Cambium revision and
Rust host triple. Its top-level directory includes:

- `bin/cambium`, `bin/cambium-cli` and `bin/enroute`.
- The two Cambium licenses, the Enroute license and NOTICE, and the backend
  source and patch pins in `vendor/enroute/manifest.toml`.
- `examples/enroute-local.toml`, the configuration for the signed backend.

The script extracts the archive, runs all three binaries with `--version`,
checks the bundled non-binary files against the checkout, then prints the
archive's SHA-256 digest. The Linux and macOS builds target their native
runner and libraries. A self-host still needs the Git version in the
[local deployment guide](local-development.md) and the runtime libraries for
that build. To run the guide from an extracted archive, use `bin/cambium`
and `bin/enroute` wherever it references build output.

## Container images

The [Dockerfile](../Dockerfile) has separate `cambium` and `enroute` runtime
targets. The workflow builds each target with a local image ID and runs its
binary with networking disabled. To build on a machine with Docker:

```sh
docker build --target cambium .
docker build --target enroute .
```

The [Compose quickstart](quickstart.md) covers the two images running together,
including the signing configuration and private network. Its end-to-end CI
remains a separate check.

## Before distribution

The owner still needs to select supported runtime platforms, review the
distribution notices and corresponding source, and decide how checksums,
signatures and a release are authorized. Any future publisher belongs in
a separate reviewed workflow operating on trusted revisions. A PR runner's
archive and image store are disposable build evidence. The Phase 1b release
checklist remains open.
