"""Unit tests for the AGT governance pre_tool_call hook."""

from __future__ import annotations

from typing import Any
from unittest import mock

import httpx
import pytest

from kars_runtime_hermes.plugin import governance


def _mock_response(status: int, body: dict[str, Any]) -> httpx.Response:
    return httpx.Response(
        status_code=status,
        json=body,
        request=httpx.Request("POST", "http://127.0.0.1:8443/agt/evaluate"),
    )


@pytest.fixture(autouse=True)
def _reset_state() -> None:
    """Reset governance module state between tests."""
    governance._reset_for_testing()
    yield


# ── Action verb construction ─────────────────────────────────────────


def test_shell_action_verb_for_exec_command() -> None:
    v = governance._action_verb("exec_command", {"command": "ls -la /tmp"})
    assert v == "shell:ls -la /tmp"


def test_egress_action_verb_for_http_fetch() -> None:
    v = governance._action_verb("http_fetch", {"url": "https://example.com/x"})
    assert v == "egress:https://example.com/x"


def test_memory_action_verb_uses_operation() -> None:
    v = governance._action_verb("foundry_memory", {"operation": "UPDATE", "text": "hi"})
    assert v == "memory:update"


def test_mesh_send_action_verb_includes_target() -> None:
    v = governance._action_verb("kars_mesh_send", {"target_agent": "auditor"})
    assert v == "mesh:send:auditor"


def test_spawn_action_verb_includes_child_name() -> None:
    v = governance._action_verb("kars_spawn", {"name": "writer-bot", "role": "writer"})
    assert v == "spawn:create:writer-bot"


def test_default_action_verb_pulls_first_significant_param() -> None:
    v = governance._action_verb("foundry_web_search", {"query": "climate change"})
    assert v == "tool:foundry_web_search:climate change"


def test_action_verb_truncates_oversized_strings() -> None:
    huge = "x" * 1000
    v = governance._action_verb("exec_command", {"command": huge})
    assert len(v) <= 256
    assert v.endswith("...")


# ── Canonicalisation: secret redaction ───────────────────────────────


def test_bearer_token_redacted_in_action() -> None:
    s = "egress:https://api.example.com?token=Bearer abcdef0123456789"
    out = governance._canonicalize(s)
    # Either pattern catches it
    assert "abcdef0123456789" not in out
    assert "REDACTED" in out


def test_jwt_redacted_in_action() -> None:
    s = "egress:https://x?t=eyJabcdefghijklmnopqrstuvwxyz.eyJpayload12345.signsignsignsi"
    out = governance._canonicalize(s)
    assert "<REDACTED_JWT>" in out


def test_github_token_redacted_in_action() -> None:
    s = "egress:https://api.github.com?token=ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ1234567"
    out = governance._canonicalize(s)
    assert "ghp_aBcDeFgHiJkLmNoPqRsTuV" not in out
    assert "<REDACTED_SECRET>" in out


def test_newlines_collapsed_in_action() -> None:
    s = "shell:ls\n-la\r/tmp"
    out = governance._canonicalize(s)
    assert "\n" not in out
    assert "\r" not in out


# ── /agt/evaluate dispatch ───────────────────────────────────────────


def test_evaluate_returns_allow_on_2xx_with_allowed_true() -> None:
    with mock.patch.object(
        governance.router_client,
        "call",
        return_value=_mock_response(200, {"allowed": True, "decision": "allow"}),
    ):
        d = governance.evaluate("http_fetch", {"url": "https://example.com"})
    assert d.allowed
    assert d.decision == "allow"


def test_evaluate_returns_deny_with_reason_on_allowed_false() -> None:
    with mock.patch.object(
        governance.router_client,
        "call",
        return_value=_mock_response(
            200,
            {
                "allowed": False,
                "decision": "deny",
                "reason": "destination on blocklist",
                "matched_rule": "block-public-internet",
            },
        ),
    ):
        d = governance.evaluate("http_fetch", {"url": "https://evil.example.com"})
    assert not d.allowed
    assert d.decision == "deny"
    assert "blocklist" in (d.reason or "")
    assert d.matched_rule == "block-public-internet"


