"""Unit tests for foundry_memory + http_fetch."""

from __future__ import annotations

import json
from typing import Any
from unittest import mock

import httpx
import pytest

from kars_runtime_hermes.plugin import foundry, http_fetch


def _mock_response(status: int, body: Any | None = None, text: str = "") -> httpx.Response:
    if body is not None:
        return httpx.Response(
            status_code=status,
            json=body,
            request=httpx.Request("POST", "http://127.0.0.1:8443/x"),
        )
    return httpx.Response(
        status_code=status,
        text=text,
        request=httpx.Request("POST", "http://127.0.0.1:8443/x"),
    )


class _FakeCtx:
    def __init__(self) -> None:
        self.tools: dict[str, Any] = {}

    def register_tool(self, **kwargs: Any) -> None:
        self.tools[kwargs["name"]] = kwargs


# ── foundry_memory — thin client over the router platform MCP ──────
#
# The router owns the Memory Store contract; the Hermes tool only
# forwards intent to `POST /platform/mcp` as a JSON-RPC `tools/call`.
# These tests pin THAT seam — the on-the-wire contract is exercised by
# the router's own platform tests (inference-router/src/mcp/platform.rs).


def _rpc_ok(text: str, *, is_error: bool = False) -> dict[str, Any]:
    """A JSON-RPC `tools/call` success envelope as the router returns it."""
    return {
        "jsonrpc": "2.0",
        "id": 1,
        "result": {"content": [{"type": "text", "text": text}], "isError": is_error},
    }


def _capture_platform_call(response: httpx.Response) -> tuple[dict[str, Any], Any]:
    """Build a `router_client.call` fake that records the JSON-RPC body."""
    captured: dict[str, Any] = {}

    def fake_call(method: str, path: str, **kwargs: Any) -> httpx.Response:
        captured["method"] = method
        captured["path"] = path
        captured["body"] = kwargs.get("json")
        return response

    return captured, fake_call


def test_foundry_memory_forwards_search_to_platform_mcp() -> None:
    captured, fake_call = _capture_platform_call(_mock_response(200, _rpc_ok('{"memories": []}')))
    with mock.patch.object(foundry.router_client, "call", side_effect=fake_call):
        result = foundry._foundry_memory(
            {"operation": "search", "query": "coffee?", "top_k": 5}
        )

    assert captured["method"] == "POST"
    assert captured["path"] == "/platform/mcp"
    body = captured["body"]
    assert body["method"] == "tools/call"
    assert body["params"]["name"] == "foundry.memory"
    # Only intent is forwarded — no store name, no scope unless given,
    # no REST contract shape.
    assert body["params"]["arguments"] == {
        "operation": "search",
        "query": "coffee?",
        "top_k": 5,
    }
    # The router's text result is flattened back to the caller.
    assert json.loads(result) == {"memories": []}


def test_foundry_memory_forwards_update_text() -> None:
    captured, fake_call = _capture_platform_call(_mock_response(200, _rpc_ok("queued")))
    with mock.patch.object(foundry.router_client, "call", side_effect=fake_call):
        result = foundry._foundry_memory(
            {"operation": "update", "text": "likes dark roast"}
        )
    assert captured["body"]["params"]["arguments"] == {
        "operation": "update",
        "text": "likes dark roast",
    }
    assert result == "queued"


def test_foundry_memory_forwards_scope_and_delete_scope() -> None:
    captured, fake_call = _capture_platform_call(_mock_response(200, _rpc_ok('{"deleted": 3}')))
    with mock.patch.object(foundry.router_client, "call", side_effect=fake_call):
        foundry._foundry_memory({"operation": "delete_scope", "scope": "session_xyz"})
    assert captured["body"]["params"]["arguments"] == {
        "operation": "delete_scope",
        "scope": "session_xyz",
    }


def test_foundry_memory_surfaces_jsonrpc_error() -> None:
    err_env = {
        "jsonrpc": "2.0",
        "id": 1,
        "error": {"code": -32602, "message": "bad", "data": {"reason": "operation must be search"}},
    }
    _, fake_call = _capture_platform_call(_mock_response(200, err_env))
    with mock.patch.object(foundry.router_client, "call", side_effect=fake_call):
        result = foundry._foundry_memory({"operation": "purge"})
    parsed = json.loads(result)
    assert "operation must be search" in parsed["error"]


def test_foundry_memory_surfaces_tool_is_error_text() -> None:
    # A tool-level failure (e.g. upstream 403) comes back as result.content
    # text with isError=true; the text is what the LLM should see.
    hint = "foundry.memory:search returned HTTP 403 (verify the project managed identity...)"
    _, fake_call = _capture_platform_call(_mock_response(200, _rpc_ok(hint, is_error=True)))
    with mock.patch.object(foundry.router_client, "call", side_effect=fake_call):
        result = foundry._foundry_memory({"operation": "search", "query": "x"})
    assert "HTTP 403" in result


