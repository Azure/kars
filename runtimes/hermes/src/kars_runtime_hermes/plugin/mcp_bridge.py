"""Governed MCP bridge for Hermes models without native deferred-tool support."""

from __future__ import annotations

import json
import logging
import threading
from typing import Any

from . import router_client

logger = logging.getLogger("kars.hermes.mcp_bridge")

_ACCEPT = "application/json, text/event-stream"
_LOCK = threading.Lock()
_SESSION_ID: str | None = None
_NEXT_ID = 1


def _request_id() -> int:
    global _NEXT_ID
    value = _NEXT_ID
    _NEXT_ID += 1
    return value


def _headers() -> dict[str, str]:
    headers = {"Accept": _ACCEPT}
    if _SESSION_ID:
        headers["mcp-session-id"] = _SESSION_ID
    return headers


def _post(method: str, params: dict[str, Any], *, notification: bool = False) -> Any:
    body: dict[str, Any] = {
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    }
    if not notification:
        body["id"] = _request_id()
    response = router_client.call("POST", "/mcp", json=body, headers=_headers())
    if response.status_code >= 400:
        raise RuntimeError(f"MCP {method} HTTP {response.status_code}: {response.text[:500]}")
    if notification or response.status_code == 202 or not response.content:
        return None
    payload = response.json()
    if isinstance(payload, dict) and payload.get("error"):
        error = payload["error"] or {}
        raise RuntimeError(str(error.get("message") or error))
    return payload.get("result") if isinstance(payload, dict) else payload


def _initialize() -> None:
    global _SESSION_ID
    init = {
        "jsonrpc": "2.0",
        "id": _request_id(),
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "kars-runtime-hermes", "version": "0.1.0"},
        },
    }
    response = router_client.call(
        "POST",
        "/mcp",
        json=init,
        headers={"Accept": _ACCEPT},
    )
    if response.status_code >= 400:
        raise RuntimeError(f"MCP initialize HTTP {response.status_code}: {response.text[:500]}")
    payload = response.json()
    if isinstance(payload, dict) and payload.get("error"):
        error = payload["error"] or {}
        raise RuntimeError(str(error.get("message") or error))
    _SESSION_ID = response.headers.get("mcp-session-id")
    _post("notifications/initialized", {}, notification=True)


def _invoke(method: str, params: dict[str, Any]) -> Any:
    global _SESSION_ID
    with _LOCK:
        if _SESSION_ID is None:
            _initialize()
        try:
            return _post(method, params)
        except RuntimeError as exc:
            if method == "tools/call":
                message = str(exc).lower()
                if "session" in message or "400" in message:
                    _SESSION_ID = None
                raise
            message = str(exc).lower()
            if "session" not in message and "400" not in message:
                raise
            _SESSION_ID = None
            _initialize()
            return _post(method, params)


def _list_tools(_args: dict[str, Any], **_kwargs: Any) -> str:
    try:
        result = _invoke("tools/list", {})
        tools = result.get("tools", []) if isinstance(result, dict) else []
        return json.dumps(
            {
                "tools": [
                    {
                        "name": tool.get("name"),
                        "description": tool.get("description"),
                        "inputSchema": tool.get("inputSchema"),
                    }
                    for tool in tools
                    if isinstance(tool, dict)
                ]
            },
            separators=(",", ":"),
        )
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"MCP tools/list failed: {exc}"})


def _call_tool(args: dict[str, Any], **_kwargs: Any) -> str:
    name = str(args.get("name") or "").strip()
    if not name:
        return json.dumps({"error": "name is required"})
    arguments = args.get("arguments") or {}
    if not isinstance(arguments, dict):
        return json.dumps({"error": "arguments must be an object"})
    try:
        result = _invoke("tools/call", {"name": name, "arguments": arguments})
        return json.dumps(result, separators=(",", ":"))
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"MCP tool {name} failed: {exc}"})


_LIST_SCHEMA = {
    "name": "kars_mcp_list",
    "description": (
        "List the governed MCP tools mounted in this sandbox. Use this when the "
        "model's native deferred MCP catalog is unavailable."
    ),
    "parameters": {"type": "object", "properties": {}},
}

_CALL_SCHEMA = {
    "name": "kars_mcp_call",
    "description": (
        "Call one governed MCP tool by the exact namespaced name returned by "
        "kars_mcp_list, for example everything.echo."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "Exact namespaced MCP tool name"},
            "arguments": {"type": "object", "description": "Tool arguments matching its input schema"},
        },
        "required": ["name"],
    },
}


def register(ctx: Any) -> None:  # noqa: ANN401
    ctx.register_tool(
        name="kars_mcp_list",
        toolset="kars_mcp",
        schema=_LIST_SCHEMA,
        handler=_list_tools,
        description=_LIST_SCHEMA["description"],
    )
    ctx.register_tool(
        name="kars_mcp_call",
        toolset="kars_mcp",
        schema=_CALL_SCHEMA,
        handler=_call_tool,
        description=_CALL_SCHEMA["description"],
    )
    logger.info("governed MCP bridge registered")


def _reset_for_tests() -> None:
    global _SESSION_ID, _NEXT_ID
    _SESSION_ID = None
    _NEXT_ID = 1
