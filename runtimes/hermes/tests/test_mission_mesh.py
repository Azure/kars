# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Mission custody shares the real SDK client and fail-closed auth publication."""
import asyncio
import base64
import json
import time
from types import SimpleNamespace
from unittest.mock import AsyncMock

from agentmesh.encryption.channel import SecureChannel
from agentmesh.encryption.x3dh import X3DHKeyManager
from nacl.signing import SigningKey

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


@pytest.fixture
def installed(runtime, bootstrap_env, monkeypatch, request):
    monkeypatch.setenv("KARS_MISSION_DISPATCH_ENABLED", "true")
    # Even if inherited, this must never start a responder for ordinary peers.
    monkeypatch.setenv("KARS_MESH_AUTO_RESPONDER", "1")
    client = mesh._get_or_init_client(start_worker=True)
    peers = {}
    client._registry.find_by_display_name = AsyncMock(side_effect=lambda name: peers.get(name))
    handlers = {}
    mesh.register(SimpleNamespace(register_tool=lambda **entry: handlers.update({entry["name"]: entry["handler"]})))
    forbidden = AsyncMock(side_effect=AssertionError("ordinary messages must not enter auto-responder"))
    monkeypatch.setattr(mesh_worker, "_resolve_sender_name", forbidden)
    if not getattr(request, "param", False):
        monkeypatch.setattr(mesh_worker.asyncio, "create_subprocess_exec", forbidden)
    channels = {}

    async def deliver(payload, name="peer"):
        if name not in peers:
            key = SigningKey.generate()
            did = "did:mesh:" + bytes(key.verify_key).hex()[:32]
            peers[name] = SimpleNamespace(did=did, display_name=name)
            manager = X3DHKeyManager.from_ed25519_keys(bytes(key) + bytes(key.verify_key), bytes(key.verify_key))
            channel, establishment = SecureChannel.create_sender(
                manager, client._key_manager.get_public_bundle(), f"{did}|{client._identity.did}".encode())
            channels[name] = channel
            await client._handle_frame({"type": "knock", "from": did,
                                        "establishment": sdk._establishment_to_wire(establishment)})
        frame = sdk._encrypted_to_message_frame(
            channels[name].send(sdk._payload_to_wire_bytes(payload)), peers[name].did, client._identity.did)
        await client._handle_frame(frame)
        return frame

    def receive(payload, name="peer"):
        return asyncio.run_coroutine_threadsafe(deliver(payload, name), mesh._BACKGROUND_LOOP).result(timeout=5)

    yield SimpleNamespace(client=client, handlers=handlers, receive=receive, deliver=deliver,
                          channels=channels, forbidden=forbidden)
    if mesh_worker._WORKER_TASK is not None:
        mesh_worker._WORKER_TASK.cancel()
    forbidden.assert_not_called()


@pytest.mark.parametrize("tool", ["kars_mesh_inbox", "kars_mesh_await"])
@pytest.mark.parametrize("payload", [b"ordinary peer reply", b'{"type":"task_request","content":"peer findings"}'])
def test_native_tools_receive_encrypted_peer_payload_intact(installed, runtime, tool, payload):
    frame = installed.receive(payload)
    result = json.loads(installed.handlers[tool]({"senders": ["peer"], "timeout_seconds": 0.2}))
    assert len(result["messages"]) == 1
    assert base64.b64decode(result["messages"][0]["payload_b64"]) == payload
    assert result["messages"][0]["message_id"] == frame["id"]
    assert mesh._get_or_init_client(start_worker=True) is installed.client
    assert runtime.registry_factory.call_count == runtime.relay_factory.call_count == 1
    assert len(sdk._SINGLETONS) == 1
    assert_lock(installed.client._config.identity_path.parent / ".mesh-prekeys.lock", held=True)


@pytest.mark.parametrize("payload", [
    b'{"type":"mission:probe"}', b'{"type":"mission:assign"}', b'{"type":"mission:reply"}',
    b'{"type":"mission:unknown"}',
    b'{"type":"task_request","content":"{\\"type\\":\\"mission:assign\\"}"}',
    b'{"type":"mission:assign","type":"task_request","content":"hidden"}',
])
def test_protocol_never_exposed_to_native_tools(installed, payload):
    received = []

    async def take(msg):
        received.append(msg.payload)

    installed.client._kars_mission_receiver.receive = take
    installed.receive(payload)
    assert json.loads(installed.handlers["kars_mesh_inbox"]({}))["messages"] == []
    result = json.loads(installed.handlers["kars_mesh_await"]({"timeout_seconds": 0.03}))
    assert result == {"messages": [], "count": 0, "completed": False}
    assert received == [payload]


