"""Unit tests for router_client.call header forwarding."""

from __future__ import annotations

from typing import Any
from unittest import mock

from kars_runtime_hermes.plugin import router_client


def test_call_forwards_headers_to_httpx() -> None:
    """Per-call headers (e.g. the MCP Accept header) must reach the underlying
    httpx request, merged onto the client defaults."""
    captured: dict[str, Any] = {}

    class _FakeClient:
        def request(self, method: str, path: str, **kwargs: Any) -> str:
            captured["method"] = method
            captured["path"] = path
            captured["headers"] = kwargs.get("headers")
            return "resp"

    with mock.patch.object(router_client, "_client", return_value=_FakeClient()):
        out = router_client.call(
            "POST",
            "/platform/mcp",
            json={"x": 1},
            headers={"Accept": "application/json, text/event-stream"},
        )

    assert out == "resp"
    assert captured["method"] == "POST"
    assert captured["path"] == "/platform/mcp"
    assert captured["headers"] == {"Accept": "application/json, text/event-stream"}


def test_call_without_headers_passes_none() -> None:
    """Backward-compat: omitting headers must not break existing callers."""
    captured: dict[str, Any] = {}

    class _FakeClient:
        def request(self, method: str, path: str, **kwargs: Any) -> str:
            captured["headers"] = kwargs.get("headers")
            return "resp"

    with mock.patch.object(router_client, "_client", return_value=_FakeClient()):
        router_client.call("GET", "/healthz")

    assert captured["headers"] is None
