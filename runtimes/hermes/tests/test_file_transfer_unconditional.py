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
        self._tool_inbox = asyncio.Queue()

    def is_plaintext_peer(self, did):
        return False

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


def test_task_request_runs_inprocess_agent(tmp_path, monkeypatch):
    """A delivered task_request runs the IN-PROCESS Hermes agent (no
    subprocess) — the single-process model that lets kars_mesh_send reuse the
    one MeshClient. Free-form (non-task) chat is NOT executed here (it goes to
    the tool inbox), matching OpenClaw."""
    incoming = tmp_path / "incoming"
    monkeypatch.setenv("KARS_INCOMING_DIR", str(incoming))
    monkeypatch.setenv("KARS_MESH_WORKER_TIMEOUT_S", "5")

    ran = {"prompts": []}

    def _fake_agent(prompt):
        ran["prompts"].append(prompt)
        return "ok", True

    monkeypatch.setattr(mesh_worker, "_run_hermes_agent_inprocess", _fake_agent)

    payload = json.dumps(
        {"type": "task_request", "content": "please process"}
    ).encode()
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

    assert ran["prompts"] == ["please process"], (
        "task_request must run the in-process agent with the objective"
    )
    assert client.sent, "a task_response reply must be sent"


def test_file_transfer_unwraps_openclaw_task_request_envelope(tmp_path, monkeypatch):
    incoming = tmp_path / "incoming"
    monkeypatch.setenv("KARS_INCOMING_DIR", str(incoming))
    monkeypatch.setenv("KARS_MESH_AUTO_RESPONDER", "0")

    inner = _envelope("oc-wrapped.txt", b"unwrapped successfully")
    outer = json.dumps({"type": "task_request", "content": inner})
    msg = _FakeMsg(outer.encode())
    client = _StubClient()

    # task_request now runs the in-process agent; stub it so the test doesn't
    # need the hermes_cli runtime. The file must still be saved (the unwrap +
    # save happens before execution).
    monkeypatch.setattr(
        mesh_worker, "_run_hermes_agent_inprocess", lambda _p: ("ok", True)
    )
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
