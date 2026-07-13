"""Unit tests for mesh_worker hooks — specifically the trust-publish
hook that surfaces inbound peers in the operator's per-sandbox AGT
panel.

Regression guard: kars commit
`fix(runtime-hermes): publish inbound peers to router trust store` —
without `submit_trust()` inside `_handle_message`, the operator's
Hermes side of an OpenClaw↔Hermes interaction shows zero peers even
though the inbox has received a decrypted message. OpenClaw's KNOCK
handler does the equivalent
(``runtimes/openclaw/src/index.ts:: pushTrustToRouter(fromName, 0.0)``)
so the two runtimes' operator views agree.
"""

from __future__ import annotations

import asyncio
import base64
import json
import time
from typing import Any
from unittest import mock

import pytest

from kars_runtime_hermes.plugin import mesh_worker


class _FakeMsg:
    def __init__(self, from_did: str, payload: bytes) -> None:
        self.from_did = from_did
        self.payload = payload


class _FakeRegistry:
    """Minimal registry stub: implements `get_agent(did)` — the
    direct DID lookup used by mesh_worker._resolve_sender_name."""

    def __init__(self, did: str, display_name: str) -> None:
        self._did = did
        self._display_name = display_name

    async def get_agent(self, did: str) -> Any | None:
        if did != self._did:
            return None
        return type(
            "Agent",
            (),
            {
                "did": self._did,
                "display_name": self._display_name,
                "capabilities": [self._display_name],
            },
        )()


class _FakeClient:
    """Minimal MeshClient stub: enough surface for _handle_message
    to resolve the sender and attempt a reply, but never actually
    spawn hermes -z."""

    def __init__(self, peer_did: str, peer_name: str) -> None:
        self._registry = _FakeRegistry(peer_did, peer_name)
        self.sent: list[tuple[str, bytes]] = []

    async def send_by_name(self, *, to: str, payload: bytes) -> None:
        self.sent.append(("by_name:" + to, payload))

    async def send_by_did(self, *, to: str, payload: bytes) -> None:
        self.sent.append(("by_did:" + to, payload))

    def is_plaintext_peer(self, did: str) -> bool:
        return did.startswith("did:controller:")