def test_evaluate_grace_period_allows_first_n_failures(monkeypatch: pytest.MonkeyPatch) -> None:
    # Default grace = 3. First two failures pass; third fails closed.
    def raise_(*_args: Any, **_kwargs: Any) -> None:
        raise httpx.ConnectError("router down")

    with mock.patch.object(governance.router_client, "call", side_effect=raise_):
        d1 = governance.evaluate("http_fetch", {"url": "x"})
        d2 = governance.evaluate("http_fetch", {"url": "x"})
        d3 = governance.evaluate("http_fetch", {"url": "x"})

    assert d1.allowed and d2.allowed  # under grace
    assert not d3.allowed  # grace exhausted → fail-closed
    assert "fail-closed" in (d3.reason or "").lower()


def test_evaluate_success_resets_failure_counter() -> None:
    """One successful call resets the failure counter to zero."""

    def raise_(*_args: Any, **_kwargs: Any) -> None:
        raise httpx.ConnectError("router down")

    with mock.patch.object(governance.router_client, "call", side_effect=raise_):
        governance.evaluate("x", {})  # failure 1
        governance.evaluate("x", {})  # failure 2

    with mock.patch.object(
        governance.router_client,
        "call",
        return_value=_mock_response(200, {"allowed": True}),
    ):
        governance.evaluate("x", {})  # success — resets

    # Now we should get 2 more grace failures (counter reset)
    with mock.patch.object(governance.router_client, "call", side_effect=raise_):
        d1 = governance.evaluate("x", {})
        d2 = governance.evaluate("x", {})
        assert d1.allowed and d2.allowed  # counter was reset


def test_evaluate_non_2xx_treated_as_failure() -> None:
    with mock.patch.object(
        governance.router_client,
        "call",
        return_value=httpx.Response(
            status_code=503,
            text="upstream timeout",
            request=httpx.Request("POST", "http://127.0.0.1:8443/agt/evaluate"),
        ),
    ):
        d1 = governance.evaluate("x", {})
        d2 = governance.evaluate("x", {})
        d3 = governance.evaluate("x", {})

    assert d1.allowed  # under grace
    assert not d3.allowed  # grace exhausted


# ── pre_tool_call hook integration ───────────────────────────────────


def test_hook_returns_none_on_allow_to_proceed() -> None:
    with mock.patch.object(
        governance.router_client,
        "call",
        return_value=_mock_response(200, {"allowed": True, "decision": "allow"}),
    ):
        result = governance._on_pre_tool_call("http_fetch", {"url": "https://example.com"})
    assert result is None


def test_hook_returns_json_error_on_deny() -> None:
    with mock.patch.object(
        governance.router_client,
        "call",
        return_value=_mock_response(
            200,
            {
                "allowed": False,
                "decision": "deny",
                "reason": "blocked",
                "matched_rule": "rule-42",
            },
        ),
    ):
        result = governance._on_pre_tool_call("http_fetch", {"url": "https://example.com"})

    import json

    assert isinstance(result, str)
    parsed = json.loads(result)
    assert "Blocked by AGT policy" in parsed["error"]
    assert parsed["kars_governance"]["rule"] == "rule-42"
    assert parsed["kars_governance"]["reason"] == "blocked"


def test_register_wires_pre_tool_call_hook() -> None:
    class FakeCtx:
        def __init__(self) -> None:
            self.hooks: dict[str, Any] = {}

        def register_hook(self, name: str, handler: Any) -> None:
            self.hooks[name] = handler

    ctx = FakeCtx()
    governance.register(ctx)
    assert "pre_tool_call" in ctx.hooks
    assert callable(ctx.hooks["pre_tool_call"])
