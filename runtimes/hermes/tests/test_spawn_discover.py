"""Unit tests for kars_spawn family + kars_discover."""

from __future__ import annotations

import json
from typing import Any
from unittest import mock

import httpx
import pytest

from kars_runtime_hermes.plugin import discover, spawn


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


# ── name validation ─────────────────────────────────────────────────


@pytest.mark.parametrize(
    "name,valid",
    [
        ("auditor", True),
        ("writer-bot", True),
        ("a", True),
        ("a-b", True),
        ("123auditor", False),  # must start with letter
        ("auditor-", False),  # must end alphanumeric
        ("Auditor", False),  # uppercase rejected
        ("audit_bot", False),  # underscore rejected
        ("a" * 64, False),  # >63 chars
        ("", False),
    ],
)
def test_validate_name(name: str, valid: bool) -> None:
    err = spawn._validate_name(name)
    if valid:
        assert err is None, f"name {name!r} should be valid but: {err}"
    else:
        assert err is not None, f"name {name!r} should be invalid"


# ── kars_spawn ───────────────────────────────────────────────────────


def test_spawn_rejects_invalid_name() -> None:
    result = spawn._kars_spawn({"name": "Bad_Name"})
    parsed = json.loads(result)
    assert "error" in parsed


def test_spawn_calls_router_with_validated_body() -> None:
    captured: dict[str, Any] = {}

    def fake_call_json(method: str, path: str, **kwargs: Any) -> dict[str, Any]:
        captured["method"] = method
        captured["path"] = path
        captured["body"] = kwargs.get("json")
        return {"status": "created", "agent_id": "writer"}

    def fake_call(method: str, path: str, **_kwargs: Any) -> httpx.Response:
        # Status polling — return Running immediately so the loop exits
        return _mock_response(200, {"phase": "Running"})

    with (
        mock.patch.object(spawn.router_client, "call_json", side_effect=fake_call_json),
        mock.patch.object(spawn.router_client, "call", side_effect=fake_call),
        mock.patch.object(spawn.time, "sleep", lambda _s: None),
    ):
        result = spawn._kars_spawn({"name": "writer", "model": "gpt-4o", "role": "writer"})

    assert captured["method"] == "POST"
    assert captured["path"] == "/sandbox/spawn"
    body = captured["body"]
    assert body["agent_id"] == "writer"
    assert body["model"] == "gpt-4o"
    assert body["trust_threshold"] == 500
    assert body["governance"] is True

    parsed = json.loads(result)
    assert parsed["phase"] == "Running"
    # Make sure we surface the mesh-not-available message
    assert "Act 2" in parsed.get("message", "") or "Memory Store" in parsed.get("message", "")