@pytest.mark.asyncio
async def test_handle_message_publishes_peer_to_router_trust_store(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An inbound message from a named peer MUST trigger a single
    `submit_trust()` call so the router's `/agt/trust` store gains a
    peer entry that the operator's per-sandbox AGT panel can render.
    Without this push, the operator shows 0 peers even after a
    successful KNOCK + decrypted MESSAGE round-trip."""

    captured: list[dict[str, Any]] = []

    def fake_submit_trust(*, agent_id: str, score: float, interactions: int = 1) -> bool:
        captured.append({
            "agent_id": agent_id,
            "score": score,
            "interactions": interactions,
        })
        return True

    monkeypatch.setattr(
        "kars_runtime_hermes.plugin.telemetry.submit_trust", fake_submit_trust
    )

    # Stub the subprocess invocation so the test doesn't try to spawn
    # `hermes -z`. We only care about the trust-publish side effect.
    async def fake_exec(*_args: Any, **_kwargs: Any) -> Any:
        fake_proc = mock.Mock()
        fake_proc.returncode = 0
        async def fake_communicate() -> tuple[bytes, bytes]:
            return (b"reply-stdout", b"")
        fake_proc.communicate = fake_communicate
        return fake_proc

    monkeypatch.setattr("asyncio.create_subprocess_exec", fake_exec)

    peer_did = "did:mesh:abc123abc123abc123abc123abc12345"
    peer_name = "test-peer-openclaw"

    client = _FakeClient(peer_did=peer_did, peer_name=peer_name)
    msg = _FakeMsg(from_did=peer_did, payload=b"hello inbound")

    await mesh_worker._handle_message(client, msg)

    assert len(captured) == 1, (
        f"_handle_message must call submit_trust() exactly once per "
        f"inbound message; got {len(captured)} call(s)"
    )
    assert captured[0]["agent_id"] == peer_name, (
        "trust entry must key on the resolved display name (so the "
        "operator panel shows a human-readable peer), not on the "
        "raw DID"
    )
    assert captured[0]["interactions"] == 1


@pytest.mark.asyncio
async def test_handle_message_falls_back_to_did_when_name_unresolvable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """If the registry has no record for the inbound peer's DID
    (transient outage, or peer using ad-hoc unregistered identity),
    publish the trust entry under the raw DID so the operator still
    sees something rather than dropping the peer silently."""

    captured: list[dict[str, Any]] = []

    def fake_submit_trust(*, agent_id: str, score: float, interactions: int = 1) -> bool:
        captured.append({"agent_id": agent_id})
        return True

    monkeypatch.setattr(
        "kars_runtime_hermes.plugin.telemetry.submit_trust", fake_submit_trust
    )

    async def fake_exec(*_args: Any, **_kwargs: Any) -> Any:
        fake_proc = mock.Mock()
        fake_proc.returncode = 0
        async def fake_communicate() -> tuple[bytes, bytes]:
            return (b"ok", b"")
        fake_proc.communicate = fake_communicate
        return fake_proc

    monkeypatch.setattr("asyncio.create_subprocess_exec", fake_exec)

    peer_did = "did:mesh:unknownunknownunknownunknown00"

    # Registry returns a DIFFERENT DID — no match → display name lookup fails
    client = _FakeClient(peer_did="did:mesh:somethingelse", peer_name="other-agent")
    msg = _FakeMsg(from_did=peer_did, payload=b"hello")

    await mesh_worker._handle_message(client, msg)

    assert captured == [{"agent_id": peer_did}], (
        f"expected fallback to raw DID, got {captured!r}"
    )


@pytest.mark.asyncio
async def test_collect_and_ship_artifacts_sends_only_task_changes(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Any,
) -> None:
    monkeypatch.setenv("KARS_HERMES_WORKSPACE_DIR", str(tmp_path))
    existing = tmp_path / "existing.txt"
    existing.write_text("before", encoding="utf-8")
    before = mesh_worker._snapshot_workspace()

    existing.write_text("after", encoding="utf-8")
    proof = tmp_path / "proof.json"
    proof.write_text('{"marker":"PASS"}', encoding="utf-8")

    peer_did = "did:controller:kars"
    client = _FakeClient(peer_did=peer_did, peer_name="controller")
    msg = _FakeMsg(from_did=peer_did, payload=b"task")
    manifest = await mesh_worker._collect_and_ship_artifacts(
        client,
        msg,
        None,
        before,
        "done",
        True,
        "request-123",
        "hermes-agent",
    )

    assert {item["path"] for item in manifest} == {"existing.txt", "proof.json"}
    assert len({item["name"] for item in manifest}) == 2
    assert len(client.sent) == 2
    frames = [json.loads(payload) for _, payload in client.sent]
    assert all(route == "by_did:" + peer_did for route, _ in client.sent)
    assert all(frame["type"] == "file_transfer" for frame in frames)
    decoded = {
        frame["file_path"]: base64.b64decode(frame["file_data"]).decode("utf-8")
        for frame in frames
    }
    assert decoded == {
        "existing.txt": "after",
        "proof.json": '{"marker":"PASS"}',
    }


@pytest.mark.asyncio
async def test_collect_and_ship_artifacts_creates_text_fallback(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Any,
) -> None:
    monkeypatch.setenv("KARS_HERMES_WORKSPACE_DIR", str(tmp_path))
    peer_did = "did:controller:kars"
    client = _FakeClient(peer_did=peer_did, peer_name="controller")
    msg = _FakeMsg(from_did=peer_did, payload=b"task")
    reply = "substantive report\n" + ("x" * 500)

    manifest = await mesh_worker._collect_and_ship_artifacts(
        client,
        msg,
        None,
        {},
        reply,
        True,
        "abcdef123456",
        "hermes-agent",
    )

    assert manifest == [
        {
            "name": mesh_worker._artifact_wire_name("task-abcdef12-report.md"),
            "path": "task-abcdef12-report.md",
            "size_bytes": len(reply.encode("utf-8")),
        }
    ]
    frame = json.loads(client.sent[0][1])
    assert base64.b64decode(frame["file_data"]).decode("utf-8") == reply


@pytest.mark.asyncio
async def test_artifact_paths_are_unique_and_symlinks_are_not_followed(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Any,
) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "secret.txt").write_text("must-not-ship", encoding="utf-8")
    (workspace / "escape").symlink_to(outside, target_is_directory=True)
    monkeypatch.setenv("KARS_HERMES_WORKSPACE_DIR", str(workspace))
    before = mesh_worker._snapshot_workspace()

    for subdir, content in (("a", "first"), ("b", "second")):
        directory = workspace / subdir
        directory.mkdir()
        (directory / "report.md").write_text(content, encoding="utf-8")

    peer_did = "did:controller:kars"
    client = _FakeClient(peer_did=peer_did, peer_name="controller")
    manifest = await mesh_worker._collect_and_ship_artifacts(
        client,
        _FakeMsg(from_did=peer_did, payload=b"task"),
        None,
        before,
        "done",
        True,
        "request-123",
        "hermes-agent",
    )

    names = [item["name"] for item in manifest]
    assert len(names) == 2
    assert len(set(names)) == 2
    frames = [json.loads(payload) for _, payload in client.sent]
    assert all("must-not-ship" not in base64.b64decode(frame["file_data"]).decode("utf-8") for frame in frames)


@pytest.mark.asyncio
async def test_artifact_failure_does_not_suppress_task_response(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def idle_heartbeat(*_args: Any, **_kwargs: Any) -> None:
        await asyncio.Event().wait()

    async def fail_artifacts(*_args: Any, **_kwargs: Any) -> list[dict[str, Any]]:
        raise OSError("read-only workspace")

    monkeypatch.setattr(mesh_worker, "_heartbeat_loop", idle_heartbeat)
    monkeypatch.setattr(mesh_worker, "_resolve_sender_name", mock.AsyncMock(return_value=None))
    monkeypatch.setattr(mesh_worker, "_telemetry_cursor", lambda: 0)
    monkeypatch.setattr(mesh_worker, "_snapshot_workspace", lambda: {})
    monkeypatch.setattr(mesh_worker, "_run_hermes_agent_inprocess", lambda _prompt: ("deliverable", True))
    monkeypatch.setattr(mesh_worker, "_telemetry_since", lambda _cursor: [])
    monkeypatch.setattr(mesh_worker, "_collect_and_ship_artifacts", fail_artifacts)

    peer_did = "did:controller:kars"
    client = _FakeClient(peer_did=peer_did, peer_name="controller")
    await mesh_worker._execute_task_request(
        client,
        _FakeMsg(from_did=peer_did, payload=b"task"),
        "objective",
        "request-123",
    )

    assert len(client.sent) == 1
    response = json.loads(client.sent[0][1])
    assert response["type"] == "task_response"
    assert response["content"] == "deliverable"
    assert response["artifacts"] == []


@pytest.mark.asyncio
async def test_timed_out_executor_is_contained_before_task_unlocks(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def idle_heartbeat(*_args: Any, **_kwargs: Any) -> None:
        await asyncio.Event().wait()

    def slow_agent(_prompt: str) -> tuple[str, bool]:
        time.sleep(0.05)
        return "late reply", True

    monkeypatch.setenv("KARS_MESH_WORKER_TIMEOUT_S", "0.01")
    monkeypatch.setattr(mesh_worker, "_heartbeat_loop", idle_heartbeat)
    monkeypatch.setattr(mesh_worker, "_resolve_sender_name", mock.AsyncMock(return_value=None))
    monkeypatch.setattr(mesh_worker, "_telemetry_cursor", lambda: 0)
    monkeypatch.setattr(mesh_worker, "_snapshot_workspace", lambda: {})
    monkeypatch.setattr(mesh_worker, "_run_hermes_agent_inprocess", slow_agent)
    monkeypatch.setattr(mesh_worker, "_telemetry_since", lambda _cursor: [])

    peer_did = "did:controller:kars"
    client = _FakeClient(peer_did=peer_did, peer_name="controller")
    started = time.monotonic()
    await mesh_worker._execute_task_request(
        client,
        _FakeMsg(from_did=peer_did, payload=b"task"),
        "objective",
        "request-123",
    )

    assert time.monotonic() - started >= 0.04
    response = json.loads(client.sent[0][1])
    assert response["ok"] is False
    assert response["content"].startswith("WORKER_TIMEOUT")


@pytest.mark.asyncio
async def test_concurrent_task_is_rejected_without_queueing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(mesh_worker, "_resolve_sender_name", mock.AsyncMock(return_value=None))
    monkeypatch.setattr(
        "kars_runtime_hermes.plugin.telemetry.submit_trust",
        lambda **_kwargs: True,
    )
    peer_did = "did:controller:kars"
    client = _FakeClient(peer_did=peer_did, peer_name="controller")
    payload = json.dumps(
        {
            "type": "task_request",
            "content": "objective",
            "request_id": "request-456",
        }
    ).encode("utf-8")

    await mesh_worker._TASK_EXECUTION_LOCK.acquire()
    try:
        await mesh_worker._handle_message(
            client,
            _FakeMsg(from_did=peer_did, payload=payload),
        )
    finally:
        mesh_worker._TASK_EXECUTION_LOCK.release()

    response = json.loads(client.sent[0][1])
    assert response["ok"] is False
    assert response["content"].startswith("WORKER_BUSY")
