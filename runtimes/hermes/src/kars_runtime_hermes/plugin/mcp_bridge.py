# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Bounded MCP access through the existing loopback router client."""

from __future__ import annotations

import json
import re
import threading
from typing import Any

from . import router_client

_LOCK = threading.Lock()
_SLOTS = threading.BoundedSemaphore(8)
_SESSION: str | None = None
_INITIALIZED = False
_NEXT_ID = 1
_PROTOCOL = "2025-06-18"
_MAX_BYTES = 2 * 1024 * 1024


def _headers(server: str | None) -> dict[str, str]:
    headers = {"Accept": "application/json, text/event-stream", "MCP-Protocol-Version": _PROTOCOL}
    if _SESSION:
        headers["Mcp-Session-Id"] = _SESSION
    if server:
        headers["X-Kars-Mcp-Server"] = server
    return headers


def _request(method: str, params: dict[str, Any], server: str | None, notification: bool = False) -> tuple[Any, dict[str, Any]]:
    global _NEXT_ID, _INITIALIZED
    request_id = _NEXT_ID
    _NEXT_ID += 1
    body = {"jsonrpc": "2.0", "method": method, "params": params}
    if not notification:
        body["id"] = request_id
    if len(json.dumps(body).encode()) > _MAX_BYTES:
        raise RuntimeError("MCP request exceeds its byte limit")
    response = router_client.call("POST", "/mcp", json=body, headers=_headers(server))
    if response.status_code in (400, 404):
        _INITIALIZED = False
    if not 200 <= response.status_code < 300:
        raise RuntimeError(f"MCP HTTP {response.status_code}")
    if len(response.content) > _MAX_BYTES:
        raise RuntimeError("MCP response exceeds its byte limit")
    if notification:
        return response, {}
    try:
        value = response.json()
    except Exception:
        raise RuntimeError("MCP router response is not valid JSON") from None
    if not isinstance(value, dict) or value.get("jsonrpc") != "2.0" or value.get("id") != request_id \
            or value.get("error") or not isinstance(value.get("result"), dict):
        raise RuntimeError("MCP RPC failed or omitted its matching result")
    return response, value["result"]


def _initialize(server: str | None) -> None:
    global _SESSION, _INITIALIZED, _PROTOCOL
    _SESSION = None
    _PROTOCOL = "2025-06-18"
    response, result = _request("initialize", {
        "protocolVersion": _PROTOCOL, "capabilities": {},
        "clientInfo": {"name": "kars-runtime-hermes", "version": "0.1.0"},
    }, server)
    protocol = result.get("protocolVersion")
    if protocol not in ("2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"):
        raise RuntimeError("MCP negotiated an unsupported protocol")
    session = response.headers.get("mcp-session-id")
    if session is not None and (not session or len(session) > 1024 or not re.fullmatch(r"[\x21-\x7e]+", session)):
        raise RuntimeError("MCP session identifier is invalid")
    _SESSION, _PROTOCOL = session, protocol
    _request("notifications/initialized", {}, server, notification=True)
    _INITIALIZED = True


def _invoke(method: str, params: dict[str, Any], server: str | None = None) -> dict[str, Any]:
    if server is not None and (not isinstance(server, str) or len(server) > 253
                              or not re.fullmatch(r"[a-z0-9.-]+", server)):
        raise RuntimeError("Invalid MCP server scope")
    if not _SLOTS.acquire(blocking=False):
        raise RuntimeError("MCP bridge queue is full")
    locked = False
    try:
        locked = _LOCK.acquire(timeout=60)
        if not locked:
            raise RuntimeError("MCP bridge queue timed out")
        if not _INITIALIZED:
            _initialize(server)
        # No tools/call retry, including accepted JSON-RPC errors.
        _, result = _request(method, params, server)
        return result
    finally:
        if locked:
            _LOCK.release()
        _SLOTS.release()


def _failure(message: str) -> str:
    return json.dumps({"error": message, "isError": True})


def _list_tools(args: dict[str, Any], **_kwargs: Any) -> str:
    try:
        result = _invoke("tools/list", {}, args.get("server"))
        if not isinstance(result.get("tools"), list):
            return _failure("MCP catalog omitted tools")
        return json.dumps(result, separators=(",", ":"))
    except Exception:
        return _failure("MCP discovery failed; inspect the router's scoped status")


def _call_tool(args: dict[str, Any], **_kwargs: Any) -> str:
    name, arguments = args.get("name"), args.get("arguments", {})
    if not isinstance(name, str) or not name.strip():
        return _failure("name is required")
    if not isinstance(arguments, dict):
        return _failure("arguments must be an object")
    try:
        return json.dumps(_invoke("tools/call", {"name": name, "arguments": arguments}, args.get("server")), separators=(",", ":"))
    except Exception:
        return _failure("MCP invocation failed; calls are never automatically replayed")


def register(ctx: Any) -> None:
    for name, description, parameters, handler in (
        ("kars_mcp_list", "List qualified MCP tools mounted in this sandbox, optionally scoped to one server.",
         {"type": "object", "properties": {"server": {"type": "string"}}}, _list_tools),
        ("kars_mcp_call", "Call an exact MCP tool name returned by kars_mcp_list. No automatic call replay.",
         {"type": "object", "properties": {"name": {"type": "string"}, "arguments": {"type": "object"},
                                          "server": {"type": "string"}}, "required": ["name"]}, _call_tool),
    ):
        ctx.register_tool(name=name, toolset="kars_mcp",
                          schema={"name": name, "description": description, "parameters": parameters},
                          handler=handler, description=description)


def _reset_for_tests() -> None:
    global _SESSION, _INITIALIZED, _NEXT_ID, _PROTOCOL
    _SESSION, _INITIALIZED, _NEXT_ID, _PROTOCOL = None, False, 1, "2025-06-18"
