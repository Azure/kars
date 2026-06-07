"""Regression guard: file_transfer auto-save runs even with
KARS_MESH_AUTO_RESPONDER off.

A top-level Hermes pod (no parent label -> AUTO_RESPONDER unset) was
silently losing every inbound file before this fix because the
dispatcher was short-circuited at start_worker. The contract now:
structural envelopes are infrastructure plumbing and run
unconditionally; only the LLM-spawning branch is gated.
"""

from __future__ import annotations

import asyncio
import base64
import json
from unittest import mock


from kars_runtime_hermes.plugin import mesh_worker


class _FakeMsg:
    def __init__(self, payload, from_did="did:mesh:test"):
        self.payload = payload
        self.from_did = from_did


class _StubClient:
    def __init__(self):
        self.sent = []
        self._identity = type("Id", (), {"did": "did:mesh:receiver"})()

    async def send_by_name(self, *, to, payload):
        self.sent.append((to, payload))

    async def send_by_did(self, *, to, payload):
        self.sent.append((to, payload))


def _envelope(name="f.txt", data=b"hello"):
    return json.dumps({
        "type": "file_transfer",
        "file_name": name,
        "file_path": name,
        "file_data": base64.b64encode(data).decode(),
        "size_bytes": len(data),
        "description": "regression test",
        "from_agent": "peer",
        "timestamp": "2026-06-06T22:00:00Z",
    })


def test_file_transfer_saved_when_auto_responder_off(tmp_path, monkeypatch):
    incoming = tmp_path / "incoming"
    monkeypatch.setenv("KARS_INCOMING_DIR", str(incoming))
    monkeypatch.setenv("KARS_MESH_AUTO_RESPONDER", "0")
    payload = _envelope("regression.txt", b"saved without auto-responder")
    msg = _FakeMsg(payload.encode())
    client = _StubClient()

    async def _never_spawn(*a, **kw):
        raise AssertionError("must NOT spawn hermes -z when AUTO_RESPONDER off")

    monkeypatch.setattr(asyncio, "create_subprocess_exec", _never_spawn, raising=False)
    with mock.patch(
        "kars_runtime_hermes.plugin.telemetry.submit_trust"
    ), mock.patch(
        "kars_runtime_hermes.plugin.mesh_worker._resolve_sender_name",
        new_callable=mock.AsyncMock,
        return_value="peer",
    ):
        asyncio.run(mesh_worker._handle_message(client, msg))

    saved = incoming / "regression.txt"
    assert saved.exists(), "file_transfer must save even with AUTO_RESPONDER off"
    assert saved.read_bytes() == b"saved without auto-responder"


def test_lll_response_runs_when_auto_responder_on(tmp_path, monkeypatch):
    """When AUTO_RESPONDER=1 the LLM-spawning path still runs (we
    don't accidentally short-circuit it)."""
    incoming = tmp_path / "incoming"
    monkeypatch.setenv("KARS_INCOMING_DIR", str(incoming))
    monkeypatch.setenv("KARS_MESH_AUTO_RESPONDER", "1")
    monkeypatch.setenv("KARS_MESH_WORKER_TIMEOUT_S", "5")

    spawned = {"count": 0}

    class _FakeProc:
        returncode = 0

        async def communicate(self):
            return b"ok", b""

        def kill(self):
            pass

        async def wait(self):
            return 0

    async def _fake_spawn(*a, **kw):
        spawned["count"] += 1
        return _FakeProc()

    monkeypatch.setattr(asyncio, "create_subprocess_exec", _fake_spawn, raising=False)
    payload = "plain task — please process".encode()
    msg = _FakeMsg(payload)
    client = _StubClient()
    with mock.patch(
        "kars_runtime_hermes.plugin.telemetry.submit_trust"
    ), mock.patch(
        "kars_runtime_hermes.plugin.mesh_worker._resolve_sender_name",
        new_callable=mock.AsyncMock,
        return_value="peer",
    ):
        asyncio.run(mesh_worker._handle_message(client, msg))

    assert spawned["count"] == 1, "hermes -z must spawn when AUTO_RESPONDER on"


def test_file_transfer_unwraps_openclaw_task_request_envelope(tmp_path, monkeypatch):
    incoming = tmp_path / "incoming"
    monkeypatch.setenv("KARS_INCOMING_DIR", str(incoming))
    monkeypatch.setenv("KARS_MESH_AUTO_RESPONDER", "0")

    inner = _envelope("oc-wrapped.txt", b"unwrapped successfully")
    outer = json.dumps({"type": "task_request", "content": inner})
    msg = _FakeMsg(outer.encode())
    client = _StubClient()

    async def _never_spawn(*a, **kw):
        raise AssertionError("must NOT spawn hermes -z when off")

    monkeypatch.setattr(asyncio, "create_subprocess_exec", _never_spawn, raising=False)
    with mock.patch(
        "kars_runtime_hermes.plugin.telemetry.submit_trust"
    ), mock.patch(
        "kars_runtime_hermes.plugin.mesh_worker._resolve_sender_name",
        new_callable=mock.AsyncMock,
        return_value="peer",
    ):
        asyncio.run(mesh_worker._handle_message(client, msg))

    saved = incoming / "oc-wrapped.txt"
    assert saved.exists(), "must unwrap OpenClaw task_request envelope to find file_transfer"
    assert saved.read_bytes() == b"unwrapped successfully"
