"""Catch incomplete native self-host bundles before they become artifacts."""

import shutil
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
            packaging.verify_bundle(root)
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
