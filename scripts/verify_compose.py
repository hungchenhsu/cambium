"""Verify a running local compose demo after examples/agent-loop.py.

Save that example's JSON stdout, then run:
  python3 scripts/verify_compose.py --credential-file .cambium-bootstrap.json \
      --result-file /tmp/cambium-demo-result.json
This creates a second disposable tenant for isolation checks. It retains
repositories and volumes. Docker must use the same compose project as the demo.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import time
from typing import Any
import urllib.error
import urllib.request
from uuid import uuid4


ROOT = Path(__file__).resolve().parents[1]


def docker(*args: str) -> str:
    """Capture local Docker output without displaying credential responses."""
    return subprocess.check_output(
        ["docker", *args], cwd=ROOT, text=True,
    ).strip()


def request(
    url: str, token: str | None = None, body: dict[str, Any] | None = None,
) -> tuple[int, dict[str, Any]]:
    """Return both HTTP status and JSON, including structured refusals."""
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    payload = json.dumps(body).encode() if body is not None else None
    query = urllib.request.Request(url, payload, headers)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        response = opener.open(query, timeout=5)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        contents = response.read()
        value = json.loads(contents) if contents.startswith(b"{") else {}
        return response.status, value


def inspect(service: str) -> dict[str, Any]:
    """Read the running container rather than only its compose declaration."""
    container = docker("compose", "ps", "-q", service)
    if not container:
        raise RuntimeError(f"{service} is not running")
    return json.loads(docker("inspect", container))[0]


def main(args: argparse.Namespace) -> None:
    """Check actual routes, mounts, network separation, and reclaimed files."""
    version = docker("version", "--format", "{{.Server.Version}}")
    assert int(version.split(".")[0]) >= 28, version
    credential = json.loads(args.credential_file.read_text())
    result = json.loads(args.result_file.read_text())
    api = "http://127.0.0.1:8080"
    backend = inspect("enroute")
    service = inspect("cambium")
    for container, expected in [(backend, "8083/tcp"), (service, "8080/tcp")]:
        ports = container["NetworkSettings"]["Ports"]
        published = {port: values for port, values in ports.items() if values}
        assert set(published) == {expected}, published
        assert all(
            value["HostIp"] == "127.0.0.1"
            for value in published[expected]
        )
        assert container["Config"]["User"] == "10001:10001"
    mounts = {item["Destination"]: item for item in backend["Mounts"]}
    assert "/var/lib/cambium" not in mounts
    assert not mounts["/run/cambium-hook"]["RW"]
    assert docker(
        "compose", "exec", "-T", "enroute", "python3", "-c",
        "from pathlib import Path; "
        "assert not Path('/var/lib/cambium/state/metadata.sqlite').exists(); "
        "assert not Path('/var/lib/cambium/state/responses.key').exists()",
    ) == ""
    networks = backend["NetworkSettings"]["Networks"]
    assert len(networks) == 1
    network_name, attachment = next(iter(networks.items()))
    network = json.loads(docker("network", "inspect", network_name))[0]
    assert network["Driver"] == "bridge"
    assert network_name != "bridge"
    probe = (
        "import socket; "
        "s = socket.socket(); s.settimeout(2); "
        f"assert s.connect_ex(({attachment['IPAddress']!r}, 8082)) != 0"
    )
    docker(
        "run", "--rm", "--network", "bridge", "--entrypoint", "python3",
        service["Config"]["Image"], "-c", probe,
    )
    for path in ["/mcp", "/v1/operations/list_repositories"]:
        assert request(api + path)[0] == 401
    git_info = result["git_url"] + "/info/refs?service=git-upload-pack"
    assert request(git_info)[0] == 401
    other = json.loads(docker(
        "compose", "exec", "-T", "cambium", "cambium",
        "--data-dir", "/var/lib/cambium/state", "--json", "admin", "bootstrap",
        "--tenant-slug", f"isolation-{uuid4().hex[:10]}",
        "--human-name", "Isolation check",
    ))
    query = {"repository_id": result["repository_id"]}
    status, value = request(
        api + "/v1/operations/get_repository", other["token"], query,
    )
    assert status == 404 and value["error"]["code"] == "not_found"
    status, value = request(api + "/mcp", other["token"], {
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "get_repository", "arguments": query},
    })
    assert status == 200
    assert value["result"]["structuredContent"]["error"]["code"] == "not_found"
    assert request(
        git_info, other["token"],
    )[0] == 404
    status, _ = request(
        api + "/v1/operations/get_repository", credential["token"], query,
    )
    assert status == 200

    # The example never gives these container paths or credentials to an agent.
    prefix = (
        f"/var/lib/enroute/objects/repos/{credential['tenant_id']}/"
        f"{result['repository_id']}"
    )
    active = result["active_generation"]
    files = (
        "from pathlib import Path; import json; "
        f"p = Path({prefix!r}); "
        "print(json.dumps({d.name: len(list(d.rglob('*.layer'))) "
        "for d in p.glob('g*')}))"
    )
    deadline = time.monotonic() + 30
    while True:
        counts = json.loads(docker(
            "compose", "exec", "-T", "enroute", "python3", "-c", files,
        ))
        old = sum(counts.get(f"g{number}", 0) for number in range(1, active))
        if old == 0 and counts.get(f"g{active}", 0) > 0:
            break
        if time.monotonic() >= deadline:
            raise TimeoutError(f"Layer cleanup did not finish: {counts}")
        time.sleep(0.5)
    print(json.dumps({
        "private_ports_unpublished": True,
        "outside_container_cannot_reach_grpc": True,
        "metadata_separated": True, "cross_tenant_denied": True,
        "old_layer_files": old, "active_layer_files": counts[f"g{active}"],
    }, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--credential-file", type=Path, required=True)
    parser.add_argument("--result-file", type=Path, required=True)
    main(parser.parse_args())
