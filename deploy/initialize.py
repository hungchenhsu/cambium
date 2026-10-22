"""Initialize local compose without replacing existing keys."""

from pathlib import Path
import os
import subprocess


DATA = Path("/var/lib/cambium/state")
SHARED_KEY = Path("/run/cambium-hook/signing.pem")


def main() -> None:
    """Only the hook signing key is shared with the backend container."""
    os.umask(0o077)
    if not DATA.exists():
        subprocess.run(
            ["cambium", "--data-dir", str(DATA), "init"], check=True
        )
        config = Path("/etc/cambium.toml").read_bytes()
        (DATA / "cambium.toml").write_bytes(config)
    source = (DATA / "hook-signing.pem").read_bytes()
    if SHARED_KEY.exists():
        if SHARED_KEY.read_bytes() != source:
            raise SystemExit("Hook keys differ; restore the matching key")
    else:
        with SHARED_KEY.open("xb") as output:
            output.write(source)
    print("Local deployment initialized")


if __name__ == "__main__":
    main()
