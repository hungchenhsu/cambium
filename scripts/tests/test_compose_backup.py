"""Test the compose backup script's safety checks without Docker."""

import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tarfile
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

COMPLETE = {
    "cambium": {
        "state/metadata.sqlite": b"metadata",
        "state/responses.key": b"key",
        "state/hook-signing.pem": b"private",
        "state/hook-public.pem": b"public",
        "state/cambium.toml": b"config",
    },
    "enroute": {"node/state": b"node", "objects/layer": b"layer"},
}


def archive(files: dict[str, bytes]) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as output:
        for name, data in files.items():
            info = tarfile.TarInfo(f"./{name}")
            info.size = len(data)
            output.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


class FakeDocker:
    """A compose project with per-service volume contents.

    `tar -cf -` streams a real archive of the service's volume; `tar -xpf -`
    records which bytes were extracted into which service and mount point.
    """

    def __init__(self, volumes=None, containers="abc123\n", nonempty=(), probe_failure=None):
        self.volumes = COMPLETE if volumes is None else volumes
        self.containers = containers
        self.nonempty = set(nonempty)
        self.probe_failure = probe_failure
        self.calls: list[list[str]] = []
        self.events: list[tuple] = []

    def __call__(self, command, cwd=None, check=False, text=False, stdin=None, stdout=None):
        self.calls.append(command)
        args = command[2:]
        if args[:3] == ["ps", "--all", "--quiet"]:
            return subprocess.CompletedProcess(command, 0, stdout=self.containers)
        if args[:1] == ["run"]:
            index = args.index("--entrypoint")
            program, service, rest = args[index + 1], args[index + 2], args[index + 3:]
            if program == "tar" and "-cf" in rest:
                stdout.write(archive(self.volumes[service]))
                self.events.append(("archive", service, rest[1]))
            elif program == "tar" and "-xpf" in rest:
                self.events.append(("extract", service, rest[1], stdin.read()))
            elif program == "python3" and "listdir" in rest[1]:
                if self.probe_failure is not None:
                    raise subprocess.CalledProcessError(self.probe_failure, command)
                if service in self.nonempty:
                    raise subprocess.CalledProcessError(3, command)
            elif program == "python3":
                kind = "remove" if "os.remove" in rest[1] else "create"
                self.events.append((f"sentinel-{kind}", service))
            return subprocess.CompletedProcess(command, 0)
        self.events.append(("compose", *args))
        return subprocess.CompletedProcess(command, 0, stdout="")


class ComposeBackupTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.backup = Path(self.directory.name) / "backup"

    def run_backup(self, docker: FakeDocker, restart: bool = True) -> None:
        with patch.object(BACKUP.subprocess, "run", docker), patch("builtins.print"):
            BACKUP.backup(self.backup, restart=restart)

    def run_restore(self, docker: FakeDocker) -> None:
        with patch.object(BACKUP.subprocess, "run", docker), patch("builtins.print"):
            BACKUP.restore(self.backup)

    def test_backup_stops_both_services_archives_each_volume_and_restarts(self) -> None:
        docker = FakeDocker()
        self.run_backup(docker)
        compose = [event for event in docker.events if event[0] == "compose"]
        self.assertEqual(compose[0], ("compose", "stop", "cambium", "enroute"))
        self.assertEqual(compose[-1], ("compose", "up", "-d", "--wait"))
        self.assertEqual(
            [event for event in docker.events if event[0] == "archive"],
            [("archive", "cambium", "/var/lib/cambium"), ("archive", "enroute", "/var/lib/enroute")],
        )
        self.assertEqual((self.backup / "metadata.tar").read_bytes(), archive(COMPLETE["cambium"]))
        self.assertEqual((self.backup / "enroute-data.tar").read_bytes(), archive(COMPLETE["enroute"]))
        manifest = json.loads((self.backup / "manifest.json").read_text())
        self.assertEqual(set(manifest["archives"]), {"metadata", "enroute-data"})
        for path in self.backup.iterdir():
            self.assertEqual(path.stat().st_mode & 0o777, 0o600, path)
        self.assertEqual(self.backup.stat().st_mode & 0o777, 0o700)

    def test_no_restart_leaves_the_services_stopped(self) -> None:
        docker = FakeDocker()
        self.run_backup(docker, restart=False)
        self.assertNotIn(("compose", "up", "-d", "--wait"), docker.events)

    def test_backup_refuses_a_missing_deployment_before_stopping_anything(self) -> None:
        docker = FakeDocker(containers="")
        with self.assertRaises(SystemExit):
            self.run_backup(docker)
        self.assertEqual(docker.events, [])

    def test_backup_of_empty_volumes_fails_and_still_restarts(self) -> None:
        docker = FakeDocker(volumes={"cambium": {}, "enroute": {}})
        with self.assertRaises(SystemExit):
            self.run_backup(docker)
        self.assertEqual(docker.events[-1], ("compose", "up", "-d", "--wait"))
        self.assertFalse((self.backup / "manifest.json").exists())

    def test_backup_refuses_a_nonempty_directory(self) -> None:
        self.backup.mkdir()
        (self.backup / "old").write_text("x")
        docker = FakeDocker()
        with self.assertRaises(SystemExit):
            self.run_backup(docker)
        self.assertFalse(any(event[0] == "compose" for event in docker.events))

    def test_restore_puts_each_archive_into_its_own_volume_behind_a_sentinel(self) -> None:
        self.run_backup(FakeDocker())
        docker = FakeDocker(containers="")
        self.run_restore(docker)
        steps = [event[:3] if event[0] == "extract" else event for event in docker.events]
        self.assertEqual(
            steps,
            [
                ("compose", "create"),
                ("sentinel-create", "cambium"),
                ("extract", "cambium", "/var/lib/cambium"),
                ("extract", "enroute", "/var/lib/enroute"),
                ("sentinel-remove", "cambium"),
                ("compose", "up", "-d", "--wait"),
            ],
        )
        payloads = {event[1]: event[3] for event in docker.events if event[0] == "extract"}
        self.assertEqual(payloads["cambium"], archive(COMPLETE["cambium"]))
        self.assertEqual(payloads["enroute"], archive(COMPLETE["enroute"]))

    def test_restore_refuses_an_existing_deployment_before_any_change(self) -> None:
        self.run_backup(FakeDocker())
        docker = FakeDocker(containers="0123abcd\n")
        with self.assertRaises(SystemExit):
            self.run_restore(docker)
        self.assertEqual(docker.events, [])

    def test_restore_refuses_nonempty_volumes_before_extracting(self) -> None:
        self.run_backup(FakeDocker())
        docker = FakeDocker(containers="", nonempty={"enroute"})
        with self.assertRaises(SystemExit):
            self.run_restore(docker)
        self.assertFalse(any(event[0] in ("extract", "sentinel-create") for event in docker.events))

    def test_a_failed_emptiness_probe_is_not_reported_as_a_nonempty_volume(self) -> None:
        self.run_backup(FakeDocker())
        docker = FakeDocker(containers="", probe_failure=125)
        with self.assertRaises(subprocess.CalledProcessError):
            self.run_restore(docker)

    def test_restore_rejects_modified_incomplete_or_unknown_backups(self) -> None:
        for damage in ("tampered", "incomplete", "format"):
            with self.subTest(damage):
                self.setUp()
                self.run_backup(FakeDocker())
                manifest_path = self.backup / "manifest.json"
                manifest = json.loads(manifest_path.read_text())
                if damage == "tampered":
                    (self.backup / "metadata.tar").write_bytes(b"tampered")
                elif damage == "incomplete":
                    partial = archive({"state/metadata.sqlite": b"metadata"})
                    (self.backup / "metadata.tar").write_bytes(partial)
                    manifest["archives"]["metadata"] = BACKUP.sha256(self.backup / "metadata.tar")
                else:
                    manifest["format"] = 2
                manifest_path.write_text(json.dumps(manifest))
                docker = FakeDocker(containers="")
                with self.assertRaises(SystemExit):
                    self.run_restore(docker)
                self.assertEqual(docker.calls, [])


if __name__ == "__main__":
    unittest.main()
