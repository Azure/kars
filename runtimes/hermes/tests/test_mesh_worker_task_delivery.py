"""Unit tests for the mesh_worker controller/principal task-delivery protocol.

These guard the single-process, in-process-agent model that makes a Hermes
agent deliver + delegate end-to-end like OpenClaw:

1. A `task_request` envelope is UNWRAPPED — the agent prompt is the objective
   `content`, not the raw JSON envelope — and executed via the IN-PROCESS Hermes
   agent (no `hermes -z` subprocess), so its kars_* tools reuse the one client.
2. The reply is a `task_response` FederationMessage (base64(json) on the wire)
   matched by the delivery waiter, carrying the real `content` + `ok` flag.
3. While the run executes, the worker ticks `task_progress` heartbeats so the
   originator's idle timeout does not kill a long run.
4. Single-owner fan-out: any inbound that is NOT a task_request (a peer's
   task_response reply, progress tick, chat) is buffered to the client's
   `_tool_inbox` for kars_mesh_inbox / kars_mesh_await — never dropped, never
   re-executed (which would loop).
"""

from __future__ import annotations

import asyncio
import json
from typing import Any

import pytest

from kars_runtime_hermes.plugin import mesh_worker

CONTROLLER_DID = "did:mesh:02b4286377b5d84d1791c2a932c2c3cd"
AGENT_DID = "did:mesh:abc123abc123abc123abc123abc12345"


class _FakeMsg:
    def __init__(self, from_did: str, payload: bytes) -> None:
        self.from_did = from_did
        self.payload = payload


class _FakeRegistry:
    def __init__(self, did: str, display_name: str) -> None:
        self._did = did
        self._display_name = display_name

    async def get_agent(self, did: str) -> Any | None:
        if did != self._did:
            return None
        return type(
            "Agent",
            (),
            {"did": self._did, "display_name": self._display_name},
        )()


class _FakeClient:
    def __init__(self, *, plaintext_dids: set[str] | None = None,
                 peer_did: str = "", peer_name: str = "") -> None:
        self._registry = _FakeRegistry(peer_did, peer_name)
        self._plaintext = plaintext_dids or set()
        self._tool_inbox: asyncio.Queue[Any] = asyncio.Queue()
        self.sent: list[tuple[str, bytes]] = []

    def is_plaintext_peer(self, did: str) -> bool:
        return did in self._plaintext

    async def send_by_name(self, *, to: str, payload: bytes) -> None:
        self.sent.append(("by_name:" + to, payload))

    async def send_by_did(self, *, to: str, payload: bytes) -> None:
        self.sent.append(("by_did:" + to, payload))


def _stub_agent(monkeypatch: pytest.MonkeyPatch,
                output: str = "the deliverable",
                ok: bool = True) -> list[str]:
    """Stub the in-process agent runner; capture the prompt(s) it receives."""
    captured_prompts: list[str] = []

    def fake_run(prompt: str) -> tuple[str, bool]:
        captured_prompts.append(prompt)
        return output, ok

    monkeypatch.setattr(mesh_worker, "_run_hermes_agent_inprocess", fake_run)
    monkeypatch.setattr(mesh_worker, "_telemetry_cursor", lambda: 0)
    monkeypatch.setattr(
        mesh_worker,
        "_telemetry_since",
        lambda _c: [
            {"kind": "round", "prompt_tokens": 100, "completion_tokens": 20,
             "total_tokens": 120},
            {"kind": "tool", "name": "kars_mesh_send"},
        ],
    )
    monkeypatch.setattr(
        "kars_runtime_hermes.plugin.telemetry.submit_trust",
        lambda **_kw: True,
    )
    return captured_prompts


