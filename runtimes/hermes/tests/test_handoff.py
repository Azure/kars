"""Unit tests for kars_handoff_* family — status, request, confirm.

The Hermes plugin's handoff module is a thin wrapper over the
inference router's `/agt/handoff/*` endpoints, mirroring the OpenClaw
plugin's shape from `runtimes/openclaw/src/core/agt-tools/agt.ts`.
These tests pin the LLM-facing contract: which tools register under
which AGT_REGISTRY_MODE, how direction arg normalization works,
and that responses are forwarded verbatim from the router so the
operator sees the same JSON the router emits.
"""

from __future__ import annotations

import json
from typing import Any
from unittest import mock

import httpx
import pytest

from kars_runtime_hermes.plugin import handoff


def _mock_response(status: int, body: Any | None = None, text: str = "") -> httpx.Response:
    if body is not None:
        return httpx.Response(
            status_code=status,
            json=body,
            request=httpx.Request("GET", "http://127.0.0.1:8443/x"),
        )
    return httpx.Response(
        status_code=status,
        text=text,
        request=httpx.Request("GET", "http://127.0.0.1:8443/x"),
    )


class _FakeCtx:
    def __init__(self) -> None:
        self.tools: dict[str, Any] = {}

    def register_tool(
        self,
        *,
        name: str,
        toolset: str,
        schema: dict[str, Any],
        handler: Any,
        description: str = "",
    ) -> None:
        self.tools[name] = {
            "toolset": toolset,
            "schema": schema,
            "handler": handler,
            "description": description,
        }


# ── registration: local mode ────────────────────────────────────────


def test_local_mode_registers_status_only(monkeypatch: pytest.MonkeyPatch) -> None:
    """In `AGT_REGISTRY_MODE=local` only kars_handoff_status registers —
    request/confirm would always 409 against the router so we don't
    expose them. Mirrors OpenClaw's `if (registryMode === "global")`
    gate in agt.ts."""
    monkeypatch.setenv("AGT_REGISTRY_MODE", "local")
    ctx = _FakeCtx()
    handoff.register(ctx)
    assert "kars_handoff_status" in ctx.tools
    assert "kars_handoff_request" not in ctx.tools
    assert "kars_handoff_confirm" not in ctx.tools


def test_no_env_defaults_to_local(monkeypatch: pytest.MonkeyPatch) -> None:
    """Missing AGT_REGISTRY_MODE defaults to 'local' (safe — no
    exposed mutations against a router that will reject them)."""
    monkeypatch.delenv("AGT_REGISTRY_MODE", raising=False)
    ctx = _FakeCtx()
    handoff.register(ctx)
    assert set(ctx.tools.keys()) == {"kars_handoff_status"}


# ── registration: global mode ───────────────────────────────────────


def test_global_mode_registers_all_three(monkeypatch: pytest.MonkeyPatch) -> None:
    """In `AGT_REGISTRY_MODE=global` all three handoff tools register
    — full parity with OpenClaw's plugin surface."""
    monkeypatch.setenv("AGT_REGISTRY_MODE", "global")
    ctx = _FakeCtx()
    handoff.register(ctx)
    assert set(ctx.tools.keys()) == {
        "kars_handoff_status",
        "kars_handoff_request",
        "kars_handoff_confirm",
    }


# ── status: response shapes ─────────────────────────────────────────


def test_status_404_returns_idle_shape() -> None:
    """Router returning 404 (no active handoff) is the steady state for
    every fresh sandbox. Mirror OpenClaw's `handoff_available: false`
    return shape so an LLM can branch on a single conventional key."""
    with mock.patch(
        "kars_runtime_hermes.plugin.router_client.call",
        return_value=_mock_response(404, text="not found"),
    ):
        result = handoff._kars_handoff_status({}, ctx=None)
    parsed = json.loads(result)
    assert parsed == {
        "handoff_available": False,
        "active": False,
        "phase": "none",
        "status": "idle",
    }


def test_status_forwards_router_body_verbatim() -> None:
    """A non-404 response from the router is passed through unchanged.
    Critical: the LLM polls handoff_status and relays each new step to
    the user — if we rewrap or reshape the body, we drop step IDs or
    timestamps the prompt expects."""
    router_body = {
        "phase": "running",
        "status": "running",
        "direction": "local_to_aks",
        "active": True,
        "total_steps": 3,
        "new_steps": ["Step 1", "Step 2", "Step 3"],
    }
    with mock.patch(
        "kars_runtime_hermes.plugin.router_client.call",
        return_value=_mock_response(200, body=router_body),
    ):
        result = handoff._kars_handoff_status({"since_step": 0}, ctx=None)
    parsed = json.loads(result)
    assert parsed == router_body


def test_status_since_step_passed_through_as_query_param() -> None:
    """When the LLM passes since_step=N, the router gets it as a query
    parameter so it can slice the steps array server-side instead of
    returning everything every poll."""
    captured: dict[str, Any] = {}

    def _capture(method: str, path: str, *, json: Any = None, params: dict | None = None) -> httpx.Response:
        captured["method"] = method
        captured["path"] = path
        captured["params"] = params
        return _mock_response(200, body={"phase": "running", "total_steps": 5})

    with mock.patch(
        "kars_runtime_hermes.plugin.router_client.call",
        side_effect=_capture,
    ):
        handoff._kars_handoff_status({"since_step": 3}, ctx=None)
    assert captured["method"] == "GET"
    assert captured["path"] == "/agt/handoff/status"
    assert captured["params"] == {"since_step": 3}


