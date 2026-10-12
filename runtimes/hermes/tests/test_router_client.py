# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

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


def test_loopback_auth_client_never_uses_environment_proxy(monkeypatch):
    monkeypatch.setenv("HTTP_PROXY", "http://proxy.invalid:8080")
    monkeypatch.setenv("HTTPS_PROXY", "http://proxy.invalid:8080")
    router_client.clear_token_cache()
    try:
        with mock.patch.object(router_client.httpx, "Client") as client:
            router_client._client()
            assert client.call_args.kwargs["trust_env"] is False
            assert client.call_args.kwargs["base_url"] == "http://127.0.0.1:8443"
    finally:
        router_client.clear_token_cache()
