# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch

import httpx
import pytest

from kars_runtime_hermes.plugin import mcp_bridge, router_client


@pytest.fixture
def server():
    state = {"calls": [], "result": {"content": [{"type": "text", "text": "ok"}]}, "error": None, "status": 200}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["content-length"])))
            state["calls"].append((body, dict(self.headers)))
            if body["method"] == "initialize":
                self.send_response(200)
                self.send_header("mcp-session-id", "session-one")
                value = {
                    "jsonrpc": "2.0", "id": body["id"],
                    "result": {"protocolVersion": "2025-06-18", "capabilities": {}},
                }
            elif body["method"] == "notifications/initialized":
                self.send_response(202)
                self.end_headers()
                return
            else:
                self.send_response(state["status"])
                value = {"jsonrpc": "2.0", "id": body["id"]}
                if state["error"]:
                    value["error"] = state["error"]
                else:
                    value["result"] = (
                        {"tools": [{"name": "everything.echo"}]} if body["method"] == "tools/list" else state["result"]
                    )
            data = json.dumps(value).encode()
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=http.serve_forever, daemon=True)
    thread.start()
    mcp_bridge._reset_for_tests()
    try:
        with httpx.Client(base_url=f"http://127.0.0.1:{http.server_port}", trust_env=False) as client:
            with patch.object(router_client, "_client", return_value=client):
                yield state
    finally:
        http.shutdown()
        http.server_close()
        thread.join(timeout=5)


def test_real_http_negotiation_scope_headers_and_one_session(server):
    result = mcp_bridge._invoke("tools/list", {}, "everything")
    assert result["tools"][0]["name"] == "everything.echo"
    mcp_bridge._invoke("tools/call", {"name": "everything.echo", "arguments": {}}, "everything")
    assert [body["method"] for body, _ in server["calls"]] == [
        "initialize", "notifications/initialized", "tools/list", "tools/call"]
    headers = server["calls"][-1][1]
    assert headers["Mcp-Session-Id"] == "session-one"
    assert headers["MCP-Protocol-Version"] == "2025-06-18"
    assert headers["X-Kars-Mcp-Server"] == "everything"


def test_accepted_session_error_never_replays_or_copies_error_body(server):
    server["error"] = {"code": -32000, "message": "session expired PRIVATE_SENTINEL"}
    result = json.loads(mcp_bridge._call_tool({"name": "everything.echo"}))
    assert result["isError"] is True
    assert "PRIVATE_SENTINEL" not in json.dumps(result)
    assert sum(body["method"] == "tools/call" for body, _ in server["calls"]) == 1
    assert sum(body["method"] == "initialize" for body, _ in server["calls"]) == 1


def test_semantic_error_result_is_preserved(server):
    server["result"]["isError"] = True
    result = json.loads(mcp_bridge._call_tool({"name": "everything.echo", "arguments": {}}))
    assert result["isError"] is True
    assert result["content"] == server["result"]["content"]


def test_bad_arguments_and_scope_never_send_requests(server):
    assert json.loads(mcp_bridge._call_tool({"name": "everything.echo", "arguments": []}))["isError"] is True
    assert json.loads(mcp_bridge._list_tools({"server": "../other"}))["isError"] is True
    assert server["calls"] == []


def test_http_failure_is_not_replayed(server):
    server["status"] = 400
    result = json.loads(mcp_bridge._call_tool({"name": "everything.echo"}))
    assert result["isError"] is True
    assert sum(body["method"] == "tools/call" for body, _ in server["calls"]) == 1


def test_registers_both_real_handlers():
    tools = []

    class Context:
        def register_tool(self, **kwargs):
            tools.append(kwargs)

    mcp_bridge.register(Context())
    assert {tool["name"] for tool in tools} == {"kars_mcp_list", "kars_mcp_call"}
    assert all(callable(tool["handler"]) for tool in tools)
