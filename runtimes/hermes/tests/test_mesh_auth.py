# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Hermes auth lifecycle with the real SDK singleton, identity and prekey lock."""

from __future__ import annotations

import asyncio
import base64
import json
import time
from concurrent.futures import ThreadPoolExecutor
import fcntl
import importlib
import os
from pathlib import Path
import threading
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest

from kars_agt_mesh import MeshTransportError
from kars_agt_mesh import client as sdk
from kars_agt_mesh import relay_transport
from kars_runtime_hermes import plugin
from kars_runtime_hermes.plugin import mesh, mesh_worker, router_client


@pytest.fixture
def runtime(monkeypatch, tmp_path):
    for key in ("MESH_AUTH_BACKEND", "AGT_SKIP_ENTRA", "AGT_OAUTH_TOKEN", "AGT_TRUST_THRESHOLD", "SRE_ENABLED"):
        monkeypatch.delenv(key, raising=False)
    monkeypatch.setenv("MESH_AUTH_BACKEND", "EntraAgentIdentity")
    monkeypatch.setenv("AGT_OAUTH_TOKEN", "test-token")
    monkeypatch.setenv("AGT_TRUST_THRESHOLD", "75")
    monkeypatch.setenv("HERMES_HOME", str(tmp_path))
    monkeypatch.setenv("SANDBOX_NAME", "test-hermes-auth")
    monkeypatch.setattr(sdk, "_SINGLETONS", {})
    monkeypatch.setattr(sdk, "_SINGLETON_LOCK", asyncio.Lock())
    monkeypatch.setattr(mesh, "_MESH_SINGLETON", None)
    monkeypatch.setattr(mesh, "_MESH_PENDING_CLEANUP", None)
    monkeypatch.setattr(mesh, "_MESH_WORKER_REQUESTED", False)
    monkeypatch.setattr(mesh, "_BACKGROUND_LOOP", None)
    monkeypatch.setattr(mesh, "_BACKGROUND_THREAD", None)
    monkeypatch.setattr(mesh_worker, "_WORKER_TASK", None)
    monkeypatch.setattr(mesh_worker, "_WORKER_LOOP", None)
    state = SimpleNamespace(registries=[], relays=[], relay_args=[], calls=[], status=200, error=None)

    def registry(**kwargs):
        result = SimpleNamespace(register_self=AsyncMock(), upload_prekeys=AsyncMock(), aclose=AsyncMock())
        state.registries.append(result)
        return result

    def relay(**kwargs):
        async def connect():
            auth = kwargs.get("connection_auth")
            if auth is not None:
                await auth.prepare_relay()
                await auth.verify_relay()
        result = SimpleNamespace(connect=AsyncMock(side_effect=connect), disconnect=AsyncMock())
        state.relays.append(result)
        state.relay_args.append(kwargs)
        return result

    def verify(method, path, **kwargs):
        assert mesh._MESH_SINGLETON is None
        assert state.relays and state.relays[-1].connect.await_count == 1
        state.calls.append((method, path, kwargs))
        if state.error is not None:
            raise state.error
        return SimpleNamespace(status_code=state.status, text="secret-token-must-not-be-logged",
                               json=lambda: state.verified_body())

    state.registry_factory = Mock(side_effect=registry)
    state.relay_factory = Mock(side_effect=relay)
    state.verify = Mock(side_effect=verify)
    state.verified_body = lambda: {"did": next(iter(sdk._SINGLETONS.values()))._identity.did,
                                   "tier": "verified", "verified_app_id": "app", "verified_tenant_id": "tenant"}
    claims = base64.urlsafe_b64encode(json.dumps({"exp": int(time.time()) + 3600}).encode()).decode().rstrip("=")
    state.token = "header." + claims + ".signature"
    state.mint = Mock(return_value=SimpleNamespace(status_code=200, json=lambda: {
        "access_token": state.token, "token_type": "Bearer", "expires_in": 3000,
    }))

    def request(method, path, **kwargs):
        if method == "GET":
            assert path == "/v1/mesh-token"
            return state.mint()
        return state.verify(method, path, **kwargs)

    monkeypatch.setattr(sdk, "RegistryClient", state.registry_factory)
    monkeypatch.setattr(relay_transport, "RelayTransport", state.relay_factory)
    monkeypatch.setattr(router_client, "call", request)
    state.lock_path = tmp_path / ".agt/.mesh-prekeys.lock"
    yield state

    loop = mesh._BACKGROUND_LOOP
    thread = mesh._BACKGROUND_THREAD
    if loop is not None:
        async def cleanup():
            for client in sdk._SINGLETONS.values():
                # A test may inject a teardown failure; remove it for fixture cleanup.
                for resource in (client._relay, client._registry):
                    if resource is not None:
                        for name in ("disconnect", "aclose"):
                            close = getattr(resource, name, None)
                            if isinstance(close, AsyncMock):
                                close.side_effect = None
                await client.disconnect()
            await loop.shutdown_asyncgens()
            await loop.shutdown_default_executor()

        asyncio.run_coroutine_threadsafe(cleanup(), loop).result(timeout=5)
        loop.call_soon_threadsafe(loop.stop)
        thread.join(timeout=5)
        assert not thread.is_alive()
        loop.close()


