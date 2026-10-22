"""Exercise the running test service with the official MCP Python SDK 2.3.0."""

import asyncio
import os
from importlib.metadata import version
from typing import Any

import httpx2
from mcp.client.session import ClientSession
from mcp.client.streamable_http import streamable_http_client


async def main() -> None:
    """Check negotiation, schemas, a complete agent loop, and stable errors."""
    headers = {"Authorization": f"Bearer {os.environ['MCP_TEST_TOKEN']}"}
    async with httpx2.AsyncClient(headers=headers, trust_env=False) as http:
        async with streamable_http_client(
            os.environ["MCP_TEST_URL"], http_client=http
        ) as (read, write):
            async with ClientSession(read, write) as session:
                initialized = await session.initialize()
                assert initialized.protocol_version == "2025-11-25"
                tools = await session.list_tools()
                assert len(tools.tools) >= 13

                async def call(name: str, arguments: dict[str, Any]) -> Any:
                    result = await session.call_tool(name, arguments)
                    assert not result.is_error, result.content
                    return result.structured_content

                repo = await call(
                    "create_repository",
                    {"name": "sdk-smoke", "idempotency_key": "sdk-create"},
                )
                repository_id = repo["repository_id"]
                arguments = {
                    "repository_id": repository_id,
                    "idempotency_key": "sdk-write",
                    "message": "SDK smoke test",
                    "changes": [{
                        "action": "put", "path": "hello", "contents": "MCP\n"
                    }],
                }
                commit = await call("write_changeset", arguments)
                assert await call("write_changeset", arguments) == commit
                contents = await call(
                    "read_file", {"repository_id": repository_id, "path": "hello"}
                )
                assert contents["contents"] == "MCP\n"
                missing = await session.call_tool(
                    "read_file", {"repository_id": repository_id, "path": "missing"}
                )
                assert missing.is_error
                assert missing.structured_content["error"]["code"] == "not_found"
                await call("delete_repository", {"repository_id": repository_id})
    print(f"Official MCP SDK {version('mcp')}: agent loop passed")


if __name__ == "__main__":
    asyncio.run(main())
