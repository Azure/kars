"""Unit tests for telemetry trust + signing-counter pushes."""

from __future__ import annotations

from typing import Any
from unittest import mock

import httpx
import pytest

from kars_runtime_hermes.plugin import telemetry


def _mock_response(status: int, body: Any | None = None) -> httpx.Response:
    return httpx.Response(
        status_code=status,
        json=body if body is not None else {},
        request=httpx.Request("POST", "http://127.0.0.1:8443/agt/trust"),
    )


def test_submit_trust_pushes_to_router() -> None:
    captured: dict[str, Any] = {}

    def fake_call(method: str, path: str, **kwargs: Any) -> httpx.Response:
        captured["method"] = method
        captured["path"] = path
        captured["body"] = kwargs.get("json")
        return _mock_response(200, {"trust_score": 800})

    with mock.patch.object(telemetry.router_client, "call", side_effect=fake_call):
        ok = telemetry.submit_trust("auditor", score=0.8)

    assert ok is True
    assert captured["method"] == "POST"
    assert captured["path"] == "/agt/trust"
    assert captured["body"]["agent_id"] == "auditor"
    # 0.8 → 800 (scaled to 0-1000 router range)
    assert captured["body"]["score"] == 800


def test_submit_trust_handles_already_integer_score() -> None:
    captured: dict[str, Any] = {}

    def fake_call(method: str, path: str, **kwargs: Any) -> httpx.Response:
        captured["body"] = kwargs.get("json")
        return _mock_response(200)

    with mock.patch.object(telemetry.router_client, "call", side_effect=fake_call):
        telemetry.submit_trust("auditor", score=500)

    # 500 stays as 500 (>1.0 → not scaled)
    assert captured["body"]["score"] == 500


def test_submit_trust_rejects_empty_agent_id() -> None:
    ok = telemetry.submit_trust("", score=0.8)
    assert ok is False


def test_submit_trust_fails_silently_on_network_error() -> None:
    def raise_(*_args: Any, **_kwargs: Any) -> None:
        raise httpx.ConnectError("router down")

    with mock.patch.object(telemetry.router_client, "call", side_effect=raise_):
        ok = telemetry.submit_trust("auditor", score=0.8)
    assert ok is False  # best-effort — no exception bubbles


def test_submit_trust_returns_false_on_http_error() -> None:
    with mock.patch.object(
        telemetry.router_client,
        "call",
        return_value=httpx.Response(
            500,
            text="upstream broke",
            request=httpx.Request("POST", "http://127.0.0.1:8443/agt/trust"),
        ),
    ):
        ok = telemetry.submit_trust("auditor", score=0.8)
    assert ok is False


# ── signing-counter ─────────────────────────────────────────────────


@pytest.mark.parametrize("action", ["signed", "verified", "rejected"])
def test_submit_signing_counter_accepts_valid_actions(action: str) -> None:
    captured: dict[str, Any] = {}

    def fake_call(method: str, path: str, **kwargs: Any) -> httpx.Response:
        captured["body"] = kwargs.get("json")
        return _mock_response(200)

    with mock.patch.object(telemetry.router_client, "call", side_effect=fake_call):
        ok = telemetry.submit_signing_counter(action)
    assert ok is True
    assert captured["body"]["action"] == action


def test_submit_signing_counter_rejects_invalid_action() -> None:
    ok = telemetry.submit_signing_counter("invented")
    assert ok is False


# ── post_tool_call hook ─────────────────────────────────────────────


def test_post_tool_call_skips_when_result_has_error(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SANDBOX_NAME", "auditor")
    called: list[bool] = []

    def fake_submit(*_args: Any, **_kwargs: Any) -> bool:
        called.append(True)
        return True

    with mock.patch.object(telemetry, "submit_trust", side_effect=fake_submit):
        telemetry._post_tool_call_hook(
            "kars_spawn", {"name": "x"}, '{"error": "name invalid"}'
        )
    assert called == []


def test_post_tool_call_pushes_trust_for_kars_tools(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SANDBOX_NAME", "auditor")
    captured: dict[str, Any] = {}

    def fake_submit(agent_id: str, **kwargs: Any) -> bool:
        captured["agent_id"] = agent_id
        captured["kwargs"] = kwargs
        return True

    with mock.patch.object(telemetry, "submit_trust", side_effect=fake_submit):
        telemetry._post_tool_call_hook(
            "kars_spawn", {"name": "child"}, '{"status": "ok"}'
        )

    assert captured["agent_id"] == "auditor"


def test_post_tool_call_pushes_trust_for_foundry_tools(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SANDBOX_NAME", "auditor")
    called: list[str] = []

    def fake_submit(agent_id: str, **_kwargs: Any) -> bool:
        called.append(agent_id)
        return True

    with mock.patch.object(telemetry, "submit_trust", side_effect=fake_submit):
        telemetry._post_tool_call_hook(
            "foundry_memory", {"operation": "search"}, '{"memories": []}'
        )
    assert called == ["auditor"]


def test_post_tool_call_skips_non_kars_tools(monkeypatch: pytest.MonkeyPatch) -> None:
    """http_fetch and shell aren't peer interactions — no trust signal."""
    monkeypatch.setenv("SANDBOX_NAME", "auditor")
    called: list[str] = []

    def fake_submit(agent_id: str, **_kwargs: Any) -> bool:
        called.append(agent_id)
        return True

    with mock.patch.object(telemetry, "submit_trust", side_effect=fake_submit):
        telemetry._post_tool_call_hook("http_fetch", {"url": "x"}, "<html></html>")
        telemetry._post_tool_call_hook("exec_command", {"command": "ls"}, "file1\n")
    assert called == []


def test_register_wires_post_tool_call_hook() -> None:
    class FakeCtx:
        def __init__(self) -> None:
            self.hooks: dict[str, Any] = {}

        def register_hook(self, name: str, handler: Any) -> None:
            self.hooks[name] = handler

    ctx = FakeCtx()
    telemetry.register(ctx)
    assert "post_tool_call" in ctx.hooks