def assert_lock(path: Path, *, held: bool):
    with path.open("a") as handle:
        if held:
            with pytest.raises(BlockingIOError):
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        else:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fcntl.flock(handle, fcntl.LOCK_UN)


@pytest.mark.parametrize("token", [None, "", " \n\t", "one two"])
def test_required_environment_token_never_replaces_fresh_router_mint(runtime, monkeypatch, token):
    if token is None:
        monkeypatch.delenv("AGT_OAUTH_TOKEN")
    else:
        monkeypatch.setenv("AGT_OAUTH_TOKEN", token)
    assert mesh._get_or_init_client().connection_auth.token == runtime.token
    runtime.mint.assert_called_once()
    assert "AGT_OAUTH_TOKEN" not in os.environ


@pytest.mark.parametrize("token", ["", "test-token"])
def test_required_skip_conflict_before_any_side_effect(runtime, monkeypatch, token):
    monkeypatch.setenv("AGT_SKIP_ENTRA", "1")
    monkeypatch.setenv("AGT_OAUTH_TOKEN", token)
    with pytest.raises(MeshTransportError, match="conflicts"):
        mesh._get_or_init_client()
    assert not sdk._SINGLETONS
    assert not runtime.registry_factory.called
    assert mesh._BACKGROUND_LOOP is None


def test_verified_client_published_once_with_exact_token_and_loopback_routes(runtime, monkeypatch):
    monkeypatch.setenv("AGT_OAUTH_TOKEN", "  test-token\n")
    monkeypatch.setenv("AGT_RELAY_URL", "wss://unused.example/relay")
    monkeypatch.setenv("AGT_REGISTRY_URL", "https://unused.example/registry")
    client = mesh._get_or_init_client()
    assert mesh._get_or_init_client() is client
    assert mesh._MESH_SINGLETON is client
    assert mesh._MESH_PENDING_CLEANUP is None
    assert runtime.registry_factory.call_count == runtime.relay_factory.call_count == 1
    assert runtime.registry_factory.call_args.kwargs["base_url"] == "http://127.0.0.1:8443/agt/registry"
    assert runtime.relay_args[0]["url"] == "ws://127.0.0.1:8443/agt/relay"
    assert runtime.relay_args[0]["entra_token"] is None
    assert runtime.relay_args[0]["connection_auth"] is client.connection_auth
    runtime.mint.assert_called_once()
    assert runtime.calls == [("POST", "/agt/registry/v1/registry/verify", {
        "json": {"amid": client._identity.did, "verification_token": runtime.token},
    })]
    assert client._config.trust_threshold == 75
    assert_lock(runtime.lock_path, held=True)


@pytest.mark.parametrize("status", [301, 302, 307, 401, 403, 404, 429, 500, 503])
def test_verification_rejection_disconnects_unlocks_and_can_retry(runtime, status, caplog):
    runtime.status = status
    with pytest.raises(MeshTransportError, match="Required Entra registry verification failed"):
        mesh._get_or_init_client()
    client = next(iter(sdk._SINGLETONS.values()))
    assert mesh._MESH_SINGLETON is None
    assert not client._is_connected
    assert client._prekey_lock_fd is None
    assert client._relay is None and client._registry is None
    runtime.relays[0].disconnect.assert_awaited_once()
    runtime.registries[0].aclose.assert_awaited_once()
    assert_lock(runtime.lock_path, held=False)
    assert "secret-token-must-not-be-logged" not in caplog.text
    runtime.status = 200
    assert mesh._get_or_init_client() is client
    assert client._is_connected
    assert runtime.registry_factory.call_count == 2
    assert len(sdk._SINGLETONS) == 1
    assert_lock(runtime.lock_path, held=True)


