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