def test_empty_wait_times_out_and_leaves_no_message_consumer(installed):
    start = time.monotonic()
    result = json.loads(installed.handlers["kars_mesh_await"]({"timeout_seconds": 0.05}))
    assert 0.04 <= time.monotonic() - start < 1
    assert result == {"messages": [], "count": 0, "completed": False}
    installed.receive(b"after timeout")
    assert json.loads(installed.handlers["kars_mesh_inbox"]({}))["count"] == 1


def test_wait_preserves_other_senders_and_additional_replies(installed):
    installed.receive(b"other first", "other")
    installed.receive(b"wanted", "peer")
    installed.receive(b"peer second", "peer")
    installed.receive(b"other last", "other")
    result = json.loads(installed.handlers["kars_mesh_await"]({"senders": ["peer"], "timeout_seconds": 0.1}))
    assert result["completed"] and result["count"] == 1
    assert result["messages"][0]["from_display_name"] == "peer"
    remainder = json.loads(installed.handlers["kars_mesh_inbox"]({}))
    assert [base64.b64decode(m["payload_b64"]) for m in remainder["messages"]] == [
        b"other first", b"peer second", b"other last"]


@pytest.mark.parametrize("senders", [[], ["peer"]])
def test_wait_wakes_on_new_encrypted_message(installed, senders):
    if senders:
        installed.receive(b"not requested", "other")

    async def later():
        await asyncio.sleep(0.03)
        await installed.deliver(b"arrived during wait")

    asyncio.run_coroutine_threadsafe(later(), mesh._BACKGROUND_LOOP)
    result = json.loads(installed.handlers["kars_mesh_await"]({"senders": senders, "timeout_seconds": 2}))
    assert result["completed"] and result["count"] == 1
    assert result["messages"][0]["content"] == "arrived during wait"
    if senders:
        assert result["messages"][0]["from_display_name"] == "peer"
        assert installed.client._registry.find_by_display_name.await_count == 2
        remainder = json.loads(installed.handlers["kars_mesh_inbox"]({}))
        assert [m["content"] for m in remainder["messages"]] == ["not requested"]


def test_registration_retry_discards_partial_results_after_auth_suspension(installed):
    installed.receive(b"must not escape former auth", "peer")
    lookup = installed.client._registry.find_by_display_name
    missing_lookups = 0

    async def resolve(name):
        nonlocal missing_lookups
        peer = await lookup(name)
        if name == "missing":
            missing_lookups += 1
            if missing_lookups == 2:
                installed.client.connection_auth.suspend()
        return peer

    installed.client._registry.find_by_display_name = AsyncMock(side_effect=resolve)
    result = json.loads(installed.handlers["kars_mesh_await"]({
        "senders": ["peer", "missing"], "timeout_seconds": 2}))
    assert missing_lookups == 2
    assert "error" in result and result["partial"] == []
    assert [call.args[0] for call in lookup.await_args_list].count("peer") == 1


def test_unresolved_registration_lookup_obeys_original_deadline(installed):
    cancelled = []

    async def resolve(_name):
        try:
            await asyncio.sleep(10)
        finally:
            cancelled.append(True)

    installed.client._registry.find_by_display_name = AsyncMock(side_effect=resolve)
    start = time.monotonic()
    result = json.loads(installed.handlers["kars_mesh_await"]({
        "senders": ["missing"], "timeout_seconds": 0.05}))
    assert time.monotonic() - start < 1
    assert result == {"messages": [], "count": 0, "completed": False}
    assert cancelled == [True]
    installed.receive(b"after timeout")
    assert json.loads(installed.handlers["kars_mesh_inbox"]({}))["count"] == 1


