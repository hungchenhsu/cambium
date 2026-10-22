"""Run an MCP agent loop and verify its result with stock Git.

Install examples/requirements.txt, then pass a private bootstrap credential
file.
The example retains the repository and checkout so they can be inspected.
"""

from __future__ import annotations

import argparse
import asyncio
from contextlib import asynccontextmanager
import json
import os
from pathlib import Path
import secrets
import subprocess
from typing import Any, AsyncIterator
from uuid import uuid4

import httpx2
from mcp.client.session import ClientSession
from mcp.client.streamable_http import streamable_http_client


@asynccontextmanager
async def connect(url: str, token: str) -> AsyncIterator[ClientSession]:
    """Keep credentials in headers and the client environment only."""
    headers = {"Authorization": f"Bearer {token}"}
    async with httpx2.AsyncClient(headers=headers, trust_env=False) as http:
        async with streamable_http_client(
            url, http_client=http
        ) as (read, write):
            async with ClientSession(read, write) as session:
                await session.initialize()
                yield session


async def call(
    session: ClientSession, name: str, arguments: dict[str, Any]
) -> dict[str, Any]:
    """Treat a structured operation error as a failed step."""
    result = await session.call_tool(name, arguments)
    if result.is_error or not isinstance(result.structured_content, dict):
        raise RuntimeError(f"{name}: {result.structured_content}")
    return result.structured_content


def clone(url: str, token: str, destination: Path, expected: str) -> None:
    """Verify the live commit without saving a token in Git configuration."""
    environment = os.environ.copy()
    environment.update({
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_TERMINAL_PROMPT": "0",
        "GIT_CONFIG_COUNT": "1",
        "GIT_CONFIG_KEY_0": "http.extraHeader",
        "GIT_CONFIG_VALUE_0": f"Authorization: Bearer {token}",
    })
    subprocess.run(
        ["git", "clone", "--quiet", url, str(destination)],
        env=environment, check=True,
    )
    actual = subprocess.check_output(
        ["git", "-C", str(destination), "rev-parse", "HEAD"], text=True,
    ).strip()
    if actual != expected:
        raise RuntimeError("Git clone returned a different commit")


async def main(args: argparse.Namespace) -> None:
    """Create, delegate, write, read, expire, reclaim, and clone."""
    credential = json.loads(args.credential_file.read_text())
    name = f"agent-demo-{uuid4().hex[:12]}"
    async with connect(args.mcp_url, credential["token"]) as operator:
        repository = await call(operator, "create_repository", {
            "name": name, "idempotency_key": f"{name}-create",
        })
        repo = repository["repository_id"]
        agent = await call(operator, "create_identity", {
            "kind": "agent", "name": name,
            "owner_id": credential["identity_id"],
        })
        delegated = await call(operator, "mint_token", {
            "identity_id": agent["identity_id"], "scope": "repo:write",
            "repositories": [repo], "ttl_seconds": 300,
        })
        async with connect(args.mcp_url, delegated["token"]) as session:
            write = {
                "repository_id": repo,
                "message": "An agent writes without a checkout",
                "idempotency_key": f"{name}-write",
                "changes": [{
                    "action": "put", "path": "hello.txt",
                    "contents": "Hello from MCP!\n",
                }],
            }
            commit = await call(session, "write_changeset", write)
            replay = await call(session, "write_changeset", write)
            if replay != commit:
                raise RuntimeError("Idempotent replay changed the result")
            read = await call(session, "read_file", {
                "repository_id": repo, "path": "hello.txt",
            })
            if read["contents"] != "Hello from MCP!\n":
                raise RuntimeError("Read returned different contents")
            branch = await call(session, "create_ephemeral_branch", {
                "repository_id": repo, "name": "scratch",
                "start_commit": commit["commit_id"], "ttl_seconds": 5,
            })
            scratch = await call(session, "write_changeset", {
                "repository_id": repo, "branch": "scratch",
                "message": "Discard this experiment when its TTL ends",
                "changes": [{
                    "action": "put", "path": "scratch.txt",
                    "contents": secrets.token_hex(16384),
                }],
            })
            deadline = asyncio.get_running_loop().time() + args.wait_seconds
            while True:
                state = await call(session, "get_branch", {
                    "repository_id": repo, "branch_id": branch["branch_id"],
                })
                if state["state"] == "expired":
                    break
                if asyncio.get_running_loop().time() >= deadline:
                    raise TimeoutError("The expiry worker did not finish")
                await asyncio.sleep(0.25)
            expired = await session.call_tool("read_file", {
                "repository_id": repo, "revision": "scratch",
                "path": "scratch.txt",
            })
            if (not expired.is_error
                    or expired.structured_content["error"]["code"]
                    != "branch_expired"):
                raise RuntimeError("The expired name remained readable")
            await call(operator, "request_compaction", {"repository_id": repo})
            while True:
                status = await call(
                    session, "get_reclamation", {"repository_id": repo}
                )
                if (status.get("compaction", {}).get("stage") == "complete"
                        and status["unreclaimed_expiries"] == 0):
                    break
                if asyncio.get_running_loop().time() >= deadline:
                    raise TimeoutError(
                        "Compaction timed out; use the demo timing settings"
                    )
                await asyncio.sleep(0.25)
            discarded = await session.call_tool("read_file", {
                "repository_id": repo, "revision": scratch["commit_id"],
                "path": "scratch.txt",
            })
            if (not discarded.is_error
                    or discarded.structured_content["error"]["code"]
                    != "not_found"):
                raise RuntimeError("Unreachable experiment was retained")
            destination = args.checkout_parent / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            await asyncio.to_thread(
                clone, repository["git_url"], delegated["token"], destination,
                commit["commit_id"],
            )
            print(json.dumps({
                "repository_id": repo, "git_url": repository["git_url"],
                "commit_id": commit["commit_id"],
                "branch_id": branch["branch_id"],
                "active_generation": status["active_generation"],
                "unreclaimed_expiries": status["unreclaimed_expiries"],
                "checkout": str(destination), "verified": True,
            }, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--credential-file", type=Path, required=True)
    parser.add_argument("--mcp-url", default="http://127.0.0.1:8080/mcp")
    parser.add_argument(
        "--checkout-parent", type=Path, default=Path(".cambium-demo")
    )
    parser.add_argument("--wait-seconds", type=float, default=60)
    asyncio.run(main(parser.parse_args()))
