"""Build and smoke-test a native self-host archive from a Cambium checkout."""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]
ENROUTE = ROOT / "target" / "enroute"
BINARIES = ("cambium", "cambium-cli", "enroute")
FILES = (
    "LICENSE-APACHE", "LICENSE-AGPL", "vendor/enroute/LICENSE",
    "vendor/enroute/NOTICE", "vendor/enroute/manifest.toml",
    "examples/enroute-local.toml",
)


def build(host: str) -> None:
    subprocess.run(
        [sys.executable, "scripts/prepare_enroute.py",
         *(["--verify"] if ENROUTE.exists() else [])],
        cwd=ROOT, check=True,
    )
    subprocess.run(
        ["cargo", "build", "--locked", "--release",
         "-p", "cambium-server", "-p", "cambium-cli", "--bins",
         "--target", host, "--target-dir", str(ROOT / "target")],
        cwd=ROOT, check=True,
    )
    # Enroute's own working directory selects its pinned Rust toolchain.
    subprocess.run(
        ["cargo", "build", "--locked", "--release", "--bin", "enroute",
         "--target", host, "--target-dir", str(ENROUTE / "target")],
        cwd=ENROUTE, check=True,
    )


def verify_bundle(directory: Path) -> None:
    directory = directory.resolve()
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())
    enroute = tomllib.loads((ROOT / "vendor/enroute/manifest.toml").read_text())
    for name in BINARIES:
        version = (enroute["version"] if name == "enroute"
                   else workspace["workspace"]["package"]["version"])
        actual = subprocess.check_output(
            [directory / "bin" / name, "--version"], cwd=directory, text=True,
        ).strip()
        if actual != f"{name} {version}":
            raise ValueError(f"Unexpected binary version for {name}: {actual}")
    for name in FILES:
        if (directory / name).read_bytes() != (ROOT / name).read_bytes():
            raise ValueError(f"Archive member differs from checkout: {name}")


def source_revision(checkout: Path) -> str:
    inputs = [
        "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo", "crates",
        "vendor/enroute", "scripts/package_self_host.py",
        "scripts/prepare_enroute.py", *FILES,
    ]
    if subprocess.check_output(
        ["git", "status", "--porcelain", "--untracked-files=all", "--", *inputs],
        cwd=checkout,
    ):
        raise ValueError("Cannot label modified packaging inputs with a commit")
    return subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=checkout, text=True,
    ).strip()


def package(output: Path) -> Path:
    host = subprocess.check_output(
        ["rustc", "--print", "host-tuple"], cwd=ROOT, text=True,
    ).strip()
    revision = source_revision(ROOT)
    build(host)
    name = f"cambium-{revision[:12]}-{host}"
    output.mkdir(parents=True, exist_ok=True)
    destination = output / f"{name}.tar.gz"
    # Build beside the destination so installing the verified archive is atomic.
    with tempfile.TemporaryDirectory(dir=output) as temporary:
        staging = Path(temporary)
        archive = staging / destination.name
        with tarfile.open(archive, "w:gz") as bundle:
            for binary in BINARIES:
                checkout = ENROUTE if binary == "enroute" else ROOT
                bundle.add(checkout / "target" / host / "release" / binary,
                           arcname=f"{name}/bin/{binary}")
            for path in FILES:
                bundle.add(ROOT / path, arcname=f"{name}/{path}")
        # Only this process's explicit list of files went into the archive.
        with tarfile.open(archive, "r:gz") as bundle:
            bundle.extractall(staging, filter="data")
        verify_bundle(staging / name)
        with archive.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        if source_revision(ROOT) != revision:
            raise ValueError("The checkout revision changed during packaging")
        os.replace(archive, destination)
    print(f"{digest}  {destination.name}")
    return destination


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output_directory", type=Path)
    args = parser.parse_args()
    package(args.output_directory)


if __name__ == "__main__":
    main()
