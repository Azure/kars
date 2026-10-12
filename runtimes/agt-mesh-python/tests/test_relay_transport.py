# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Real loopback sockets exercise both websockets connection implementations."""

from __future__ import annotations

import asyncio
import base64
import hashlib
import importlib
import inspect
import json
import re
import tomllib
from datetime import datetime, timezone
from pathlib import Path

import pytest
import websockets
from nacl.signing import SigningKey, VerifyKey
from packaging.requirements import Requirement
from websockets.exceptions import ConnectionClosed, WebSocketException
from websockets.protocol import State

from kars_agt_mesh import MeshTransportError
from kars_agt_mesh import relay_transport


def test_hermes_and_shared_mesh_accept_the_same_websocket_versions():
    root = Path(__file__).resolve().parents[3]
    project = tomllib.loads(
        (root / "runtimes/agt-mesh-python/pyproject.toml").read_text()
    )
    requirement = next(
        Requirement(value)
        for value in project["project"]["dependencies"]
        if Requirement(value).name == "websockets"
    )
    image = (root / "sandbox-images/hermes/Dockerfile").read_text()
    image_requirements = re.findall(r'"(websockets[^"]+)"', image)
    assert len(image_requirements) == 1
    assert Requirement(image_requirements[0]).specifier == requirement.specifier
    for version in ("12.0", "13.1", "14.2", "15.0.1"):
        assert version in requirement.specifier
    for version in ("11.0", "16.0"):
        assert version not in requirement.specifier


@pytest.fixture(params=["legacy", "asyncio"])
def connect_api(request, monkeypatch):
    try:
        connect = importlib.import_module(f"websockets.{request.param}.client").connect
    except ModuleNotFoundError:
        pytest.skip("websockets 12 has no asyncio client implementation")
    monkeypatch.setattr(relay_transport.websockets, "connect", connect)
    return connect


@pytest.fixture
async def proxy_trap(monkeypatch):
    attempts = []

    async def reject_proxy(reader, writer):
        attempts.append(True)
        writer.close()
        await writer.wait_closed()

    async with await asyncio.start_server(reject_proxy, "127.0.0.1", 0) as server:
        url = f"http://127.0.0.1:{server.sockets[0].getsockname()[1]}"
        for key in ("ws_proxy", "wss_proxy", "http_proxy", "https_proxy", "all_proxy"):
            monkeypatch.setenv(key, url)
            monkeypatch.setenv(key.upper(), url)
        monkeypatch.setenv("no_proxy", "")
        monkeypatch.setenv("NO_PROXY", "")
        yield attempts


@pytest.fixture
async def loopback_relay():
    connections = asyncio.Queue()

    async def echo(ws):
        try:
            connect = json.loads(await ws.recv())
            await connections.put((ws, connect))
            async for raw in ws:
                await ws.send(raw)
        except ConnectionClosed:
            pass

    async with websockets.serve(echo, "127.0.0.1", 0) as server:
        yield f"ws://127.0.0.1:{server.sockets[0].getsockname()[1]}/agt/relay", connections


def make_relay(url, on_frame, token=None, heartbeat=60):
    key = SigningKey.generate()
    public_key = bytes(key.verify_key)
    return relay_transport.RelayTransport(
        url=url,
        identity_did=f"did:mesh:{hashlib.sha256(public_key).hexdigest()[:32]}",
        identity_signing_key=key,
        identity_public_key=public_key,
        user_agent="kars-relay-loopback-test",
        heartbeat_interval_seconds=heartbeat,
        reconnect_initial_seconds=0.01,
        reconnect_max_seconds=0.02,
        on_frame=on_frame,
        entra_token=token,
    )


def assert_connect_pop(frame, relay, token):
    assert set(frame) == {"v", "type", "from", "public_key", "timestamp", "signature"} | (
        {"token"} if token else set()
    )
    assert frame["v"] == 1 and frame["type"] == "connect"
    assert frame["from"] == relay._identity_did
    public_key = base64.b64decode(frame["public_key"], validate=True)
    assert public_key == relay._public_key
    assert frame.get("token") == token
    timestamp = frame["timestamp"]
    assert timestamp.endswith("Z")
    assert abs((datetime.now(timezone.utc) - datetime.fromisoformat(timestamp)).total_seconds()) < 5
    signature = base64.b64decode(frame["signature"], validate=True)
    assert VerifyKey(public_key).verify(timestamp.encode(), signature) == timestamp.encode()


