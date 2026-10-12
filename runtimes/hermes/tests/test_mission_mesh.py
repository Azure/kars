# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Mission custody shares the real SDK client and fail-closed auth publication."""
import asyncio
import json
from unittest.mock import AsyncMock

import pytest

from kars_agt_mesh import MeshTransportError
from kars_agt_mesh import client as sdk
from kars_agt_mesh.messages import InboundMessage
from kars_runtime_hermes.plugin import mesh, mesh_worker
import test_mesh_auth
import test_mission_receiver
from test_mesh_auth import assert_lock
from test_mission_receiver import message

runtime = test_mesh_auth.runtime
bootstrap_env = test_mission_receiver.bootstrap_env


@pytest.mark.parametrize("handler", [mesh._kars_mesh_inbox, mesh._kars_mesh_await])
def test_mission_mode_prevents_other_tools_consuming_dispatcher_messages(runtime, monkeypatch, handler):
    monkeypatch.setenv("KARS_MISSION_DISPATCH_ENABLED", "true")
    assert "shared mesh listener" in json.loads(handler({}))["error"]
    assert not runtime.registry_factory.called and not sdk._SINGLETONS


@pytest.mark.parametrize("handler", [mesh._kars_mesh_inbox, mesh._kars_mesh_await])
@pytest.mark.parametrize("enabled", [None, "false"])
def test_nonmission_pull_tools_keep_existing_inbox_behavior(runtime, monkeypatch, handler, enabled):
    if enabled is None:
        monkeypatch.delenv("KARS_MISSION_DISPATCH_ENABLED", raising=False)
    else:
        monkeypatch.setenv("KARS_MISSION_DISPATCH_ENABLED", enabled)
    client = mesh._get_or_init_client()
    inbound = InboundMessage.new(from_did="did:mesh:" + "a" * 32, from_display_name="peer",
                                 payload=b"ordinary peer reply", message_id="ordinary-1")
    asyncio.run_coroutine_threadsafe(client._inbox.put(inbound), mesh._BACKGROUND_LOOP).result(timeout=5)
    result = json.loads(handler({"senders": ["peer"], "timeout_seconds": 1}))
    assert result["messages"][0]["message_id"] == "ordinary-1"
    assert result["messages"][0]["payload_b64"] == "b3JkaW5hcnkgcGVlciByZXBseQ=="
    assert client._inbox.empty()


def test_mission_receiver_attached_only_after_verified_singleton(runtime, bootstrap_env, monkeypatch):
    monkeypatch.setenv("KARS_MISSION_DISPATCH_ENABLED", "true")
    original_verify = runtime.verify.side_effect

    def verify(*args, **kwargs):
        client = next(iter(sdk._SINGLETONS.values()))
        assert getattr(client, "_kars_mission_receiver", None) is None
        return original_verify(*args, **kwargs)

    runtime.verify.side_effect = verify
    client = mesh._get_or_init_client()
    receiver = client._kars_mission_receiver
    assert receiver.client is client and receiver.target["agentDid"] == client._identity.did
    assert receiver.model == "mission-model"
    assert mesh._get_or_init_client() is client
    assert runtime.registry_factory.call_count == runtime.relay_factory.call_count == 1
    assert len(sdk._SINGLETONS) == 1
    lock = bootstrap_env / ".agt" / "missions" / "pod-uid" / ".mesh-prekeys.lock"
    assert_lock(lock, held=True)
    assert not (bootstrap_env / ".agt" / ".mesh-prekeys.lock").exists()

    async def probe():
        # This is the sole worker's already-decrypted inbox boundary, not another listener.
        monkeypatch.setattr(client, "send_by_did", AsyncMock())
        payload = {**receiver.target, **receiver.contract, "type": "mission:probe", "challenge": "connected",
                   "runNonce": "nonce-1", "activityFormat": "router-observations-v1"}
        await mesh_worker._handle_message(client, message(receiver, payload))
        reply = client.send_by_did.call_args.kwargs["payload"]
        assert b'"status":"ready"' in reply and b'"activityFormat":"router-observations-v1"' in reply

    asyncio.run_coroutine_threadsafe(probe(), mesh._BACKGROUND_LOOP).result(timeout=5)


@pytest.mark.parametrize("failure", ["rejected", "outage"])
def test_mission_auth_failure_never_exposes_receiver_or_holds_prekeys(runtime, bootstrap_env, monkeypatch, failure):
    monkeypatch.setenv("KARS_MISSION_DISPATCH_ENABLED", "true")
    if failure == "rejected":
        runtime.status = 403
    else:
        runtime.error = TimeoutError("registry unavailable")
    with pytest.raises(MeshTransportError):
        mesh._get_or_init_client()
    client = next(iter(sdk._SINGLETONS.values()))
    assert mesh._MESH_SINGLETON is None
    assert getattr(client, "_kars_mission_receiver", None) is None
    assert not client._is_connected
    lock = bootstrap_env / ".agt" / "missions" / "pod-uid" / ".mesh-prekeys.lock"
    assert_lock(lock, held=False)
    runtime.status, runtime.error = 200, None
    assert mesh._get_or_init_client() is client
    assert client._kars_mission_receiver.client is client
    assert len(sdk._SINGLETONS) == 1
    assert_lock(lock, held=True)


def test_explicit_kind_no_entra_keeps_same_mission_custody(runtime, bootstrap_env, monkeypatch):
    monkeypatch.setenv("KARS_MISSION_DISPATCH_ENABLED", "true")
    monkeypatch.setenv("MESH_AUTH_BACKEND", "")
    monkeypatch.setenv("AGT_SKIP_ENTRA", "1")
    client = mesh._get_or_init_client()
    assert client._kars_mission_receiver.client is client
    assert client._config.trust_threshold == 0
    assert not runtime.verify.called
    assert not runtime.relay_args[0]["entra_token"]


def test_sre_ignores_mission_bootstrap_and_stays_mesh_free(runtime, bootstrap_env, monkeypatch):
    from kars_runtime_hermes.plugin import mission_receiver

    monkeypatch.setenv("KARS_MISSION_DISPATCH_ENABLED", "true")
    monkeypatch.setenv("SRE_ENABLED", "true")
    with pytest.raises(MeshTransportError):
        mesh._get_or_init_client()
    assert mission_receiver._CUSTODY is None
    assert not runtime.registry_factory.called
    assert not sdk._SINGLETONS
