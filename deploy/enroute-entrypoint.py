"""Load only the shared hook key before replacing this process with Enroute."""

import os
from pathlib import Path


key = Path("/run/cambium-hook/signing.pem")
os.environ["ENROUTE_HOOK_SIGNING_KEY"] = key.read_text()
os.execvp(
    "enroute", ["enroute", "--config", "file:///etc/enroute.toml", "serve"]
)