@pytest.mark.parametrize("error", [TimeoutError("token-secret"), OSError("token-secret")])
def test_verification_outage_does_not_publish_or_leak_secrets(runtime, error, caplog):
    runtime.error = error
    with pytest.raises(MeshTransportError) as caught:
        mesh._get_or_init_client()
    assert "token-secret" not in str(caught.value) + caplog.text
    assert mesh._MESH_SINGLETON is None
    assert_lock(runtime.lock_path, held=False)
    runtime.relays[0].disconnect.assert_awaited_once()
    runtime.registries[0].aclose.assert_awaited_once()


def test_connect_failure_before_relay_releases_lock(runtime):
    original = runtime.registry_factory.side_effect

    def broken_registry(**kwargs):
        result = original(**kwargs)
        result.register_self.side_effect = OSError("registry unavailable")
        return result

    runtime.registry_factory.side_effect = broken_registry
    with pytest.raises(OSError, match="registry unavailable"):
        mesh._get_or_init_client()
    assert mesh._MESH_SINGLETON is None
    assert not runtime.relay_factory.called
    assert not runtime.verify.called
    runtime.registries[0].aclose.assert_awaited_once()
    assert_lock(runtime.lock_path, held=False)
    runtime.registry_factory.side_effect = original
    assert mesh._get_or_init_client()._is_connected


def test_connect_timeout_cancels_work_before_unlock_and_retry(runtime, monkeypatch):
    cancelled = threading.Event()
    original_factory = runtime.relay_factory.side_effect
    original_wait_for = asyncio.wait_for

    async def never_connect():
        try:
            await asyncio.Future()
        except asyncio.CancelledError:
            cancelled.set()
            raise

    def slow_relay(**kwargs):
        result = original_factory(**kwargs)
        result.connect.side_effect = never_connect
        return result

    async def short_timeout(coro, timeout):
        assert timeout == 30.0
        return await original_wait_for(coro, timeout=0.03)

    runtime.relay_factory.side_effect = slow_relay
    monkeypatch.setattr(mesh.asyncio, "wait_for", short_timeout)
    with pytest.raises(TimeoutError):
        mesh._get_or_init_client()
    assert cancelled.is_set()
    assert mesh._MESH_SINGLETON is None
    assert not runtime.verify.called
    runtime.relays[0].disconnect.assert_awaited_once()
    runtime.registries[0].aclose.assert_awaited_once()
    assert_lock(runtime.lock_path, held=False)
    runtime.relay_factory.side_effect = original_factory
    assert mesh._get_or_init_client()._is_connected


@pytest.mark.parametrize("resource,method", [("relays", "disconnect"), ("registries", "aclose")])
def test_failed_teardown_is_quarantined_before_cached_client_retry(runtime, resource, method):
    def verification_failure(*args, **kwargs):
        getattr(getattr(runtime, resource)[0], method).side_effect = [OSError("close failed"), None]
        return SimpleNamespace(status_code=403)

    runtime.verify.side_effect = verification_failure
    with pytest.raises(OSError, match="close failed"):
        mesh._get_or_init_client()
    client = next(iter(sdk._SINGLETONS.values()))
    assert mesh._MESH_SINGLETON is None
    assert mesh._MESH_PENDING_CLEANUP is client
    assert_lock(runtime.lock_path, held=True)
    runtime.verify.side_effect = lambda *args, **kwargs: SimpleNamespace(status_code=200, json=runtime.verified_body)
    assert mesh._get_or_init_client() is client
    assert runtime.relay_factory.call_count == 2
    assert getattr(getattr(runtime, resource)[0], method).await_count == 2
    assert mesh._MESH_PENDING_CLEANUP is None
    assert_lock(runtime.lock_path, held=True)


def test_repeated_teardown_failure_blocks_cached_client_publication(runtime):
    def verification_failure(*args, **kwargs):
        runtime.relays[0].disconnect.side_effect = OSError("close unavailable")
        return SimpleNamespace(status_code=403)

    runtime.verify.side_effect = verification_failure
    for _ in range(2):
        with pytest.raises(OSError, match="close unavailable"):
            mesh._get_or_init_client()
        assert mesh._MESH_SINGLETON is None
        assert mesh._MESH_PENDING_CLEANUP is not None
        assert runtime.relay_factory.call_count == 1
        assert runtime.verify.call_count == 1
        assert_lock(runtime.lock_path, held=True)
    runtime.relays[0].disconnect.side_effect = None
    runtime.verify.side_effect = lambda *args, **kwargs: SimpleNamespace(status_code=200, json=runtime.verified_body)
    assert mesh._get_or_init_client()._is_connected
    assert mesh._MESH_PENDING_CLEANUP is None


