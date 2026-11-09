"""Back up or restore the local compose deployment's state volumes.

Cambium's metadata and Enroute's storage must be backed up together, because
the metadata store maps names to backend repository IDs (ADR-0010). This script
takes a cold backup: it stops both services, archives each volume through the
deployment's own images and mount points, and starts the services again.

  python3 scripts/compose_backup.py backup  BACKUP_DIR
  python3 scripts/compose_backup.py restore BACKUP_DIR

`restore` writes only into empty volumes. To restore over an existing
deployment, first remove it deliberately with `docker compose down --volumes`.
If a restore fails part way, initialization refuses to start the partial state;
remove it with `docker compose down --volumes` and restore again.
The hook-key volume is not archived: initialization republishes it from the
restored metadata. See docs/backup.md.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = "manifest.json"
FORMAT = 1
# Archive name, the compose service whose image and mounts are used, the
# volume's mount point in that service, and entries a complete archive holds.
VOLUMES = (
    (
        "metadata", "cambium", "/var/lib/cambium",
        (
            "state/metadata.sqlite", "state/responses.key",
            "state/hook-signing.pem", "state/hook-public.pem",
            "state/cambium.toml",
        ),
    ),
    ("enroute-data", "enroute", "/var/lib/enroute", ("node",)),
)
# Written into the metadata volume before extraction and removed only after
# every archive is in place. deploy/initialize.py refuses to start while it
# exists, so a failed restore cannot be started by accident.
INCOMPLETE = "/var/lib/cambium/.restore-incomplete"


def compose(*args: str, stdin=None, stdout=None) -> None:
    subprocess.run(
        ["docker", "compose", *args], cwd=ROOT, check=True, stdin=stdin, stdout=stdout
    )


def containers(*services: str) -> str:
    return subprocess.run(
        ["docker", "compose", "ps", "--all", "--quiet", *services],
        cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE,
    ).stdout.strip()


def in_volume(service: str, *command: str, stdin=None, stdout=None) -> None:
    """Run a command in a one-off container with the service's image and volumes.

    Archives stream over stdin and stdout, so the host directory needs no
    permissions for the containers' unprivileged user.
    """
    compose(
        "run", "--rm", "--no-deps", "-T", "--entrypoint", command[0],
        service, *command[1:], stdin=stdin, stdout=stdout,
    )


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def check_archive(path: Path, required: tuple[str, ...]) -> None:
    """Refuse an archive that does not hold a complete deployment's state."""
    try:
        with tarfile.open(path) as archive:
            names = {name.removeprefix("./").rstrip("/") for name in archive.getnames()}
    except tarfile.TarError:
        raise SystemExit(f"{path.name} is not a readable tar archive") from None
    missing = [
        name for name in required
        if name not in names and not any(entry.startswith(f"{name}/") for entry in names)
    ]
    if missing:
        raise SystemExit(f"{path.name} lacks {', '.join(missing)}; it is not a complete backup")


def backup(directory: Path, restart: bool) -> None:
    # Without both services' containers, `run` would back up fresh empty volumes.
    for service in ("cambium", "enroute"):
        if not containers(service):
            raise SystemExit(
                f"No {service} container exists in this compose project; nothing to back up"
            )
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    if any(directory.iterdir()):
        raise SystemExit(f"{directory} is not empty; choose a new directory")
    directory.chmod(0o700)
    compose("stop", "cambium", "enroute")
    try:
        archives = {}
        for name, service, mount, required in VOLUMES:
            path = directory / f"{name}.tar"
            with path.open("xb") as output:
                path.chmod(0o600)
                in_volume(service, "tar", "-C", mount, "-cf", "-", ".", stdout=output)
                output.flush()
                os.fsync(output.fileno())
            check_archive(path, required)
            archives[name] = sha256(path)
        manifest = {"format": FORMAT, "archives": archives}
        with (directory / MANIFEST).open("x") as output:
            os.chmod(output.fileno(), 0o600)
            output.write(json.dumps(manifest, sort_keys=True) + "\n")
            output.flush()
            os.fsync(output.fileno())
    finally:
        # A failed backup must not leave the deployment stopped.
        if restart:
            compose("up", "-d", "--wait")
    print(json.dumps({"backup": str(directory), **manifest}, sort_keys=True))


def restore(directory: Path) -> None:
    manifest = json.loads((directory / MANIFEST).read_text())
    if manifest.get("format") != FORMAT:
        raise SystemExit("Unknown backup format")
    if set(manifest["archives"]) != {name for name, *_ in VOLUMES}:
        raise SystemExit("The backup does not contain every state volume")
    for name, *_, required in VOLUMES:
        path = directory / f"{name}.tar"
        if sha256(path) != manifest["archives"][name]:
            raise SystemExit(f"{name}.tar does not match the manifest")
        check_archive(path, required)
    # Refuse before touching anything while any container of the project exists.
    if containers():
        raise SystemExit(
            "The deployment still exists; remove it with `docker compose down --volumes` first"
        )
    compose("create")
    for name, service, mount, _ in VOLUMES:
        # Refuse to mix a backup into existing state.
        try:
            in_volume(
                service, "python3", "-c",
                f"import os, sys; sys.exit(3 if os.listdir({mount!r}) else 0)",
            )
        except subprocess.CalledProcessError as error:
            if error.returncode == 3:
                raise SystemExit(
                    f"The {name} volume is not empty; remove the deployment first"
                ) from None
            raise
    in_volume("cambium", "python3", "-c", f"open({INCOMPLETE!r}, 'x').close()")
    for name, service, mount, _ in VOLUMES:
        with (directory / f"{name}.tar").open("rb") as source:
            in_volume(service, "tar", "-C", mount, "-xpf", "-", stdin=source)
    in_volume("cambium", "python3", "-c", f"import os; os.remove({INCOMPLETE!r})")
    compose("up", "-d", "--wait")
    print(json.dumps({"restored": str(directory)}, sort_keys=True))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("backup", help="stop, archive, and restart")
    create.add_argument("directory", type=Path)
    create.add_argument(
        "--no-restart", action="store_true", help="leave the services stopped"
    )
    load = commands.add_parser("restore", help="restore into empty volumes and start")
    load.add_argument("directory", type=Path)
    args = parser.parse_args()
    directory = args.directory.resolve()
    if args.command == "backup":
        backup(directory, restart=not args.no_restart)
    else:
        restore(directory)


if __name__ == "__main__":
    main()
