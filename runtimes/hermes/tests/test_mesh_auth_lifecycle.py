# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Real SDK/relay/lease, with only router, registry and WebSocket boundaries faked.

The socket implements pinned AGT relay/app.py's routing behavior: no connect or
heartbeat acknowledgement; authenticated frames (including self-KNOCKs) route.
"""
from __future__ import annotations

import asyncio
import base64
import json
import threading
import time
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest
from websockets.protocol import State

from kars_agt_mesh import MeshTransportError
from kars_agt_mesh import client as sdk
from kars_agt_mesh import relay_transport as transport
from kars_runtime_hermes.plugin import mesh, mesh_auth, mesh_worker
from test_mesh_auth import assert_lock, runtime  # noqa: F401

REAL_RELAY = transport.RelayTransport
CLOSED = object()


def run(coro):
    return asyncio.run_coroutine_threadsafe(coro, mesh._get_or_init_loop()).result(timeout=5)


async def eventually(predicate):
    async with asyncio.timeout(2):
        while not predicate():
            await asyncio.sleep(0.005)


def jwt(expiry):
    claims = base64.urlsafe_b64encode(json.dumps({"exp": expiry}).encode()).decode().rstrip("=")
    return f"header.{claims}.signature"


def token_body(**changes):
    return {"access_token": jwt(int(time.time()) + 3600), "expires_in": 3000,
            "token_type": "Bearer", **changes}


@pytest.fixture
def connected_runtime(runtime, monkeypatch):  # noqa: F811
    state = runtime
    state.sockets = []
    state.pending = []
    state.probe = "echo"
    state.open_error = None
    state.block_close = None
    state.close_error = None
    state.before_echo = None
    state.before_verify = None

    class Socket:
        def __init__(self):
            self.frames = []
            self.queue = asyncio.Queue()
            self.closed = False

        @property
        def state(self):
            return State.CLOSED if self.closed else State.OPEN

        async def send(self, raw):
            frame = json.loads(raw)
            self.frames.append(frame)
            if frame["type"] == "connect":
                for pending in state.pending:
                    self.queue.put_nowait(pending)
            elif frame["type"] == "knock" and frame.get("intent") == {"action": "kars_auth_probe"}:
                if state.before_echo:
                    state.before_echo()
                if state.probe == "echo":
                    self.queue.put_nowait(json.dumps(frame))
                elif state.probe == "error":
                    self.queue.put_nowait(json.dumps({"type": "error", "error": "rejected"}))
                elif state.probe == "forged":
                    self.queue.put_nowait(json.dumps({**frame, "id": "not-the-nonce"}))
                    await self.close()
                elif state.probe == "binary":
                    self.queue.put_nowait(b"invalid")
                elif state.probe == "malformed":
                    self.queue.put_nowait("{")

        async def recv(self):
            value = await self.queue.get()
            if value is CLOSED:
                raise OSError("relay closed before authentication")
            return value

        def __aiter__(self):
            return self

        async def __anext__(self):
            value = await self.queue.get()
            if value is CLOSED:
                raise StopAsyncIteration
            return value

        async def close(self):
            if state.close_error is not None:
                raise state.close_error
            if state.block_close is not None:
                await state.block_close.wait()
            self.closed = True
            self.queue.put_nowait(CLOSED)

    class Connection:
        async def __aenter__(self):
            if state.open_error is not None:
                raise state.open_error
            self.socket = Socket()
            state.sockets.append(self.socket)
            return self.socket

        async def __aexit__(self, *_):
            await self.socket.close()

    def verify(*_, **__):
        if state.before_verify:
            state.before_verify()
        if state.error is not None:
            raise state.error
        return SimpleNamespace(status_code=state.status, json=state.verified_body)

    state.verify.side_effect = verify
    def real_relay(**kwargs):
        relay = REAL_RELAY(**kwargs)
        relay._reconnect_initial = 0.001
        relay._auth_timeout = 0.2
        return relay

    monkeypatch.setattr(transport, "RelayTransport", real_relay)
    state.open = Mock(side_effect=lambda *a, **kw: Connection())
    monkeypatch.setattr(transport.websockets, "connect", state.open)
    yield state
    state.close_error = None
    if state.block_close is not None:
        async def release_close():
            state.block_close.set()
        run(release_close())


def test_required_publication_waits_for_real_relay_roundtrip_and_canonical_verify(connected_runtime):
    state = connected_runtime

    def unavailable():
        client = next(iter(sdk._SINGLETONS.values()))
        assert mesh._MESH_SINGLETON is None
        assert not client._relay.is_connected
        with pytest.raises(MeshTransportError):
            client.connection_auth.check()

    state.before_echo = unavailable
    state.before_verify = unavailable
    client = mesh._get_or_init_client()
    assert client._relay.is_connected
    state.mint.assert_called_once()
    state.verify.assert_called_once()
    frames = state.sockets[0].frames
    assert [f["type"] for f in frames] == ["connect", "knock"]
    assert frames[0]["token"] == state.token
    assert frames[1]["from"] == frames[1]["to"] == client._identity.did
    assert "connect_ack" not in json.dumps(frames)
    assert_lock(state.lock_path, held=True)


@pytest.mark.parametrize("failure", ["error", "forged", "binary", "malformed", "open", "timeout"])
def test_relay_authentication_rejection_never_publishes_and_releases_writer(connected_runtime, failure):
    state = connected_runtime
    state.probe = failure
    if failure == "open":
        state.open_error = OSError("relay unavailable")
    with pytest.raises(MeshTransportError):
        mesh._get_or_init_client()
    assert mesh._MESH_SINGLETON is None
    assert not state.verify.called
    assert all(s.closed for s in state.sockets)
    assert_lock(state.lock_path, held=False)


def test_pending_inbox_is_buffered_until_verification_probe_never_reaches_client(connected_runtime, monkeypatch):
    state = connected_runtime
    pending = {"type": "knock", "id": "offline-message", "from": "peer"}
    state.pending = [json.dumps(pending)]
    delivered = AsyncMock()
    monkeypatch.setattr(sdk.MeshClient, "_handle_frame", delivered)
    state.before_verify = lambda: delivered.assert_not_called()
    mesh._get_or_init_client()
    run(eventually(lambda: delivered.await_count == 1))
    assert delivered.call_args.args == (pending,)


@pytest.mark.parametrize("count,size", [(129, 10), (1, 1024 * 1024)])
def test_preverification_backlog_is_bounded(connected_runtime, count, size):
    state = connected_runtime
    state.pending = [json.dumps({"type": "knock", "payload": "x" * size})] * count
    with pytest.raises(MeshTransportError):
        mesh._get_or_init_client()
    assert mesh._MESH_SINGLETON is None and not state.verify.called
    assert_lock(state.lock_path, held=False)


@pytest.mark.parametrize("body", [
    None, {}, token_body(access_token="static-token"), token_body(access_token="a.b.c"),
    token_body(access_token="a.%%%.c"), token_body(access_token=jwt(0)),
    token_body(access_token=jwt(True)), token_body(access_token=jwt(int(time.time()) + 20)),
    token_body(expires_in=0), token_body(expires_in=True), token_body(expires_in="300"),
    token_body(expires_in=86401), token_body(token_type="Basic"), token_body(token_type=None),
])
def test_invalid_mint_never_registers_or_opens_relay(connected_runtime, body):
    state = connected_runtime
    state.mint.return_value = SimpleNamespace(status_code=200, json=lambda: body)
    with pytest.raises(MeshTransportError, match="invalid token lifetime"):
        mesh._get_or_init_client()
    assert not state.registry_factory.called and not state.open.called
    assert mesh._MESH_SINGLETON is None


def test_lifetime_is_earlier_of_jwt_and_router_ttl():
    token, lifetime = mesh_auth.token_lifetime(token_body(access_token=jwt(1100), expires_in=500), 1000)
    assert token == jwt(1100) and lifetime == 70
    assert mesh_auth.token_lifetime(token_body(access_token=jwt(2000), expires_in=100), 1000)[1] == 70


@pytest.mark.parametrize("body", [
    {}, {"tier": "verified"},
    {"did": "wrong", "tier": "verified", "verified_app_id": "app", "verified_tenant_id": "tenant"},
    {"did": "DID", "tier": "anonymous", "verified_app_id": "app", "verified_tenant_id": "tenant"},
    {"did": "DID", "tier": "verified", "verified_app_id": " ", "verified_tenant_id": "tenant"},
    {"did": "DID", "tier": "verified", "verified_app_id": "app", "verified_tenant_id": "tenant", "error": "rejected"},
])
def test_canonical_verification_requires_identity_not_http_success(body):
    with pytest.raises(MeshTransportError):
        mesh_auth.verified_identity(body, "DID")


def test_successful_renewal_retains_client_identity_ratchets_and_prekey_writer(connected_runtime):
    state = connected_runtime
    client = mesh._get_or_init_client()
    relay, fd, keys, channels = client._relay, client._prekey_lock_fd, client._key_manager, client._channels
    relay._reconnect_initial = 0.001

    async def renew():
        auth = client.connection_auth
        generation = auth.generation
        auth.deadline = time.monotonic() + 0.2
        auth.wall_deadline = time.time() + 0.2
        auth._arm_expiry()
        auth.renewal.cancel()
        auth.renewal = asyncio.create_task(auth._renew(generation))
        await eventually(lambda: len(state.sockets) == 2 and auth.ready)
        assert auth.generation > generation and not auth.closed
        auth.check()

    run(renew())
    assert mesh._get_or_init_client() is client
    assert client._relay is relay and client._prekey_lock_fd == fd
    assert client._key_manager is keys and client._channels is channels
    assert state.mint.call_count == state.verify.call_count == 2
    state.registries[0].register_self.assert_awaited_once()
    state.registries[0].upload_prekeys.assert_awaited_once()
    assert state.sockets[0].closed
    assert_lock(state.lock_path, held=True)


def test_remote_close_reauthenticates_same_transport_before_accepting_frames(connected_runtime):
    state = connected_runtime
    client = mesh._get_or_init_client()

    async def reconnect():
        await state.sockets[0].close()
        await eventually(lambda: len(state.sockets) == 2 and client.connection_auth.ready)
        assert client._relay.is_connected

    run(reconnect())
    assert state.mint.call_count == state.verify.call_count == 2
    assert mesh._MESH_SINGLETON is client
    assert_lock(state.lock_path, held=True)


@pytest.mark.parametrize("failure", ["mint", "verify", "socket", "probe"])
def test_reconnect_outage_unpublishes_cancels_and_tears_down(connected_runtime, failure):
    state = connected_runtime
    client = mesh._get_or_init_client()
    old_auth = client.connection_auth
    if failure == "mint":
        state.mint.side_effect = OSError("mint outage")
    elif failure == "verify":
        state.status = 403
    elif failure == "socket":
        state.open_error = OSError("socket outage")
    else:
        state.probe = "error"

    async def fail():
        with pytest.raises(MeshTransportError):
            await client._relay.reconnect()
        await eventually(lambda: old_auth.closed and client._prekey_lock_fd is None)
        with pytest.raises(MeshTransportError):
            await client.send_by_name(to="peer", payload=b"must not escape")
        assert mesh._MESH_SINGLETON is None

    run(fail())
    assert all(s.closed for s in state.sockets)
    assert_lock(state.lock_path, held=False)


@pytest.mark.parametrize("clock", ["monotonic", "wall", "timer"])
def test_expiry_is_fail_closed_even_on_retained_client_reference(connected_runtime, clock):
    state = connected_runtime
    client = mesh._get_or_init_client()
    auth = client.connection_auth

    async def expire():
        if clock == "timer":
            auth.deadline = time.monotonic() + 0.02
            auth._arm_expiry()
        else:
            if clock == "monotonic":
                auth.deadline = time.monotonic() - 1
            else:
                auth.wall_deadline = time.time() - 1
            with pytest.raises(MeshTransportError):
                await client.send_by_did(to="peer", payload=b"expired")
        await eventually(lambda: auth.closed and client._prekey_lock_fd is None)
        with pytest.raises(MeshTransportError):
            await anext(client.inbox())

    run(expire())
    assert mesh._MESH_SINGLETON is None and state.sockets[0].closed
    assert_lock(state.lock_path, held=False)


def test_late_mint_thread_cannot_publish_after_timeout(connected_runtime, monkeypatch):
    state = connected_runtime
    release = threading.Event()
    entered = threading.Event()
    returned = threading.Event()
    response = state.mint.return_value

    def slow():
        entered.set()
        release.wait(2)
        returned.set()
        return response

    state.mint.side_effect = slow
    monkeypatch.setattr(mesh_auth, "REQUEST_TIMEOUT", 0.03)
    try:
        with pytest.raises(MeshTransportError):
            mesh._get_or_init_client()
        assert entered.is_set() and mesh._MESH_SINGLETON is None
        auth = next(iter(sdk._SINGLETONS.values())).connection_auth
        assert auth.closed and auth.token is None
    finally:
        release.set()
    assert returned.wait(2)
    assert not state.open.called and not state.registry_factory.called
    assert mesh._MESH_SINGLETON is None and auth.token is None


def test_late_verify_response_cannot_restore_stopped_epoch(connected_runtime):
    state = connected_runtime
    client = mesh._get_or_init_client()
    auth = client.connection_auth
    entered, release = threading.Event(), threading.Event()

    def blocked(*_, **__):
        entered.set()
        release.wait(2)
        return SimpleNamespace(status_code=200, json=state.verified_body)

    state.verify.side_effect = blocked

    async def verify():
        auth.suspend()
        await auth.mint_initial()
        pending = asyncio.create_task(auth.verify_relay())
        await eventually(entered.is_set)
        auth.stop()
        release.set()
        with pytest.raises(MeshTransportError):
            await pending
        assert not auth.ready and auth.closed and auth.token is None

    try:
        run(verify())
    finally:
        release.set()


def test_shutdown_is_bounded_quarantines_writer_and_allows_only_finished_cleanup_retry(connected_runtime, monkeypatch):
    state = connected_runtime
    client = mesh._get_or_init_client()
    auth = client.connection_auth
    monkeypatch.setattr(mesh_auth, "CLEANUP_TIMEOUT", 0.02)

    async def block():
        state.block_close = asyncio.Event()

    run(block())
    started = time.monotonic()
    mesh.shutdown_mesh()
    assert time.monotonic() - started < 1
    assert auth.closed and mesh._MESH_SINGLETON is None
    assert mesh._MESH_PENDING_CLEANUP is client
    assert_lock(state.lock_path, held=True)
    with pytest.raises(MeshTransportError, match="teardown pending"):
        mesh._get_or_init_client()
    assert state.mint.call_count == 1

    async def release():
        state.block_close.set()
        await eventually(lambda: client._prekey_lock_fd is None)
        state.block_close = None

    run(release())
    assert_lock(state.lock_path, held=False)
    assert mesh._get_or_init_client() is client
    assert client.connection_auth is not auth
    assert state.mint.call_count == 2


@pytest.mark.parametrize("phase", ["startup", "shutdown"])
def test_failed_socket_close_retains_writer_until_confirmed_cleanup(connected_runtime, phase):
    state = connected_runtime
    if phase == "startup":
        state.probe = "error"
        state.close_error = OSError("socket teardown failed")
        with pytest.raises(MeshTransportError):
            mesh._get_or_init_client()
        client = next(iter(sdk._SINGLETONS.values()))
    else:
        client = mesh._get_or_init_client()
        state.close_error = OSError("socket teardown failed")
        mesh.shutdown_mesh()
    assert client.connection_auth.closed
    assert mesh._MESH_SINGLETON is None and mesh._MESH_PENDING_CLEANUP is client
    assert not state.sockets[0].closed
    assert_lock(state.lock_path, held=True)
    with pytest.raises(MeshTransportError, match="teardown"):
        mesh._get_or_init_client()
    assert state.open.call_count == state.mint.call_count == 1

    state.close_error = None
    state.probe = "echo"
    assert mesh._get_or_init_client() is client
    assert state.sockets[0].closed
    assert state.open.call_count == state.mint.call_count == 2
    assert_lock(state.lock_path, held=True)


def test_cancel_resistant_handler_keeps_prekey_lock_until_it_exits(connected_runtime, monkeypatch):
    state = connected_runtime
    client = mesh._get_or_init_client()
    monkeypatch.setattr(mesh_auth, "CLEANUP_TIMEOUT", 0.02)

    async def shutdown():
        release = asyncio.Event()
        entered = asyncio.Event()

        async def stubborn():
            entered.set()
            while not release.is_set():
                try:
                    await release.wait()
                except asyncio.CancelledError:
                    pass

        task = asyncio.create_task(stubborn())
        client.connection_auth.track(task)
        try:
            await entered.wait()
            client.connection_auth.fail()
            await eventually(lambda: client.connection_auth.failure_task.done())
            assert mesh._MESH_SINGLETON is None
            assert_lock(state.lock_path, held=True)
        finally:
            release.set()
            await task
        await mesh_auth.bounded_cleanup(client)

    run(shutdown())
    assert_lock(state.lock_path, held=False)


def test_reconnect_cancels_old_work_but_preserves_listener_and_discards_old_queue(connected_runtime):
    client = mesh._get_or_init_client()

    async def reconnect():
        handler = asyncio.create_task(asyncio.Event().wait())
        listener = asyncio.create_task(asyncio.Event().wait())
        auth = client.connection_auth
        auth.track(handler)
        auth.track(listener, listener=True)
        stamp = client._auth_stamp()
        client._inbox.put_nowait((stamp, SimpleNamespace(payload=b"stale")))
        await client._relay.reconnect()
        await eventually(handler.done)
        assert handler.cancelled() and not listener.done() and client._inbox.empty()
        with pytest.raises(MeshTransportError):
            client._check_auth(stamp)
        client._inbox.put_nowait((stamp, SimpleNamespace(payload=b"stale")))
        current = SimpleNamespace(payload=b"fresh")
        client._inbox.put_nowait((client._auth_stamp(), current))
        assert await anext(client.inbox()) is current
        listener.cancel()
        await asyncio.gather(listener, return_exceptions=True)

    run(reconnect())


def test_async_registry_completion_is_fenced_across_reconnect(connected_runtime):
    client = mesh._get_or_init_client()

    async def lookup():
        entered, release = asyncio.Event(), asyncio.Event()

        async def search(*_, **__):
            entered.set()
            await release.wait()
            return []

        client._registry.discover = search
        pending = asyncio.create_task(client.discover("peer"))
        await entered.wait()
        await client._relay.reconnect()
        release.set()
        with pytest.raises(MeshTransportError, match="superseded"):
            await pending

    run(lookup())


def test_worker_restarts_only_after_failed_lease_cleanup_with_same_client(connected_runtime):
    client = mesh._get_or_init_client()
    mesh_worker.start_worker(mesh._get_or_init_client)
    old_worker = mesh_worker._WORKER_TASK

    async def stop():
        await eventually(lambda: len(client.connection_auth.listeners) == 1)
        client.connection_auth.fail()
        await eventually(lambda: client._prekey_lock_fd is None and old_worker.done())

    run(stop())
    assert mesh._get_or_init_client() is client
    assert mesh_worker._WORKER_TASK is not old_worker
    run(eventually(lambda: len(client.connection_auth.listeners) == 1))


@pytest.mark.parametrize("mode", ["optional-token", "development"])
def test_optional_and_explicit_development_preserve_legacy_relay_compatibility(connected_runtime, monkeypatch, mode):
    state = connected_runtime
    monkeypatch.delenv("MESH_AUTH_BACKEND")
    if mode == "development":
        monkeypatch.setenv("AGT_SKIP_ENTRA", "1")
    client = mesh._get_or_init_client()
    assert client.connection_auth is None
    state.mint.assert_not_called()
    frames = state.sockets[0].frames
    assert [frame["type"] for frame in frames] == ["connect"]
    assert ("token" in frames[0]) is (mode == "optional-token")
    assert state.verify.call_count == int(mode == "optional-token")


@pytest.mark.parametrize("failure", ["send", "heartbeat", "server-error"])
def test_established_transport_outage_revokes_immediately(connected_runtime, failure):
    state = connected_runtime
    client = mesh._get_or_init_client()
    relay, auth = client._relay, client.connection_auth

    async def fail():
        if failure == "server-error":
            with pytest.raises(MeshTransportError):
                await relay._handle_raw(json.dumps({"type": "error", "error": "rejected"}))
        else:
            state.sockets[0].send = AsyncMock(side_effect=OSError("connection outage"))
            if failure == "send":
                with pytest.raises(MeshTransportError):
                    await relay.send_frame({"type": "message"})
            else:
                relay._heartbeat_interval = 0.001
                await relay._heartbeat_loop()
        assert auth.closed and not auth.ready and mesh._MESH_SINGLETON is None
        await eventually(lambda: client._prekey_lock_fd is None)

    run(fail())
    assert_lock(state.lock_path, held=False)


@pytest.mark.parametrize("boundary", ["trust", "execution", "reply"])
def test_generic_late_callback_never_launches_or_replies_in_new_epoch(connected_runtime, monkeypatch, boundary):
    client = mesh._get_or_init_client()
    monkeypatch.setenv("KARS_MESH_AUTO_RESPONDER", "1")
    trust = Mock()
    from kars_runtime_hermes.plugin import telemetry
    monkeypatch.setattr(telemetry, "submit_trust", trust)
    client.send_by_name = AsyncMock()
    client.send_by_did = AsyncMock()

    async def scenario():
        entered, release = asyncio.Event(), asyncio.Event()
        calls = 0

        async def stale_boundary():
            entered.set()
            while not release.is_set():
                try:
                    await release.wait()
                except asyncio.CancelledError:
                    pass

        async def resolve(*_):
            nonlocal calls
            calls += 1
            if (boundary == "trust" and calls == 1) or (boundary == "reply" and calls == 2):
                await stale_boundary()
            return "peer"

        async def communicate():
            if boundary == "execution":
                await stale_boundary()
            return b"stale completion", b""

        process = SimpleNamespace(returncode=0, communicate=communicate, wait=AsyncMock(), kill=Mock())
        spawn = AsyncMock(return_value=process)
        monkeypatch.setattr(mesh_worker, "_resolve_sender_name", resolve)
        monkeypatch.setattr(asyncio, "create_subprocess_exec", spawn)
        message = SimpleNamespace(from_did="did:mesh:peer", payload=b"hello")
        task = asyncio.create_task(mesh_worker._handle_message(client, message))
        try:
            await entered.wait()
            await client._relay.reconnect()
            release.set()
            with pytest.raises((MeshTransportError, asyncio.CancelledError)):
                await task
            if boundary == "trust":
                trust.assert_not_called()
                spawn.assert_not_called()
            client.send_by_name.assert_not_called()
            client.send_by_did.assert_not_called()
        finally:
            release.set()
            await asyncio.gather(task, return_exceptions=True)

    run(scenario())
