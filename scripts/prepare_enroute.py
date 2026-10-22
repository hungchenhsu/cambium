#!/usr/bin/env python3
"""Prepare or verify the exact Enroute source and Cambium patch."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import subprocess
import tomllib


ROOT = Path(__file__).resolve().parents[1]
VENDOR = ROOT / "vendor" / "enroute"


def git(directory: Path, *args: str) -> bytes:
    """Run Git with stable diff output and without local hooks."""
    result = subprocess.run(
        [
            "git", "-c", "core.hooksPath=/dev/null", "-c", "core.quotePath=true",
            "-C", str(directory), *args,
        ],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return result.stdout


def verify(destination: Path, upstream_commit: str, patch_hash: str) -> None:
    """Reject source drift, including staged or untracked code."""
    head = git(destination, "rev-parse", "HEAD").decode().strip()
    if head != upstream_commit:
        raise ValueError("Enroute HEAD does not match the pinned upstream commit")
    patch = git(
        destination, "diff", "--cached", "--binary", "--no-color",
        "--no-ext-diff", "--src-prefix=a/", "--dst-prefix=b/", "HEAD",
    )
    if hashlib.sha256(patch).hexdigest() != patch_hash:
        raise ValueError("Enroute staged changes do not match the pinned patch")
    git(destination, "diff", "--exit-code", "--no-ext-diff")
    if git(destination, "ls-files", "--others", "--exclude-standard").strip():
        raise ValueError("Enroute source has unexpected untracked files")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--destination", type=Path, default=ROOT / "target" / "enroute",
    )
    parser.add_argument(
        "--verify", action="store_true", help="Verify an existing prepared checkout",
    )
    args = parser.parse_args()
    manifest = tomllib.loads((VENDOR / "manifest.toml").read_text())
    upstream_commit: str = manifest["upstream_commit"]
    patch_hash: str = manifest["patch_sha256"]
    patch_path = VENDOR / "durable-seal.patch"
    if hashlib.sha256(patch_path.read_bytes()).hexdigest() != patch_hash:
        raise ValueError("Cambium's Enroute patch checksum does not match")
    destination = args.destination.resolve()
    if not args.verify:
        if destination.exists():
            raise ValueError("Destination exists; use --verify or a new directory")
        destination.mkdir(parents=True)
        git(destination, "init", "--quiet")
        git(destination, "config", "core.autocrlf", "false")
        git(destination, "remote", "add", "origin", manifest["upstream_url"])
        git(destination, "fetch", "--depth", "1", "origin", upstream_commit)
        git(destination, "checkout", "--detach", "--quiet", upstream_commit)
        git(destination, "apply", "--index", str(patch_path))
    verify(destination, upstream_commit, patch_hash)
    print(f"Verified Enroute {manifest['version']}: {destination}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        if isinstance(error, subprocess.CalledProcessError):
            detail = error.stderr.decode(errors="replace").strip()
        else:
            detail = str(error)
        raise SystemExit(f"Enroute source preparation failed: {detail}") from error
