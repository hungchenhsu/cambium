"""Catch incomplete native self-host bundles before they become artifacts."""

import os
import shutil
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path

from scripts import package_self_host as packaging


class BundleTests(unittest.TestCase):
    def test_rejects_a_missing_notice_or_wrong_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "bin").mkdir()
            workspace = tomllib.loads((packaging.ROOT / "Cargo.toml").read_text())
            backend = tomllib.loads(
                (packaging.ROOT / "vendor/enroute/manifest.toml").read_text()
            )
            for name in packaging.BINARIES:
                executable = root / "bin" / name
                version = (backend["version"] if name == "enroute"
                           else workspace["workspace"]["package"]["version"])
                executable.write_text(f"#!/bin/sh\necho '{name} {version}'\n")
                executable.chmod(0o755)
            for name in packaging.FILES:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(packaging.ROOT / name, path)
            packaging.verify_bundle(Path(os.path.relpath(root)))
            notice = root / "vendor/enroute/NOTICE"
            notice.write_bytes(b"unexpected notice")
            with self.assertRaisesRegex(ValueError, "NOTICE"):
                packaging.verify_bundle(root)
            notice.unlink()
            with self.assertRaises(FileNotFoundError):
                packaging.verify_bundle(root)
            shutil.copyfile(packaging.ROOT / "vendor/enroute/NOTICE", notice)
            (root / "bin/enroute").write_text("#!/bin/sh\necho wrong\n")
            with self.assertRaisesRegex(ValueError, "version for enroute"):
                packaging.verify_bundle(root)
            (root / "bin/enroute").chmod(0o644)
            with self.assertRaises(PermissionError):
                packaging.verify_bundle(root)

    def test_modified_input_cannot_reuse_a_clean_revision(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            checkout = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=checkout, check=True)
            config = checkout / "examples/enroute-local.toml"
            config.parent.mkdir()
            config.write_text("original")
            subprocess.run(["git", "add", "examples/enroute-local.toml"],
                           cwd=checkout, check=True)
            subprocess.run(
                ["git", "-c", "user.name=Fixture", "-c",
                 "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false",
                 "commit", "-qm", "fixture"], cwd=checkout, check=True,
            )
            self.assertEqual(len(packaging.source_revision(checkout)), 40)
            config.write_text("modified")
            with self.assertRaisesRegex(ValueError, "modified packaging inputs"):
                packaging.source_revision(checkout)
