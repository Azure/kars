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


# ── foundry_memory — store name convention ─────────────────────────


def test_store_name_follows_kars_convention(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SANDBOX_NAME", "auditor")
    assert foundry._store_name() == "memory-auditor"


def test_store_name_truncates_long_sandbox_names(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SANDBOX_NAME", "a" * 100)
    name = foundry._store_name()
    assert len(name) <= 63
    assert name.startswith("memory-")


def test_store_name_falls_back_when_sandbox_name_unset(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("SANDBOX_NAME", raising=False)
    assert foundry._store_name() == "memory-unknown"


# ── foundry_memory — operation dispatch ─────────────────────────────


def test_foundry_memory_rejects_unknown_operation(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SANDBOX_NAME", "x")
    result = foundry._foundry_memory({"operation": "purge"})
    parsed = json.loads(result)
    assert "must be" in parsed["error"]


def test_foundry_memory_search_uses_correct_endpoint(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SANDBOX_NAME", "auditor")
    captured: dict[str, Any] = {}

    def fake_call(method: str, path: str, **kwargs: Any) -> httpx.Response:
        captured["method"] = method
        captured["path"] = path
        captured["body"] = kwargs.get("json")
        return _mock_response(200, {"memories": [{"text": "hello"}]})

    with mock.patch.object(foundry.router_client, "call", side_effect=fake_call):
        result = foundry._foundry_memory({"operation": "search", "query": "test"})

    assert captured["method"] == "POST"
    assert ":search_memories" in captured["path"]
    assert "memory-auditor" in captured["path"]
    assert captured["body"]["query_text"] == "test"
    assert captured["body"]["scope"] == "agent:auditor"
    parsed = json.loads(result)
    assert "memories" in parsed


def test_foundry_memory_update_requires_text(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SANDBOX_NAME", "x")
    result = foundry._foundry_memory({"operation": "update"})
    parsed = json.loads(result)
    assert "requires 'text'" in parsed["error"]


def test_foundry_memory_update_wraps_text_in_message(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SANDBOX_NAME", "x")
    captured: dict[str, Any] = {}

    def fake_call(method: str, path: str, **kwargs: Any) -> httpx.Response:
        captured["body"] = kwargs.get("json")
        return _mock_response(200, {"id": "mem-1"})

    with mock.patch.object(foundry.router_client, "call", side_effect=fake_call):
        foundry._foundry_memory({"operation": "update", "text": "remember this"})

    body = captured["body"]
    assert body["messages"][0]["content"] == "remember this"
    assert body["scope"] == "agent:x"


def test_foundry_memory_custom_scope_overrides_default(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SANDBOX_NAME", "x")
    captured: dict[str, Any] = {}

    def fake_call(method: str, path: str, **kwargs: Any) -> httpx.Response:
        captured["body"] = kwargs.get("json")
        return _mock_response(200, {"results": []})

    with mock.patch.object(foundry.router_client, "call", side_effect=fake_call):
        foundry._foundry_memory(
            {"operation": "search", "query": "x", "scope": "session:abc123"}
        )

    assert captured["body"]["scope"] == "session:abc123"


def test_foundry_memory_delete_scope() -> None:
    captured: dict[str, Any] = {}

    def fake_call(method: str, path: str, **kwargs: Any) -> httpx.Response:
        captured["path"] = path
        captured["body"] = kwargs.get("json")
        return _mock_response(200, {"deleted_count": 5})

    with mock.patch.object(foundry.router_client, "call", side_effect=fake_call):
        result = foundry._foundry_memory(
            {"operation": "delete_scope", "scope": "session:xyz"}
        )

    assert ":delete_scope" in captured["path"]
    assert captured["body"]["scope"] == "session:xyz"
    parsed = json.loads(result)
    assert parsed["deleted_count"] == 5


def test_foundry_memory_http_error_surfaces_status(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SANDBOX_NAME", "x")
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
