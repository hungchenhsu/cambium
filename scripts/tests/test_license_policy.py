"""Exercise the real Cargo graph and cargo-deny with isolated local crates."""

from __future__ import annotations

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class LicensePolicyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(
            prefix="cambium-licenses-"
        )
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["crates/*"]\n'
            'exclude = ["external"]\nresolver = "3"\n',
            encoding="utf-8",
        )

    def crate(
        self,
        name: str,
        license_name: str | None,
        dependencies: str = "",
        external: bool = False,
    ) -> None:
        directory = self.root / ("external" if external else "crates") / name
        directory.mkdir(parents=True)
        manifest = (
            f'[package]\nname = "{name}"\nversion = "0.1.0"\n'
            'edition = "2024"\npublish = false\n'
        )
        if license_name is not None:
            manifest += f'license = "{license_name}"\n'
        manifest += dependencies
        if external:
            manifest += "\n[workspace]\n"
        (directory / "Cargo.toml").write_text(manifest, encoding="utf-8")
        (directory / "src").mkdir()
        (directory / "src/lib.rs").write_text("", encoding="utf-8")

    def run_policy(self, success: bool, message: str = "") -> None:
        subprocess.run(
            [
                "cargo", "generate-lockfile", "--offline",
                "--manifest-path", str(self.root / "Cargo.toml"),
            ],
            cwd=ROOT,
            check=True,
            capture_output=True,
        )
        result = subprocess.run(
            [
                sys.executable, str(ROOT / "scripts/check_licenses.py"),
                "--manifest-path", str(self.root / "Cargo.toml"), "--offline",
            ],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode == 0, success, output)
        self.assertIn(message, output)

    def test_valid_server_depends_on_apache_library(self) -> None:
        self.crate("protocol", "Apache-2.0")
        self.crate("server", "AGPL-3.0-only",
                   '\n[dependencies]\nprotocol = { path = "../protocol" }\n')
        self.run_policy(True)

    def test_missing_license_is_rejected_even_when_unpublished(self) -> None:
        self.crate("protocol", None)
        self.run_policy(False, "declare license explicitly")

    def test_wrong_workspace_license_is_rejected(self) -> None:
        self.crate("protocol", "MIT")
        self.run_policy(False, "declare license explicitly")

    def test_direct_internal_agpl_dependency_is_rejected(self) -> None:
        self.crate("server", "AGPL-3.0-only")
        self.crate("client", "Apache-2.0",
                   '\n[dependencies]\nserver = { path = "../server" }\n')
        self.run_policy(False, "client -> server")

    def test_transitive_internal_agpl_dependency_is_rejected(self) -> None:
        self.crate("server", "AGPL-3.0-only")
        self.crate("bridge", "Apache-2.0",
                   '\n[dependencies]\nserver = { path = "../server" }\n')
        self.crate("client", "Apache-2.0",
                   '\n[dependencies]\nbridge = { path = "../bridge" }\n')
        self.run_policy(False, "client -> bridge -> server")

    def test_optional_and_other_target_dependency_is_checked(self) -> None:
        self.crate("server", "AGPL-3.0-only")
        self.crate(
            "client", "Apache-2.0",
            '\n[target.\'cfg(target_os = "windows")\'.dependencies]\n'
            'server = { path = "../server", optional = true }\n',
        )
        self.run_policy(False, "client -> server")

    def test_build_and_dev_dependencies_are_checked(self) -> None:
        for kind in ("build", "dev"):
            with self.subTest(kind=kind):
                self.crate("server-" + kind, "AGPL-3.0-only")
                self.crate(
                    "client-" + kind, "Apache-2.0",
                    f'\n[{kind}-dependencies]\n'
                    f'server-{kind} = {{ path = "../server-{kind}" }}\n',
                )
                self.run_policy(False, f"client-{kind} -> server-{kind}")

    def test_unapproved_third_party_license_is_rejected(self) -> None:
        self.crate("dependency", "GPL-2.0-only", external=True)
        self.crate(
            "server", "AGPL-3.0-only",
            '\n[dependencies]\n'
            'dependency = { path = "../../external/dependency" }\n',
        )
        self.run_policy(False, "error[rejected]")

    def test_apache_graph_rejects_server_only_external_license(self) -> None:
        self.crate("dependency", "GPL-3.0-only", external=True)
        self.crate(
            "client", "Apache-2.0",
            '\n[dev-dependencies]\n'
            'dependency = { path = "../../external/dependency" }\n',
        )
        self.run_policy(False, "error[rejected]")

    def test_server_accepts_compatible_third_party_license(self) -> None:
        self.crate("dependency", "GPL-3.0-only", external=True)
        self.crate(
            "server", "AGPL-3.0-only",
            '\n[dependencies]\n'
            'dependency = { path = "../../external/dependency" }\n',
        )
        self.run_policy(True)

    def test_dual_license_permissive_alternative_is_accepted(self) -> None:
        self.crate("dependency", "MIT OR GPL-3.0-only", external=True)
        self.crate(
            "client", "Apache-2.0",
            '\n[dependencies]\n'
            'dependency = { path = "../../external/dependency" }\n',
        )
        self.run_policy(True)


if __name__ == "__main__":
    unittest.main()
