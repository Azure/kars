# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
Foundry MCP tools for the Microsoft Agent Framework Python SDK.

The router sidecar hosts a streamable-HTTP MCP server at
`/platform/mcp` that publishes the canonical 9-tool Foundry shim catalog
(mirrored from `inference-router/src/mcp/platform.rs`). MAF natively
supports streamable-HTTP MCP via `MCPStreamableHTTPTool`, but plumbing
that into every user agent is boilerplate; this module wraps each
Foundry tool as a MAF `@tool`-decorated callable so user agents pick
them up with one call to `register_foundry_tools(agent)`.
"""

from __future__ import annotations

import logging
import os
from typing import Any, Dict, Optional, Tuple

import httpx

logger = logging.getLogger(__name__)

DEFAULT_PLATFORM_MCP_URL = "http://127.0.0.1:8443/platform/mcp"

#: Canonical list of Foundry-shim tool names served by `/platform/mcp`
#: (see `inference-router/src/mcp/platform.rs::foundry_tool_catalog`).
FOUNDRY_TOOL_NAMES: Tuple[str, ...] = (
    "foundry.web_search",
    "foundry.code_execute",
    "foundry.file_search",
    "foundry.memory",
    "foundry.image_generation",
    "foundry.conversations",
    "foundry.evaluations",
    "foundry.deployments",
    "foundry.agents",
)


def _platform_mcp_url() -> str:
    return os.environ.get("KARS_PLATFORM_MCP_URL", DEFAULT_PLATFORM_MCP_URL)


class FoundryMCPClient:
    """Streamable-HTTP MCP client targeting the platform MCP server.

    We talk MCP `2024-11-05` JSON-RPC over a single HTTP POST per call —
    the simplest profile of streamable-HTTP that the router supports.
    """

    def __init__(
        self,
        url: Optional[str] = None,
        *,
        client: Optional[httpx.AsyncClient] = None,
        timeout: float = 30.0,
    ) -> None:
        self._url = url or _platform_mcp_url()
        self._client = client or httpx.AsyncClient(timeout=timeout)
        self._owns_client = client is None
        self._req_id = 0

    async def aclose(self) -> None:
        if self._owns_client:
            await self._client.aclose()

    async def call_tool(self, name: str, arguments: Dict[str, Any]) -> str:
        self._req_id += 1
        payload = {
            "jsonrpc": "2.0",
            "id": self._req_id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments or {}},
        }
        resp = await self._client.post(
            self._url,
            json=payload,
            headers={"Accept": "application/json, text/event-stream"},
        )
        resp.raise_for_status()
        body = resp.json()
        if "error" in body:
            err = body["error"]
            raise RuntimeError(
                f"MCP tool {name!r} failed: {err.get('code')} {err.get('message')}"
            )
        result = body.get("result", {})
        if result.get("isError") or result.get("is_error"):
            content = result.get("content") or []
            text = next(
                (c.get("text", "") for c in content if c.get("type") == "text"),
                "",
            )
            raise RuntimeError(f"MCP tool {name!r} returned is_error: {text}")
        content = result.get("content") or []
        chunks = [c.get("text", "") for c in content if c.get("type") == "text"]
        return "\n".join(chunks)


# A module-level singleton so registered tools share one HTTP client.
_default_mcp_client: Optional[FoundryMCPClient] = None


def _mcp_client() -> FoundryMCPClient:
    global _default_mcp_client
    if _default_mcp_client is None:
        _default_mcp_client = FoundryMCPClient()
    return _default_mcp_client


def reset_default_mcp_client() -> None:
    """Test hook."""
    global _default_mcp_client
    _default_mcp_client = None


def _make_tool(name: str):
    """Build a MAF `@tool`-decorated callable that fans out to MCP.

    The decorator is imported lazily so unit tests that exercise the MCP
    plumbing don't need the full `agent_framework` package installed.
    """
    from agent_framework import tool  # type: ignore

    short_name = name.replace(".", "_")
    description = f"Invoke the kars platform Foundry tool {name!r} via MCP."

    @tool(name=short_name, description=description)
    async def _tool_fn(arguments_json: str = "{}") -> str:
        """Call the Foundry tool. ``arguments_json`` is a JSON object string."""
        import json

        try:
            arguments = json.loads(arguments_json) if arguments_json else {}
        except json.JSONDecodeError as exc:
            raise ValueError(
                f"arguments_json must be a JSON object, got: {arguments_json!r}"
            ) from exc
        if not isinstance(arguments, dict):
            raise ValueError("arguments_json must decode to a JSON object")
        return await _mcp_client().call_tool(name, arguments)

    # Stash the canonical name on the wrapper for introspection.
    try:
        _tool_fn.__kars_tool_name__ = name  # type: ignore[attr-defined]
    except AttributeError:
        pass
    return _tool_fn


def build_foundry_tools() -> list:
    """Return a list of MAF tools for every Foundry-shim tool."""
    return [_make_tool(name) for name in FOUNDRY_TOOL_NAMES]


def _tool_canonical_name(t: Any) -> Optional[str]:
    return (
        getattr(t, "__kars_tool_name__", None)
        or getattr(t, "name", None)
        or getattr(getattr(t, "metadata", None), "name", None)
    )


def register_foundry_tools(agent: Any) -> None:
    """Attach all 9 Foundry tools to *agent*.

    Works with any object exposing a mutable `tools` attribute (this
    includes MAF `ChatAgent`, which stores tools in the chat client's
    options or directly on the agent depending on construction). The
    function is idempotent per agent — names already present are
    skipped.
    """
    if not hasattr(agent, "tools"):
        raise TypeError(
            "agent must expose a mutable `tools` attribute (got "
            f"{type(agent).__name__})"
        )
    existing = agent.tools or []
    existing_names = {n for n in (_tool_canonical_name(t) for t in existing) if n}
    new_tools = [
        t
        for t in build_foundry_tools()
        if _tool_canonical_name(t) not in existing_names
    ]
    if agent.tools is None:
        agent.tools = []
    agent.tools = list(agent.tools) + new_tools
    logger.info("registered %d foundry tools on agent", len(new_tools))