def test_status_since_step_zero_or_missing_no_param() -> None:
    """since_step=0 or omitted means 'send everything' — don't pass an
    empty query param to the router (avoids `?since_step=0` noise)."""
    captured: dict[str, Any] = {}

    def _capture(method: str, path: str, *, json: Any = None, params: dict | None = None) -> httpx.Response:
        captured["params"] = params
        return _mock_response(200, body={"phase": "running"})

    with mock.patch(
        "kars_runtime_hermes.plugin.router_client.call",
        side_effect=_capture,
    ):
        handoff._kars_handoff_status({}, ctx=None)
    assert captured["params"] is None


# ── request: direction normalization ────────────────────────────────


@pytest.mark.parametrize(
    "user_value,wire_value",
    [
        ("cloud", "local_to_aks"),
        ("CLOUD", "local_to_aks"),
        (" cloud ", "local_to_aks"),
        ("local_to_aks", "local_to_aks"),
        ("local", "aks_to_local"),
        ("aks_to_local", "aks_to_local"),
        ("to_local", "aks_to_local"),
    ],
)
def test_request_direction_normalization(
    user_value: str, wire_value: str
) -> None:
    """Accept the LLM-friendly 'cloud' / 'local' aliases and normalize
    to the router's canonical `local_to_aks` / `aks_to_local` (same
    mapping the OpenClaw plugin does at agt.ts line 1806)."""
    captured: dict[str, Any] = {}

    def _capture(method: str, path: str, *, json: Any = None, params: dict | None = None) -> httpx.Response:
        captured["json"] = json
        return _mock_response(200, body={"status": "pending"})

    with mock.patch(
        "kars_runtime_hermes.plugin.router_client.call",
        side_effect=_capture,
    ):
        handoff._kars_handoff_request({"direction": user_value}, ctx=None)
    assert captured["json"]["direction"] == wire_value


def test_request_unknown_direction_rejected() -> None:
    """A typo in direction returns a structured error instead of
    forwarding garbage to the router. Mirrors the OpenClaw behaviour."""
    result = handoff._kars_handoff_request({"direction": "sideways"}, ctx=None)
    parsed = json.loads(result)
    assert parsed["status"] == "error"
    assert "sideways" in parsed["error"]


def test_request_default_reason() -> None:
    """When reason is omitted, default to 'user_requested' (mirrors
    OpenClaw — agent doesn't have to think about it)."""
    captured: dict[str, Any] = {}

    def _capture(method: str, path: str, *, json: Any = None, params: dict | None = None) -> httpx.Response:
        captured["json"] = json
        return _mock_response(200, body={"status": "pending"})

    with mock.patch(
        "kars_runtime_hermes.plugin.router_client.call",
        side_effect=_capture,
    ):
        handoff._kars_handoff_request({"direction": "cloud"}, ctx=None)
    assert captured["json"]["reason"] == "user_requested"


# ── confirm: token plumbing ─────────────────────────────────────────


def test_confirm_missing_token_returns_error() -> None:
    """Without a confirmation_token we never reach the router — the
    user hasn't typed the code yet and the LLM needs to ask again."""
    result = handoff._kars_handoff_confirm({}, ctx=None)
    parsed = json.loads(result)
    assert parsed["status"] == "error"
    assert "confirmation_token" in parsed["error"]


def test_confirm_forwards_token() -> None:
    """When the token is present, post it to the router and forward the
    response verbatim. The orchestration kicks off server-side."""
    captured: dict[str, Any] = {}

    def _capture(method: str, path: str, *, json: Any = None, params: dict | None = None) -> httpx.Response:
        captured["method"] = method
        captured["path"] = path
        captured["json"] = json
        return _mock_response(
            200,
            body={
                "status": "confirmed",
                "handoff_token": "tok-abc",
                "direction": "local_to_aks",
            },
        )

    with mock.patch(
        "kars_runtime_hermes.plugin.router_client.call",
        side_effect=_capture,
    ):
        result = handoff._kars_handoff_confirm(
            {"confirmation_token": "ABCD-1234"}, ctx=None
        )
    assert captured["method"] == "POST"
    assert captured["path"] == "/agt/handoff/confirm"
    assert captured["json"] == {"confirmation_token": "ABCD-1234"}
    parsed = json.loads(result)
    assert parsed["status"] == "confirmed"
    assert parsed["handoff_token"] == "tok-abc"


# ── error propagation ──────────────────────────────────────────────


def test_router_unreachable_returns_structured_error() -> None:
    """Any transport-level exception (router down, connect refused,
    timeout) surfaces as JSON instead of bubbling a Python traceback
    into the LLM context — the LLM can then tell the user the handoff
    backend is unavailable."""
    with mock.patch(
        "kars_runtime_hermes.plugin.router_client.call",
        side_effect=httpx.ConnectError("connection refused"),
    ):
        for fn, args in [
            (handoff._kars_handoff_status, {}),
            (handoff._kars_handoff_request, {"direction": "cloud"}),
            (handoff._kars_handoff_confirm, {"confirmation_token": "x"}),
        ]:
            parsed = json.loads(fn(args, ctx=None))
            assert "error" in parsed
            assert "connection refused" in str(parsed.get("error", ""))
