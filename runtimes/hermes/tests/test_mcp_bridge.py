from __future__ import annotations

import json
from typing import Any
from unittest import mock

import httpx

from kars_runtime_hermes.plugin import mcp_bridge


def _response(
    status: int,
    payload: dict[str, Any] | None = None,
    *,
    headers: dict[str, str] | None = None,
) -> httpx.Response:
    request = httpx.Request("POST", "http://127.0.0.1:8443/mcp")
    if payload is None:
        return httpx.Response(status, request=request, headers=headers)
    return httpx.Response(status, request=request, headers=headers, json=payload)


def test_list_initializes_session_and_returns_namespaced_tools() -> None:
    mcp_bridge._reset_for_tests()
    responses = [
        _response(
            200,
            {"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": "2025-06-18"}},
            headers={"mcp-session-id": "session-1"},
        ),
        _response(202),
        _response(
            200,
            {
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "tools": [
                        {
                            "name": "everything.echo",
                            "description": "Echo",
                            "inputSchema": {"type": "object"},
                        }
                    ]
                },
            },
        ),
    ]
    with mock.patch.object(mcp_bridge.router_client, "call", side_effect=responses) as call:
        result = json.loads(mcp_bridge._list_tools({}))

    assert result["tools"][0]["name"] == "everything.echo"
    assert call.call_args_list[2].kwargs["headers"]["mcp-session-id"] == "session-1"


def test_call_returns_real_mcp_result() -> None:
    mcp_bridge._reset_for_tests()
    responses = [
        _response(
            200,
            {"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": "2025-06-18"}},
            headers={"mcp-session-id": "session-2"},
        ),
        _response(202),
        _response(
            200,
            {
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "content": [{"type": "text", "text": "Echo: HERMES_H100_START"}],
                    "isError": False,
                },
            },
        ),
    ]
    with mock.patch.object(mcp_bridge.router_client, "call", side_effect=responses):
        result = json.loads(
            mcp_bridge._call_tool(
                {
                    "name": "everything.echo",
                    "arguments": {"message": "HERMES_H100_START"},
                }
            )
        )

    assert result["content"][0]["text"] == "Echo: HERMES_H100_START"
    assert result["isError"] is False


def test_list_reinitializes_once_after_stale_session() -> None:
    mcp_bridge._reset_for_tests()
    responses = [
        _response(
            200,
            {"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": "2025-06-18"}},
            headers={"mcp-session-id": "old"},
        ),
        _response(202),
        _response(400, {"error": {"message": "No valid session ID"}}),
        _response(
            200,
            {"jsonrpc": "2.0", "id": 3, "result": {"protocolVersion": "2025-06-18"}},
            headers={"mcp-session-id": "new"},
        ),
        _response(202),
        _response(
            200,
            {
                "jsonrpc": "2.0",
                "id": 4,
                "result": {"tools": [{"name": "everything.echo"}]},
            },
        ),
    ]
    with mock.patch.object(mcp_bridge.router_client, "call", side_effect=responses):
        result = json.loads(mcp_bridge._list_tools({}))

    assert result["tools"][0]["name"] == "everything.echo"


def test_call_does_not_retry_after_session_error() -> None:
    mcp_bridge._reset_for_tests()
    responses = [
        _response(
            200,
            {"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": "2025-06-18"}},
            headers={"mcp-session-id": "old"},
        ),
        _response(202),
        _response(400, {"error": {"message": "No valid session ID"}}),
    ]
    with mock.patch.object(mcp_bridge.router_client, "call", side_effect=responses) as call:
        result = json.loads(
            mcp_bridge._call_tool({"name": "everything.echo", "arguments": {}})
        )

    assert "No valid session ID" in result["error"]
    assert call.call_count == 3
    assert mcp_bridge._SESSION_ID is None
