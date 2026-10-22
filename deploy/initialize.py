"""Initialize local compose without replacing existing deployment keys."""

from pathlib import Path
import os
import shutil
import subprocess
import tempfile


DATA = Path("/var/lib/cambium/state")
SHARED_KEY = Path("/run/cambium-hook/signing.pem")
CONFIG = Path("/etc/cambium.toml")
REQUIRED = (
    "responses.key", "hook-signing.pem", "hook-public.pem",
    "cambium.toml", "metadata.sqlite",
)


def sync_directory(path: Path) -> None:
    """Persist an atomic publication within its containing volume."""
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def initialize_state() -> None:
    """Publish complete state; retry an unpublished interrupted build."""
    if not DATA.exists() or (DATA.is_dir() and not any(DATA.iterdir())):
        if SHARED_KEY.exists():
            raise SystemExit(
                "State is missing; restore the matching deployment"
            )
        staging = DATA.with_name(f".{DATA.name}-initializing")
        if staging.exists():
            shutil.rmtree(staging)
        subprocess.run(
            ["cambium", "--data-dir", str(staging), "init"], check=True
        )
        with (staging / "cambium.toml").open("wb") as output:
            output.write(CONFIG.read_bytes())
            output.flush()
            os.fsync(output.fileno())
        sync_directory(staging)
        staging.rename(DATA)
        sync_directory(DATA.parent)
    if not all((DATA / name).is_file() for name in REQUIRED):
        raise SystemExit("State is incomplete; restore its original files")


def share_key(source: bytes) -> None:
    """Install a complete hook key atomically; retain every published key."""
    if SHARED_KEY.exists():
        if SHARED_KEY.read_bytes() != source:
            raise SystemExit("Hook keys differ; restore the matching key")
        return
    with tempfile.NamedTemporaryFile(
        prefix=".signing-", dir=SHARED_KEY.parent, delete=False,
    ) as output:
        pending = Path(output.name)
        try:
            output.write(source)
            output.flush()
            os.fsync(output.fileno())
            os.link(pending, SHARED_KEY)
            sync_directory(SHARED_KEY.parent)
        finally:
            pending.unlink()


def main() -> None:
    """Only the hook signing key is shared with the backend container."""
    os.umask(0o077)
    initialize_state()
    share_key((DATA / "hook-signing.pem").read_bytes())
    print("Local deployment initialized")


if __name__ == "__main__":
    main()
