"""Unit tests for the mesh_worker controller task-delivery protocol.

These guard the three fixes that make a Hermes agent deliver a mission
end-to-end like OpenClaw over the plaintext control-plane channel:

1. A `task_request` envelope from the controller is UNWRAPPED — the LLM
   prompt is the objective `content`, not the raw JSON envelope.
2. The reply is a `task_response` FederationMessage (base64(json) on the
   wire), matched by the controller's task-delivery waiter, carrying the
   real `content` + `ok` flag — a raw text reply is dropped.
3. While the (potentially long) run executes, the worker ticks
   `task_progress` heartbeats so the controller's 180s idle timeout does
   not kill a run doing real work (the original Hermes failure mode).

A non-task peer chat still gets a raw (unwrapped) reply.
"""

from __future__ import annotations

import asyncio
import json
from typing import Any
from unittest import mock

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
        self.sent: list[tuple[str, bytes]] = []

    def is_plaintext_peer(self, did: str) -> bool:
        return did in self._plaintext

    async def send_by_name(self, *, to: str, payload: bytes) -> None:
        self.sent.append(("by_name:" + to, payload))

    async def send_by_did(self, *, to: str, payload: bytes) -> None:
        self.sent.append(("by_did:" + to, payload))


def _stub_subprocess(monkeypatch: pytest.MonkeyPatch,
                     stdout: bytes = b"the deliverable",
                     returncode: int = 0) -> list[list[str]]:
    """Stub asyncio.create_subprocess_exec; return a list that captures
    the argv of each invocation (so the test can assert the prompt)."""
    captured_argv: list[list[str]] = []

    async def fake_exec(*args: Any, **_kwargs: Any) -> Any:
        captured_argv.append(list(args))
        proc = mock.Mock()
        proc.returncode = returncode

        async def communicate() -> tuple[bytes, bytes]:
            return (stdout, b"")

        proc.communicate = communicate
        return proc

    monkeypatch.setattr("asyncio.create_subprocess_exec", fake_exec)
    monkeypatch.setattr(
        "kars_runtime_hermes.plugin.telemetry.submit_trust",
        lambda **_kw: True,
    )
    return captured_argv


@pytest.mark.asyncio
async def test_task_request_unwrapped_and_wrapped_as_task_response(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("KARS_MESH_AUTO_RESPONDER", "1")
    monkeypatch.setenv("SANDBOX_NAME", "hermes-run-1")
    argv = _stub_subprocess(monkeypatch, stdout=b"the deliverable")

    client = _FakeClient(plaintext_dids={CONTROLLER_DID})
    envelope = json.dumps(
        {"type": "task_request", "content": "Summarize the repo", "request_id": "r1"}
    ).encode("utf-8")
    msg = _FakeMsg(from_did=CONTROLLER_DID, payload=envelope)

    await mesh_worker._handle_message(client, msg)

    # 1) hermes -z got the OBJECTIVE, not the raw JSON envelope.
    assert argv, "subprocess must be spawned for an auto-responder task"
    assert argv[0] == ["hermes", "-z", "Summarize the repo"]

    # 2) reply is a task_response FederationMessage sent by DID to the controller.
    assert len(client.sent) == 1
    target, payload = client.sent[0]
    assert target == "by_did:" + CONTROLLER_DID
    reply = json.loads(payload.decode("utf-8"))
    assert reply["type"] == "task_response"
    assert reply["content"] == "the deliverable"
    assert reply["ok"] is True
    assert reply["from_agent"] == "hermes-run-1"


@pytest.mark.asyncio
async def test_task_request_failure_sets_ok_false(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("KARS_MESH_AUTO_RESPONDER", "1")
    _stub_subprocess(monkeypatch, stdout=b"", returncode=3)

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
async def test_non_task_peer_chat_replies_raw(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("KARS_MESH_AUTO_RESPONDER", "1")
    _stub_subprocess(monkeypatch, stdout=b"hi there")

    # A real (non-plaintext) agent peer sending free-form chat.
    client = _FakeClient(peer_did=AGENT_DID, peer_name="peer-openclaw")
    msg = _FakeMsg(from_did=AGENT_DID, payload=b"hello, how are you?")

    await mesh_worker._handle_message(client, msg)

    assert len(client.sent) == 1
    target, payload = client.sent[0]
    # Resolved to a friendly name, raw bytes (NOT a task_response envelope).
    assert target == "by_name:peer-openclaw"
    assert payload == b"hi there"


@pytest.mark.asyncio
async def test_task_request_runs_without_auto_responder(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A Hermes MISSION principal has no KARS_MESH_AUTO_RESPONDER, yet the
    controller's delivered task_request MUST still execute — otherwise the
    mission times out with 'no progress heartbeat'."""
    monkeypatch.delenv("KARS_MESH_AUTO_RESPONDER", raising=False)
    argv = _stub_subprocess(monkeypatch, stdout=b"delivered")

    client = _FakeClient(plaintext_dids={CONTROLLER_DID})
    envelope = json.dumps(
        {"type": "task_request", "content": "Do the mission", "request_id": "m1"}
    ).encode("utf-8")
    await mesh_worker._handle_message(client, _FakeMsg(CONTROLLER_DID, envelope))

    assert argv and argv[0] == ["hermes", "-z", "Do the mission"]
    assert len(client.sent) == 1
    reply = json.loads(client.sent[0][1].decode("utf-8"))
    assert reply["type"] == "task_response"
    assert reply["content"] == "delivered"


@pytest.mark.asyncio
async def test_non_task_chat_suppressed_without_auto_responder(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Free-form peer chat is still gated by the opt-in: without
    AUTO_RESPONDER the worker drains it (no LLM, no reply) so a
    channel-driven top-level agent can't loop on its own replies."""
    monkeypatch.delenv("KARS_MESH_AUTO_RESPONDER", raising=False)
    argv = _stub_subprocess(monkeypatch, stdout=b"should-not-run")

    client = _FakeClient(peer_did=AGENT_DID, peer_name="peer-openclaw")
    await mesh_worker._handle_message(client, _FakeMsg(AGENT_DID, b"just chatting"))

    assert argv == [], "chat must not spawn hermes -z when AUTO_RESPONDER is off"
    assert client.sent == [], "no reply for suppressed chat"


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