def test_foundry_memory_surfaces_http_error() -> None:
    with mock.patch.object(
        foundry.router_client,
        "call",
        return_value=_mock_response(500, text="upstream broke"),
    ):
        result = foundry._foundry_memory({"operation": "search", "query": "x"})
    parsed = json.loads(result)
    assert "500" in parsed["error"]


# ── foundry_memory — registration gating ───────────────────────────


def test_foundry_memory_skipped_in_github_copilot_mode(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("KARS_PROVIDER", "github-copilot")
    monkeypatch.setenv("FOUNDRY_PROJECT_ENDPOINT", "https://my-foundry.azure.com")
    ctx = _FakeCtx()
    foundry.register(ctx)
    assert "foundry_memory" not in ctx.tools


def test_foundry_memory_skipped_in_github_models_mode(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("KARS_PROVIDER", "github-models")
    monkeypatch.setenv("FOUNDRY_PROJECT_ENDPOINT", "https://my-foundry.azure.com")
    ctx = _FakeCtx()
    foundry.register(ctx)
    assert "foundry_memory" not in ctx.tools


def test_foundry_memory_skipped_without_foundry_endpoint(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("KARS_PROVIDER", raising=False)
    monkeypatch.delenv("FOUNDRY_PROJECT_ENDPOINT", raising=False)
    ctx = _FakeCtx()
    foundry.register(ctx)
    assert "foundry_memory" not in ctx.tools


def test_foundry_memory_registered_when_foundry_bound(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("KARS_PROVIDER", raising=False)
    monkeypatch.setenv("FOUNDRY_PROJECT_ENDPOINT", "https://my-foundry.azure.com")
    ctx = _FakeCtx()
    foundry.register(ctx)
    assert "foundry_memory" in ctx.tools


# ── http_fetch ─────────────────────────────────────────────────────


def test_http_fetch_requires_url() -> None:
    result = http_fetch._http_fetch({})
    parsed = json.loads(result)
    assert "required" in parsed["error"]


def test_http_fetch_proxies_via_egress() -> None:
    captured: dict[str, Any] = {}

    def fake_call(method: str, path: str, **kwargs: Any) -> httpx.Response:
        captured["method"] = method
        captured["path"] = path
        captured["body"] = kwargs.get("json")
        return _mock_response(200, text="<!DOCTYPE html><html></html>")

    with mock.patch.object(http_fetch.router_client, "call", side_effect=fake_call):
        result = http_fetch._http_fetch(
            {"url": "https://example.com", "method": "POST", "body": "{\"x\":1}"}
        )

    assert captured["method"] == "POST"
    assert captured["path"] == "/egress/fetch"
    assert captured["body"]["url"] == "https://example.com"
    assert captured["body"]["method"] == "POST"
    assert captured["body"]["body"] == '{"x":1}'
    assert "<!DOCTYPE html>" in result


def test_http_fetch_denial_surfaces_error() -> None:
    with mock.patch.object(
        http_fetch.router_client,
        "call",
        return_value=_mock_response(403, text="domain blocked"),
    ):
        result = http_fetch._http_fetch({"url": "https://evil.example"})
    parsed = json.loads(result)
    assert "denied" in parsed["error"]
    assert "blocked" in parsed["body"]


def test_http_fetch_response_truncated_at_32kb() -> None:
    big = "x" * (50 * 1024)  # 50KB
    with mock.patch.object(
        http_fetch.router_client,
        "call",
        return_value=_mock_response(200, text=big),
    ):
        result = http_fetch._http_fetch({"url": "https://example.com"})
    assert len(result) <= 32 * 1024


def test_http_fetch_register_wires_tool() -> None:
    ctx = _FakeCtx()
    http_fetch.register(ctx)
    assert "http_fetch" in ctx.tools


# ── foundry_memory — MCP Accept negotiation (regression) ───────────


def test_foundry_memory_sends_mcp_accept_header() -> None:
    """The router's /platform/mcp REQUIRES Accept: application/json +
    text/event-stream, else it 406s. The thin client must send it —
    this is the bug that broke memory end-to-end."""
    captured: dict[str, Any] = {}

    def fake_call(method: str, path: str, **kwargs: Any) -> httpx.Response:
        captured["path"] = path
        captured["headers"] = kwargs.get("headers")
        return _mock_response(200, _rpc_ok('{"memories": []}'))

    with mock.patch.object(foundry.router_client, "call", side_effect=fake_call):
        foundry._foundry_memory({"operation": "search", "query": "x"})

    assert captured["path"] == "/platform/mcp"
    headers = captured["headers"]
    assert headers is not None, "thin client must pass headers"
    accept = headers.get("Accept", "")
    assert "application/json" in accept
    assert "text/event-stream" in accept