def test_spawn_dev_profile_injects_learn_egress(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("KARS_DEV_PROFILE", "true")
    captured: dict[str, Any] = {}

    def fake_call_json(method: str, path: str, **kwargs: Any) -> dict[str, Any]:
        captured["body"] = kwargs.get("json")
        return {"status": "created"}

    def fake_call(method: str, path: str, **_kwargs: Any) -> httpx.Response:
        return _mock_response(200, {"phase": "Running"})

    with (
        mock.patch.object(spawn.router_client, "call_json", side_effect=fake_call_json),
        mock.patch.object(spawn.router_client, "call", side_effect=fake_call),
        mock.patch.object(spawn.time, "sleep", lambda _s: None),
    ):
        spawn._kars_spawn({"name": "child"})

    assert captured["body"]["learn_egress"] is True


def test_spawn_returns_warning_if_not_running(monkeypatch: pytest.MonkeyPatch) -> None:
    def fake_call_json(*_args: Any, **_kwargs: Any) -> dict[str, Any]:
        return {"status": "created"}

    def fake_call(*_args: Any, **_kwargs: Any) -> httpx.Response:
        return _mock_response(200, {"phase": "Pending"})

    # Mock monotonic to make deadline pass immediately
    monkeypatch.setattr(spawn.time, "sleep", lambda _s: None)
    times = iter([0.0, 100.0, 100.0])
    monkeypatch.setattr(spawn.time, "monotonic", lambda: next(times))

    with (
        mock.patch.object(spawn.router_client, "call_json", side_effect=fake_call_json),
        mock.patch.object(spawn.router_client, "call", side_effect=fake_call),
    ):
        result = spawn._kars_spawn({"name": "slow-child"})

    parsed = json.loads(result)
    assert parsed["phase"] == "Pending"
    assert "warning" in parsed


# ── kars_spawn_status ────────────────────────────────────────────────


def test_spawn_status_404_returns_not_found() -> None:
    with mock.patch.object(
        spawn.router_client,
        "call",
        return_value=_mock_response(404, text="not found"),
    ):
        result = spawn._kars_spawn_status({"name": "ghost"})
    parsed = json.loads(result)
    assert "not found" in parsed["error"].lower()


def test_spawn_status_returns_phase() -> None:
    with mock.patch.object(
        spawn.router_client,
        "call",
        return_value=_mock_response(200, {"phase": "Running", "namespace": "kars-x"}),
    ):
        result = spawn._kars_spawn_status({"name": "child"})
    parsed = json.loads(result)
    assert parsed["phase"] == "Running"


# ── kars_spawn_destroy ───────────────────────────────────────────────


def test_spawn_destroy_calls_delete() -> None:
    captured: list[tuple[str, str]] = []

    def fake_call(method: str, path: str, **_kwargs: Any) -> httpx.Response:
        captured.append((method, path))
        return _mock_response(200, {"deleted": True})

    with mock.patch.object(spawn.router_client, "call", side_effect=fake_call):
        result = spawn._kars_spawn_destroy({"name": "auditor"})

    parsed = json.loads(result)
    assert parsed["deleted"] == "auditor"
    # Sandbox delete + trust cleanup
    methods = [c[0] for c in captured]
    paths = [c[1] for c in captured]
    assert "DELETE" in methods
    assert "/sandbox/auditor" in paths
    assert "/agt/trust/auditor" in paths


def test_spawn_destroy_404_returns_warning() -> None:
    with mock.patch.object(
        spawn.router_client,
        "call",
        return_value=_mock_response(404, text="not found"),
    ):
        result = spawn._kars_spawn_destroy({"name": "ghost"})
    parsed = json.loads(result)
    assert "already gone" in parsed.get("warning", "")


# ── kars_spawn_list ──────────────────────────────────────────────────


def test_spawn_list_returns_array() -> None:
    with mock.patch.object(
        spawn.router_client,
        "call_json",
        return_value={"sub_agents": [{"agent_id": "a"}, {"agent_id": "b"}]},
    ):
        result = spawn._kars_spawn_list({})
    parsed = json.loads(result)
    assert len(parsed["sub_agents"]) == 2


# ── register() ───────────────────────────────────────────────────────


def test_spawn_register_wires_four_tools() -> None:
    ctx = _FakeCtx()
    spawn.register(ctx)
    assert set(ctx.tools.keys()) == {
        "kars_spawn",
        "kars_spawn_status",
        "kars_spawn_destroy",
        "kars_spawn_list",
    }


# ── kars_discover ────────────────────────────────────────────────────


def test_discover_requires_name_or_did() -> None:
    result = discover._kars_discover({})
    parsed = json.loads(result)
    assert "required" in parsed["error"]


def test_discover_by_did_direct_lookup() -> None:
    captured: dict[str, Any] = {}

    def fake_call(method: str, path: str, **_kwargs: Any) -> httpx.Response:
        captured["path"] = path
        return _mock_response(
            200,
            {"did": "did:mesh:abc123", "display_name": "writer", "tier": "verified"},
        )

    with mock.patch.object(discover.router_client, "call", side_effect=fake_call):
        result = discover._kars_discover({"did": "did:mesh:abc123"})

    assert "/agt/registry/v1/agents/did:mesh:abc123" in captured["path"]
    parsed = json.loads(result)
    assert parsed["agent"]["did"] == "did:mesh:abc123"


def test_discover_by_name_searches_registry() -> None:
    captured: dict[str, Any] = {}

    def fake_call(method: str, path: str, **kwargs: Any) -> httpx.Response:
        captured["path"] = path
        captured["params"] = kwargs.get("params")
        return _mock_response(200, {"agents": [{"display_name": "writer", "did": "did:mesh:x"}]})

    with mock.patch.object(discover.router_client, "call", side_effect=fake_call):
        result = discover._kars_discover({"name": "writer"})

    assert captured["path"] == "/agt/registry/v1/agents"
    assert captured["params"] == {"display_name": "writer"}
    parsed = json.loads(result)
    assert parsed["count"] == 1


def test_discover_no_matches_returns_empty() -> None:
    with mock.patch.object(
        discover.router_client,
        "call",
        return_value=_mock_response(200, {"agents": []}),
    ):
        result = discover._kars_discover({"name": "nonexistent"})
    parsed = json.loads(result)
    assert parsed["agents"] == []


def test_discover_404_did_returns_error() -> None:
    with mock.patch.object(
        discover.router_client,
        "call",
        return_value=_mock_response(404, text="not found"),
    ):
        result = discover._kars_discover({"did": "did:mesh:ghost"})
    parsed = json.loads(result)
    assert "not found" in parsed["error"].lower()


def test_discover_register_wires_single_tool() -> None:
    ctx = _FakeCtx()
    discover.register(ctx)
    assert "kars_discover" in ctx.tools
