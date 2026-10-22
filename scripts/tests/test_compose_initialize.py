"""Test interrupted compose initialization without changing live volumes."""

import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "compose_initialize", ROOT / "deploy" / "initialize.py",
)
assert SPEC is not None and SPEC.loader is not None
INITIALIZE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INITIALIZE)


class ComposeInitializationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        root = Path(self.directory.name)
        self.data = root / "state"
        self.shared = root / "signing.pem"
        self.config = root / "compose.toml"
        self.config.write_bytes(b"compose configuration")
        for name, value in (
            ("DATA", self.data), ("SHARED_KEY", self.shared),
            ("CONFIG", self.config),
        ):
            current = patch.object(INITIALIZE, name, value)
            current.start()
            self.addCleanup(current.stop)

    @staticmethod
    def create_state(
        command: list[str], *, check: bool,
    ) -> subprocess.CompletedProcess[str]:
        assert check
        state = Path(command[2])
        state.mkdir(mode=0o700)
        for name in INITIALIZE.REQUIRED:
            (state / name).write_bytes(f"original {name}".encode())
        return subprocess.CompletedProcess(command, 0)

    def test_empty_directory_and_interrupted_stage_can_retry(self) -> None:
        self.data.mkdir()

        def interrupted(command: list[str], *, check: bool) -> None:
            assert check
            state = Path(command[2])
            state.mkdir()
            (state / "responses.key").write_bytes(b"unpublished key")
            raise subprocess.CalledProcessError(1, command)

        with patch.object(INITIALIZE.subprocess, "run", interrupted):
            with self.assertRaises(subprocess.CalledProcessError):
                INITIALIZE.main()
        self.assertEqual(list(self.data.iterdir()), [])
        self.assertFalse(self.shared.exists())
        with patch.object(
            INITIALIZE.subprocess, "run", self.create_state,
        ):
            INITIALIZE.main()
        self.assertEqual(
            (self.data / "cambium.toml").read_bytes(),
            self.config.read_bytes(),
        )
        self.assertEqual(
            self.shared.read_bytes(),
            (self.data / "hook-signing.pem").read_bytes(),
        )
        self.assertFalse(self.data.with_name(".state-initializing").exists())

    def test_repeat_preserves_published_keys_and_configuration(self) -> None:
        with patch.object(INITIALIZE.subprocess, "run", self.create_state):
            INITIALIZE.main()
        before = {p.name: p.read_bytes() for p in self.data.iterdir()}
        self.config.write_bytes(b"changed image configuration")
        with patch.object(INITIALIZE.subprocess, "run") as initialize:
            INITIALIZE.main()
            initialize.assert_not_called()
        self.assertEqual(
            {p.name: p.read_bytes() for p in self.data.iterdir()}, before,
        )

    def test_interrupted_key_publication_reuses_existing_state(self) -> None:
        with patch.object(INITIALIZE.subprocess, "run", self.create_state):
            with patch.object(INITIALIZE.os, "link", side_effect=OSError):
                with self.assertRaises(OSError):
                    INITIALIZE.main()
        before = (self.data / "hook-signing.pem").read_bytes()
        self.assertFalse(self.shared.exists())
        with patch.object(INITIALIZE.subprocess, "run") as initialize:
            INITIALIZE.main()
            initialize.assert_not_called()
        self.assertEqual(self.shared.read_bytes(), before)

    def test_incomplete_published_state_is_never_reset(self) -> None:
        self.data.mkdir()
        key = self.data / "responses.key"
        key.write_bytes(b"existing response encryption key")
        with patch.object(INITIALIZE.subprocess, "run") as initialize:
            with self.assertRaises(SystemExit):
                INITIALIZE.main()
            initialize.assert_not_called()
        self.assertEqual(key.read_bytes(), b"existing response encryption key")
        self.assertFalse(self.shared.exists())


if __name__ == "__main__":
    unittest.main()
