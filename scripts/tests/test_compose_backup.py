"""Test the compose backup script's safety checks without Docker."""

import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "compose_backup", ROOT / "scripts" / "compose_backup.py",
)
assert SPEC is not None and SPEC.loader is not None
BACKUP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BACKUP)


class FakeDocker:
    """Records docker commands; `tar -cf -` writes the volume name as the archive."""

    def __init__(self, containers: str = "", nonempty: frozenset = frozenset()):
        self.calls: list[list[str]] = []
        self.containers = containers
        self.nonempty = nonempty

    def __call__(self, command, cwd=None, check=False, text=False, stdin=None, stdout=None):
        self.calls.append(command)
        args = command[2:]
        if args[:3] == ["ps", "--all", "--quiet"]:
            return subprocess.CompletedProcess(command, 0, stdout=self.containers)
        if args[:1] == ["run"]:
            service = args[args.index("--entrypoint") + 2]
            program = args[args.index("--entrypoint") + 1]
            if program == "tar" and "-cf" in args:
                stdout.write(f"archive of {service}".encode())
            if program == "python3" and service in self.nonempty:
                raise subprocess.CalledProcessError(1, command)
        return subprocess.CompletedProcess(command, 0, stdout="")

    def subcommands(self) -> list[str]:
        return [" ".join(call[2:4]) for call in self.calls]


class ComposeBackupTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.backup = Path(self.directory.name) / "backup"

    def take_backup(self) -> FakeDocker:
        docker = FakeDocker()
        with patch.object(BACKUP.subprocess, "run", docker), patch("builtins.print"):
            BACKUP.backup(self.backup, restart=True)
        return docker

    def test_backup_stops_archives_both_volumes_and_restarts(self) -> None:
        docker = self.take_backup()
        self.assertEqual(docker.subcommands()[0], "stop cambium")
        self.assertEqual(docker.subcommands()[-1], "up -d")
        manifest = json.loads((self.backup / "manifest.json").read_text())
        self.assertEqual(set(manifest["archives"]), {"metadata", "enroute-data"})
        self.assertEqual(
            (self.backup / "enroute-data.tar").read_bytes(), b"archive of enroute"
        )
        self.assertEqual((self.backup / "metadata.tar").stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.backup.stat().st_mode & 0o777, 0o700)

    def test_backup_refuses_a_nonempty_directory(self) -> None:
        self.backup.mkdir()
        (self.backup / "old").write_text("x")
        docker = FakeDocker()
        with patch.object(BACKUP.subprocess, "run", docker):
            with self.assertRaises(SystemExit):
                BACKUP.backup(self.backup, restart=True)
        self.assertEqual(docker.calls, [])

    def test_restore_refuses_an_existing_deployment_before_any_change(self) -> None:
        self.take_backup()
        docker = FakeDocker(containers="0123abcd\n")
        with patch.object(BACKUP.subprocess, "run", docker):
            with self.assertRaises(SystemExit):
                BACKUP.restore(self.backup)
        self.assertEqual(docker.subcommands(), ["ps --all"])

    def test_restore_refuses_nonempty_volumes_before_extracting(self) -> None:
        self.take_backup()
        docker = FakeDocker(nonempty=frozenset({"enroute"}))
        with patch.object(BACKUP.subprocess, "run", docker):
            with self.assertRaises(SystemExit):
                BACKUP.restore(self.backup)
        self.assertFalse(any("-xpf" in call for call in docker.calls))

    def test_restore_rejects_a_modified_archive(self) -> None:
        self.take_backup()
        (self.backup / "metadata.tar").write_bytes(b"tampered")
        docker = FakeDocker()
        with patch.object(BACKUP.subprocess, "run", docker):
            with self.assertRaises(SystemExit):
                BACKUP.restore(self.backup)
        self.assertEqual(docker.calls, [])

    def test_restore_extracts_both_archives_then_starts(self) -> None:
        self.take_backup()
        docker = FakeDocker()
        with patch.object(BACKUP.subprocess, "run", docker), patch("builtins.print"):
            BACKUP.restore(self.backup)
        extracts = [call for call in docker.calls if "-xpf" in call]
        self.assertEqual(len(extracts), 2)
        self.assertEqual(docker.subcommands()[-1], "up -d")


if __name__ == "__main__":
    unittest.main()
