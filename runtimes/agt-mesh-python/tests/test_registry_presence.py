# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Registry presence shares the real relay's authenticated heartbeat lifecycle."""

from __future__ import annotations

import asyncio
import base64
import importlib
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
import websockets
from websockets.exceptions import ConnectionClosed

from kars_agt_mesh import MeshClient, MeshConfig, MeshTransportError
from kars_agt_mesh import registry_client, relay_transport
from kars_agt_mesh.client import _SINGLETONS


@pytest.fixture(params=["legacy", "asyncio"])
def connect_api(request, monkeypatch):
    try:
        connect = importlib.import_module(f"websockets.{request.param}.client").connect
    except ModuleNotFoundError:
        pytest.skip("websockets 12 has no asyncio client implementation")
    monkeypatch.setattr(relay_transport.websockets, "connect", connect)


@pytest.fixture
async def mesh(tmp_path, monkeypatch, connect_api):
    state = SimpleNamespace(
        requests=[], heartbeats=asyncio.Queue(), connections=asyncio.Queue(), outcome=None,
    )

    async def handle(request):
        state.requests.append(request)
        if request.url.path == "/v1/agents":
            return httpx.Response(201)
        scheme, did, timestamp, signature = request.headers["Authorization"].split()
        assert scheme == "Ed25519-Timestamp"
        assert did == client._identity.did
        assert request.url.path.startswith(f"/v1/agents/{did}/")
        client._identity.signing_key.verify_key.verify(
            timestamp.encode(), base64.urlsafe_b64decode(signature + "=" * (-len(signature) % 4)),
        )
        if request.url.path.endswith("/heartbeat"):
            state.heartbeats.put_nowait(request)
            if isinstance(state.outcome, Exception):
                raise state.outcome
            if state.outcome is not None:
                await state.outcome()
        return httpx.Response(204)

    http_client = httpx.AsyncClient
    monkeypatch.setattr(
        registry_client.httpx, "AsyncClient",
        lambda **kwargs: http_client(transport=httpx.MockTransport(handle), **kwargs),
    )

    async def echo(ws):
        try:
            await ws.recv()
            await state.connections.put(ws)
            async for raw in ws:
                await ws.send(raw)
        except ConnectionClosed:
            pass

    async with websockets.serve(echo, "127.0.0.1", 0) as server:
        config = MeshConfig(
            name=tmp_path.name,
            relay_url=f"ws://127.0.0.1:{server.sockets[0].getsockname()[1]}/relay",
            registry_url="http://registry.test",
            identity_path=tmp_path / "identity.json",
            heartbeat_interval_seconds=0.01,
            reconnect_initial_seconds=0.01,
            reconnect_max_seconds=0.02,
        )
        client = MeshClient(config)
        state.client = client
        try:
            yield state
        finally:
            await client.disconnect()
            _SINGLETONS.pop((config.name, config.relay_url, config.registry_url), None)


async def next_heartbeat(mesh):
    return await asyncio.wait_for(mesh.heartbeats.get(), timeout=2)


async def test_connected_singleton_refreshes_signed_registry_presence(mesh):
    await mesh.client.connect()
    heartbeat_task = mesh.client._relay._hb_task
    await mesh.client.connect()
    assert mesh.client._relay._hb_task is heartbeat_task
    first = await next_heartbeat(mesh)
    second = await next_heartbeat(mesh)
    assert first.method == second.method == "POST"
    assert first.headers["Authorization"] != second.headers["Authorization"]
    assert sum(r.url.path == "/v1/agents" for r in mesh.requests) == 1
    assert sum(r.url.path.endswith("/prekeys") for r in mesh.requests) == 1
    assert mesh.client._prekey_lock_fd is not None


async def test_registry_outage_retries_without_reconnecting_or_exposing_response(mesh, caplog):
    mesh.outcome = httpx.ConnectError("private upstream response")
    await mesh.client.connect()
    relay = mesh.client._relay
    task = relay._hb_task
    await next_heartbeat(mesh)
    mesh.outcome = None
    await next_heartbeat(mesh)
    assert relay.is_connected
    assert relay._hb_task is task and not task.done()
    assert "Registry heartbeat failed (ConnectError)" in caplog.text
    assert "private upstream response" not in caplog.text
    assert sum(r.url.path == "/v1/agents" for r in mesh.requests) == 1


async def test_reconnect_replaces_heartbeat_without_another_prekey_writer(mesh):
    await mesh.client.connect()
    await next_heartbeat(mesh)
    old_task = mesh.client._relay._hb_task
    ws = await asyncio.wait_for(mesh.connections.get(), 2)
    await ws.close()
    await asyncio.wait_for(mesh.connections.get(), 2)
    while not mesh.heartbeats.empty():
        mesh.heartbeats.get_nowait()
    await next_heartbeat(mesh)
    assert old_task.done()
    assert mesh.client._relay._hb_task is not old_task
    assert sum(r.url.path.endswith("/prekeys") for r in mesh.requests) == 1


async def test_disconnect_cancels_inflight_presence_before_registry_close(mesh):
    cancelled = asyncio.Event()

    async def blocked():
        try:
            await asyncio.Event().wait()
        finally:
            cancelled.set()

    mesh.outcome = blocked
    await mesh.client.connect()
    registry = mesh.client._registry
    await next_heartbeat(mesh)
    await asyncio.wait_for(mesh.client.disconnect(), 2)
    assert cancelled.is_set()
    assert registry._client.is_closed
    count = len(mesh.requests)
    await asyncio.sleep(0.03)
    assert len(mesh.requests) == count
    assert mesh.client._prekey_lock_fd is None


async def test_presence_rejects_revoked_connection_before_http(mesh):
    def revoked():
        raise MeshTransportError("revoked")

    auth = SimpleNamespace(generation=1, check=revoked, shutdown=AsyncMock())
    mesh.client.connection_auth = auth
    mesh.client._registry = SimpleNamespace(heartbeat=AsyncMock(), aclose=AsyncMock())
    with pytest.raises(MeshTransportError, match="revoked"):
        await mesh.client._heartbeat_registry()
    mesh.client._registry.heartbeat.assert_not_awaited()


async def test_presence_rejects_authorization_replaced_during_http(mesh):
    auth = SimpleNamespace(generation=1, check=lambda: None, shutdown=AsyncMock())
    mesh.client.connection_auth = auth

    async def change_generation():
        auth.generation += 1

    mesh.client._registry = SimpleNamespace(
        heartbeat=AsyncMock(side_effect=change_generation), aclose=AsyncMock(),
    )
    with pytest.raises(MeshTransportError, match="superseded authorization"):
        await mesh.client._heartbeat_registry()