def test_concurrent_getter_cannot_observe_unverified_client(runtime):
    entered = threading.Event()
    release = threading.Event()

    def verify(*args, **kwargs):
        entered.set()
        assert release.wait(timeout=5)
        return SimpleNamespace(status_code=200, json=runtime.verified_body)

    runtime.verify.side_effect = verify
    with ThreadPoolExecutor(max_workers=2) as executor:
        first = executor.submit(mesh._get_or_init_client)
        try:
            assert entered.wait(timeout=5)
            second = executor.submit(mesh._get_or_init_client)
            assert mesh._MESH_SINGLETON is None
            assert not second.done()
            assert runtime.relay_factory.call_count == 1
        finally:
            release.set()
        assert first.result(timeout=5) is second.result(timeout=5)
    assert runtime.verify.call_count == 1


def test_existing_prekey_writer_is_not_bypassed_or_unlocked(runtime):
    runtime.lock_path.parent.mkdir(parents=True)
    with runtime.lock_path.open("a") as owner:
        fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with pytest.raises(MeshTransportError, match="Another mesh-client process"):
            mesh._get_or_init_client()
        assert mesh._MESH_SINGLETON is None
        assert not runtime.registry_factory.called
        assert_lock(runtime.lock_path, held=True)
    assert mesh._get_or_init_client()._is_connected


def test_explicit_no_entra_mode_does_not_verify_or_forward_stale_token(runtime, monkeypatch):
    monkeypatch.delenv("MESH_AUTH_BACKEND")
    monkeypatch.setenv("AGT_SKIP_ENTRA", "1")
    client = mesh._get_or_init_client()
    assert client._config.trust_threshold == 0
    assert runtime.relay_args[0]["entra_token"] is None
    assert not runtime.verify.called
    assert "AGT_OAUTH_TOKEN" not in os.environ


def test_optional_legacy_verification_failure_preserves_existing_mode(runtime, monkeypatch):
    monkeypatch.delenv("MESH_AUTH_BACKEND")
    runtime.status = 403
    client = mesh._get_or_init_client()
    assert client._is_connected
    assert mesh._MESH_SINGLETON is client
    assert_lock(runtime.lock_path, held=True)


def prepare_plugin_registration(monkeypatch):
    for name in ("governance", "spawn", "discover", "foundry", "http_fetch", "mcp_bridge",
                 "mesh", "handoff", "sre", "telemetry"):
        module = importlib.import_module(f"kars_runtime_hermes.plugin.{name}")
        monkeypatch.setattr(module, "register", Mock())
    original_thread = threading.Thread

    def thread(*args, **kwargs):
        if kwargs.get("name") == "kars-mesh-eager-init":
            return SimpleNamespace(start=kwargs["target"])
        return original_thread(*args, **kwargs)

    monkeypatch.setattr(threading, "Thread", thread)
    start_worker = Mock()
    monkeypatch.setattr(mesh_worker, "start_worker", start_worker)
    return start_worker


def test_eager_startup_never_starts_listener_after_rejected_verification(runtime, monkeypatch):
    start_worker = prepare_plugin_registration(monkeypatch)
    runtime.status = 403
    plugin.register(Mock())
    assert not start_worker.called
    assert mesh._MESH_SINGLETON is None
    assert_lock(runtime.lock_path, held=False)
    runtime.status = 200
    mesh._get_or_init_client()
    start_worker.assert_called_once_with(mesh._get_or_init_client)
    assert mesh._MESH_SINGLETON is not None
    assert len(sdk._SINGLETONS) == 1


@pytest.mark.parametrize("sre", ["true", "True", "TRUE", "1", "yes", "YES"])
def test_sre_never_constructs_mesh_client_or_starts_worker(runtime, monkeypatch, sre):
    start_worker = prepare_plugin_registration(monkeypatch)
    monkeypatch.setenv("SRE_ENABLED", sre)
    monkeypatch.setenv("AGT_SKIP_ENTRA", "1")
    plugin.register(Mock())
    with pytest.raises(MeshTransportError, match="disabled in SRE"):
        mesh._get_or_init_client()
    assert not start_worker.called
    assert not sdk._SINGLETONS
    assert mesh._BACKGROUND_LOOP is None
    assert not runtime.registry_factory.called