@pytest.mark.parametrize("token", [None, "opaque-test-token-not-an-Entra-credential"])
async def test_send_disconnect_and_reconnect_use_direct_sockets_with_pop(
    connect_api, proxy_trap, loopback_relay, token,
):
    url, connections = loopback_relay
    received = asyncio.Queue()
    relay = make_relay(url, received.put, token)
    try:
        async with asyncio.timeout(5):
            with pytest.raises(MeshTransportError, match="not connected"):
                await relay.send_frame({"type": "message"})
            await relay.connect()
            first, frame = await connections.get()
            assert_connect_pop(frame, relay, token)
            await relay.connect()  # Idempotent: no duplicate sockets or connect frames.
            assert connections.empty()
            message = {"v": 1, "type": "message", "id": "before-reconnect", "ciphertext": "opaque"}
            await relay.send_frame(message)
            assert await received.get() == message

            await relay.reconnect()
            second, frame = await connections.get()
            assert first.state is State.CLOSED
            assert_connect_pop(frame, relay, token)
            assert relay.is_connected
            await relay.send_frame({**message, "id": "explicit-reconnect"})
            assert (await received.get())["id"] == "explicit-reconnect"

            # An error close enters the retained-socket cleanup branch, unlike
            # a normal 1000 close. Modern ClientConnection has no .closed API.
            await second.close(code=1011, reason="exercise reconnect cleanup")
            third, frame = await connections.get()
            assert_connect_pop(frame, relay, token)
            assert second.state is State.CLOSED
            assert relay._closing_ws is None and not relay._recv_task.done()
            await relay.send_frame({**message, "id": "remote-reconnect"})
            assert (await received.get())["id"] == "remote-reconnect"
            assert proxy_trap == []

            await relay.disconnect()
            await third.wait_closed()
            assert third.state is State.CLOSED
            assert not relay.is_connected
            assert relay._ws is relay._closing_ws is relay._recv_task is relay._hb_task is None
            with pytest.raises(MeshTransportError, match="not connected"):
                await relay.send_frame(message)
            await relay.disconnect()
    finally:
        await asyncio.wait_for(relay.disconnect(), 5)


async def test_heartbeat_uses_same_direct_connection(connect_api, proxy_trap, loopback_relay):
    url, connections = loopback_relay
    received = asyncio.Queue()
    relay = make_relay(url, received.put, heartbeat=0.01)
    try:
        async with asyncio.timeout(5):
            await relay.connect()
            await connections.get()
            frame = await received.get()
            assert frame["type"] == "heartbeat" and frame["from"] == relay._identity_did
            assert frame["v"] == 1 and frame["ts"].endswith("Z")
            assert connections.empty() and proxy_trap == []
    finally:
        await asyncio.wait_for(relay.disconnect(), 5)


async def test_proxy_capable_api_bypasses_a_demonstrably_active_environment_proxy(
    connect_api, proxy_trap, loopback_relay,
):
    if "proxy" not in inspect.signature(connect_api).parameters:
        pytest.skip("this websockets API does not automatically select environment proxies")
    url, connections = loopback_relay
    async with asyncio.timeout(5):
        with pytest.raises((WebSocketException, OSError)):
            async with connect_api(url, open_timeout=1):
                pytest.fail("the control connection should have reached the rejecting proxy")
        assert proxy_trap == [True]
        assert connections.empty()
        received = asyncio.Queue()
        relay = make_relay(url, received.put)
        try:
            await relay.connect()
            await connections.get()
            await relay.send_frame({"type": "message", "id": "direct-router"})
            assert await received.get() == {"type": "message", "id": "direct-router"}
            assert proxy_trap == [True]
        finally:
            await asyncio.wait_for(relay.disconnect(), 5)
