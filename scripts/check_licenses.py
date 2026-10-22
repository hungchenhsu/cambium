#!/usr/bin/env python3
"""Enforce ADR-0003 for all workspace crates and resolved dependencies."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tomllib
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
CARGO_DENY_VERSION = "0.20.2"
WORKSPACE_LICENSES = {"Apache-2.0", "AGPL-3.0-only"}


def structural_errors(metadata: dict[str, Any]) -> list[str]:
    """Check explicit licenses and transitive internal dependencies."""
    packages = {package["id"]: package for package in metadata["packages"]}
    members = set(metadata["workspace_members"])
    nodes = {
        node["id"]: node["dependencies"]
        for node in metadata["resolve"]["nodes"]
    }
    errors = []
    for member in sorted(members):
        package = packages[member]
        manifest = Path(package["manifest_path"])
        with manifest.open("rb") as source:
            declared = tomllib.load(source)["package"].get("license")
        if not isinstance(declared, str) or declared not in WORKSPACE_LICENSES:
            errors.append(
                f"{package['name']}: declare license explicitly as Apache-2.0 "
                "or AGPL-3.0-only in Cargo.toml"
            )
        if package["license"] != "Apache-2.0":
            continue
        pending = [(member, [package["name"]])]
        visited = set()
        while pending:
            current, chain = pending.pop()
            if current in visited:
                continue
            visited.add(current)
            if (
                current in members
                and packages[current]["license"] == "AGPL-3.0-only"
            ):
                errors.append(
                    "Apache-to-AGPL dependency: " + " -> ".join(chain)
                )
            for dependency in nodes[current]:
                pending.append(
                    (dependency, [*chain, packages[dependency]["name"]])
                )
    return errors


def check(manifest: Path, offline: bool) -> int:
    """Check all targets and features, including build and dev dependencies."""
    flags = ["--locked", "--all-features"]
    if offline:
        flags.append("--offline")
    result = subprocess.run(
        [
            "cargo", "metadata", "--format-version", "1",
            "--manifest-path", str(manifest), *flags,
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    metadata = json.loads(result.stdout)
    errors = structural_errors(metadata)
    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1

    version = subprocess.run(
        ["cargo", "deny", "--version"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if version != f"cargo-deny {CARGO_DENY_VERSION}":
        print(
            f"Expected cargo-deny {CARGO_DENY_VERSION}; got {version}",
            file=sys.stderr,
        )
        return 1

    checks = [(manifest, ROOT / "deny.toml", True)]
    members = set(metadata["workspace_members"])
    for package in metadata["packages"]:
        if package["id"] in members and package["license"] == "Apache-2.0":
            checks.append(
                (
                    Path(package["manifest_path"]),
                    ROOT / "deny-apache.toml",
                    False,
                )
            )
    for crate_manifest, config, workspace in checks:
        print(
            f"License check: {crate_manifest.parent.name} ({config.name})",
            flush=True,
        )
        command = [
            "cargo", "deny", "--manifest-path", str(crate_manifest),
            "--config", str(config), *flags,
        ]
        if workspace:
            command.append("--workspace")
        subprocess.run([*command, "check", "licenses"], check=True)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest-path", type=Path, default=ROOT / "Cargo.toml"
    )
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    try:
        return check(args.manifest_path.resolve(), args.offline)
    except subprocess.CalledProcessError as error:
        if error.stderr:
            print(error.stderr, file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError) as error:
        print(f"License check failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