@pytest.mark.asyncio
async def test_task_request_runs_inprocess_and_wraps_task_response(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("SANDBOX_NAME", "hermes-run-1")
    prompts = _stub_agent(monkeypatch, output="the deliverable")

    client = _FakeClient(plaintext_dids={CONTROLLER_DID})
    envelope = json.dumps(
        {"type": "task_request", "content": "Summarize the repo", "request_id": "r1"}
    ).encode("utf-8")
    await mesh_worker._handle_message(client, _FakeMsg(CONTROLLER_DID, envelope))

    # 1) the in-process agent got the OBJECTIVE, not the raw JSON envelope.
    assert prompts == ["Summarize the repo"]

    # 2) reply is a task_response FederationMessage sent by DID to the controller.
    assert len(client.sent) == 1
    target, payload = client.sent[0]
    assert target == "by_did:" + CONTROLLER_DID
    reply = json.loads(payload.decode("utf-8"))
    assert reply["type"] == "task_response"
    assert reply["content"] == "the deliverable"
    assert reply["ok"] is True
    assert reply["from_agent"] == "hermes-run-1"
    # Real telemetry + trace ride along so the controller scores the run as
    # substantive work (not 'low yield') and the Activity tab renders it.
    assert reply["telemetry"]["total_tokens"] == 120
    assert reply["telemetry"]["rounds"] == 1
    assert reply["telemetry"]["tool_calls"] == 1
    assert len(reply["trace"]) == 2


@pytest.mark.asyncio
async def test_task_request_failure_sets_ok_false(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _stub_agent(monkeypatch, output="", ok=False)

    client = _FakeClient(plaintext_dids={CONTROLLER_DID})
    envelope = json.dumps(
        {"type": "task_request", "content": "do X", "request_id": "r9"}
    ).encode("utf-8")
    await mesh_worker._handle_message(client, _FakeMsg(CONTROLLER_DID, envelope))

    assert len(client.sent) == 1
    reply = json.loads(client.sent[0][1].decode("utf-8"))
    assert reply["type"] == "task_response"
    assert reply["ok"] is False


@pytest.mark.asyncio
async def test_non_task_frame_buffered_to_tool_inbox_not_executed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A sub-agent's task_response reply (or any non-task frame) must be
    buffered to the tool inbox for kars_mesh_await — NOT executed (which would
    loop) and NOT dropped (which would starve a delegating principal)."""
    prompts = _stub_agent(monkeypatch, output="should-not-run")

    client = _FakeClient(peer_did=AGENT_DID, peer_name="peer-openclaw")
    reply_frame = json.dumps(
        {"type": "task_response", "content": "sub-agent result", "ok": True}
    ).encode("utf-8")
    await mesh_worker._handle_message(client, _FakeMsg(AGENT_DID, reply_frame))

    assert prompts == [], "a task_response must not trigger the agent"
    assert client.sent == [], "no reply is emitted for a buffered frame"
    # Buffered for the agent's kars_mesh_inbox / kars_mesh_await tools.
    assert client._tool_inbox.qsize() == 1
    buffered = client._tool_inbox.get_nowait()
    assert buffered.from_did == AGENT_DID


@pytest.mark.asyncio
async def test_heartbeat_loop_emits_task_progress(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(mesh_worker, "_HEARTBEAT_INTERVAL_S", 0.01)
    client = _FakeClient(plaintext_dids={CONTROLLER_DID})
    msg = _FakeMsg(from_did=CONTROLLER_DID, payload=b"")

    task = asyncio.create_task(
        mesh_worker._heartbeat_loop(client, msg, None, "hermes-run-1")
    )
    await asyncio.sleep(0.035)  # allow a few ticks
    task.cancel()
    try:
        await task
    except asyncio.CancelledError:
        pass

    assert client.sent, "heartbeat loop must emit at least one task_progress frame"
    target, payload = client.sent[0]
    assert target == "by_did:" + CONTROLLER_DID
    frame = json.loads(payload.decode("utf-8"))
    assert frame["type"] == "task_progress"
    assert frame["tick"] >= 1
    assert frame["from_agent"] == "hermes-run-1"