def test_concurrent_sender_waits_do_not_steal_each_others_messages(installed):
    async def run():
        first = asyncio.create_task(installed.client.receive_inbox(lambda msg: msg.payload == b"first"))
        second = asyncio.create_task(installed.client.receive_inbox(lambda msg: msg.payload == b"second"))
        await asyncio.sleep(0)
        await installed.deliver(b"second", "other")
        await installed.deliver(b"first", "peer")
        messages = await asyncio.wait_for(asyncio.gather(first, second), 0.2)
        assert [msg.payload for msg in messages] == [b"first", b"second"]

    asyncio.run_coroutine_threadsafe(run(), mesh._BACKGROUND_LOOP).result(timeout=5)


def test_wait_does_not_return_buffered_messages_after_auth_suspension(installed):
    installed.receive(b"must not escape former auth", "peer")

    async def suspend():
        await asyncio.sleep(0.03)
        installed.client.connection_auth.suspend()

    asyncio.run_coroutine_threadsafe(suspend(), mesh._BACKGROUND_LOOP)
    result = json.loads(installed.handlers["kars_mesh_await"]({
        "senders": ["peer", "missing"], "timeout_seconds": 0.2}))
    assert "error" in result and result["partial"] == []


def test_partial_wait_reports_timeout_without_losing_other_peer(installed):
    installed.receive(b"first", "peer")
    installed.receive(b"not requested", "other")
    result = json.loads(installed.handlers["kars_mesh_await"]({
        "senders": ["peer", "missing"], "timeout_seconds": 0.03}))
    assert not result["completed"] and result["count"] == 1
    assert json.loads(installed.handlers["kars_mesh_inbox"]({}))["count"] == 1


def test_partition_keeps_replay_protection(installed):
    frame = installed.receive(b"once only")
    asyncio.run_coroutine_threadsafe(installed.client._handle_frame(frame), mesh._BACKGROUND_LOOP).result(timeout=5)
    assert json.loads(installed.handlers["kars_mesh_inbox"]({}))["count"] == 1


def test_peer_backlog_bounded_without_blocking_mission(installed):
    async def fill():
        for n in range(140):
            await installed.deliver(str(n).encode())

    asyncio.run_coroutine_threadsafe(fill(), mesh._BACKGROUND_LOOP).result(timeout=5)
    assert installed.client._inbox.qsize() == installed.client._inbox.maxsize == 128
    assert installed.client._reserved_inbox.maxsize == 128
    received = []

    async def take(msg):
        received.append(msg.payload)

    installed.client._kars_mission_receiver.receive = take
    installed.receive(b'{"type":"mission:probe"}')
    json.loads(installed.handlers["kars_mesh_await"]({"senders": ["absent"], "timeout_seconds": 0.03}))
    assert received == [b'{"type":"mission:probe"}']
    assert installed.client._inbox.qsize() == 128


def test_auth_suspension_clears_both_partitions_and_wakes_waiters(installed):
    async def suspend():
        client = installed.client
        pending = asyncio.create_task(client.receive_inbox())
        await asyncio.sleep(0)
        await installed.deliver(b"old peer")
        await installed.deliver(b'{"type":"mission:probe"}')
        client.connection_auth.suspend()
        assert client._inbox.empty() and client._reserved_inbox.empty()
        with pytest.raises(MeshTransportError):
            await asyncio.wait_for(pending, 0.1)

    asyncio.run_coroutine_threadsafe(suspend(), mesh._BACKGROUND_LOOP).result(timeout=5)


@pytest.mark.parametrize("timeout", [0, -1, 301, "nan", "inf"])
def test_native_wait_rejects_unbounded_deadlines(installed, timeout):
    assert "error" in json.loads(installed.handlers["kars_mesh_await"]({"timeout_seconds": timeout}))


@pytest.mark.parametrize("handler", [mesh._kars_mesh_inbox, mesh._kars_mesh_await])
@pytest.mark.parametrize("enabled", [None, "false"])
def test_nonmission_pull_tools_keep_existing_inbox_behavior(runtime, monkeypatch, handler, enabled):
    if enabled is None:
        monkeypatch.delenv("KARS_MISSION_DISPATCH_ENABLED", raising=False)
    else:
        monkeypatch.setenv("KARS_MISSION_DISPATCH_ENABLED", enabled)
    client = mesh._get_or_init_client()
    client._registry.find_by_display_name = AsyncMock(return_value=None)
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
