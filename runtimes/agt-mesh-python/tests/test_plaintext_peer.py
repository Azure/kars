# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Tests for the plaintext-peer allowlist — the kars control-plane bridge.

The kars controller does not run the Signal handshake; it sends task-delivery
frames as `plaintext: true` (JSON duplicated into `ciphertext`). Agents must
accept those frames ONLY from an allowlisted control-plane peer (the
controller's AMID) and reply in plaintext, while all agent↔agent traffic stays
E2E. This mirrors the TS SDK's plaintext-peer allowlist.
"""

from __future__ import annotations

import asyncio
import base64
import json
from pathlib import Path

from kars_agt_mesh.client import MeshClient
from kars_agt_mesh.config import MeshConfig

CTRL = "did:mesh:02b4286377b5d84d1791c2a932c2c3cd"
STRANGER = "did:mesh:deadbeefdeadbeefdeadbeefdeadbeef"


def _cfg(tmp_path: Path, peers=()) -> MeshConfig:
    return MeshConfig(
        name="hermes-agent",
        relay_url="ws://127.0.0.1:65535/agt/relay",
        registry_url="http://127.0.0.1:65535/agt/registry",
        identity_path=tmp_path / ".agt" / "identity.json",
        trust_threshold=0,
        plaintext_peers=peers,
    )


def _plaintext_frame(from_did: str, obj: dict) -> dict:
    b64 = base64.b64encode(json.dumps(obj).encode()).decode()
    return {"type": "message", "from": from_did, "id": "m1", "payload": b64, "ciphertext": b64, "plaintext": True}


def test_plaintext_from_controller_is_delivered(tmp_path: Path) -> None:
    from kars_agt_mesh.client import _SINGLETONS

    _SINGLETONS.clear()
    client = MeshClient(_cfg(tmp_path, peers=(CTRL,)))
    payload = {"type": "task_request", "content": "What is 2+2?"}
    asyncio.run(client._handle_message_frame(_plaintext_frame(CTRL, payload)))
    msg = client._inbox.get_nowait()
    assert msg.from_did == CTRL
    assert json.loads(msg.payload.decode()) == payload


def test_plaintext_from_stranger_is_dropped(tmp_path: Path) -> None:
    from kars_agt_mesh.client import _SINGLETONS

    _SINGLETONS.clear()
    client = MeshClient(_cfg(tmp_path, peers=(CTRL,)))
    asyncio.run(client._handle_message_frame(_plaintext_frame(STRANGER, {"x": 1})))
    # A plaintext frame from a non-allowlisted DID must NOT be delivered.
    assert client._inbox.empty()


def test_allowlist_methods(tmp_path: Path) -> None:
    from kars_agt_mesh.client import _SINGLETONS

    _SINGLETONS.clear()
    client = MeshClient(_cfg(tmp_path))
    assert not client.is_plaintext_peer(CTRL)
    client.add_plaintext_peer(CTRL)
    assert client.is_plaintext_peer(CTRL)
    client.remove_plaintext_peer(CTRL)
    assert not client.is_plaintext_peer(CTRL)
