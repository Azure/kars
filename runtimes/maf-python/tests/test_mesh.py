# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import httpx
import pytest
import respx

from kars_runtime_maf_python import mesh


@respx.mock
def test_send_message_posts_envelope_to_relay():
    route = respx.post("http://127.0.0.1:8443/agt/relay/send").mock(
        return_value=httpx.Response(200, json={"task_id": "t1", "state": "submitted"})
    )
    client = mesh.MeshClient()
    out = client.send("did:mesh:other", "hello")
    client.close()
    assert route.called
    body = route.calls.last.request.read().decode()
    assert "did:mesh:other" in body
    assert "hello" in body
    assert out["task_id"] == "t1"


@respx.mock
def test_receive_messages_returns_inbox(monkeypatch):
    monkeypatch.setenv("KARS_AGENT_DID", "did:mesh:me")
    route = respx.get("http://127.0.0.1:8443/agt/relay/inbox").mock(
        return_value=httpx.Response(
            200,
            json={"messages": [{"task_id": "t9", "content": "hi"}]},
        )
    )
    client = mesh.MeshClient()
    msgs = client.receive()
    client.close()
    assert route.called
    assert msgs == [{"task_id": "t9", "content": "hi"}]
    assert route.calls.last.request.url.params["agent_did"] == "did:mesh:me"


@respx.mock
def test_receive_messages_accepts_bare_list():
    respx.get("http://127.0.0.1:8443/agt/relay/inbox").mock(
        return_value=httpx.Response(200, json=[{"task_id": "x"}])
    )
    client = mesh.MeshClient()
    assert client.receive() == [{"task_id": "x"}]
    client.close()


@respx.mock
def test_lookup_returns_none_on_404():
    respx.get("http://127.0.0.1:8443/agt/registry/lookup").mock(
        return_value=httpx.Response(404)
    )
    client = mesh.MeshClient()
    assert client.lookup("missing") is None
    client.close()


@respx.mock
def test_lookup_returns_record_on_200():
    respx.get("http://127.0.0.1:8443/agt/registry/lookup").mock(
        return_value=httpx.Response(200, json={"did": "did:mesh:abc"})
    )
    client = mesh.MeshClient()
    assert client.lookup("alice") == {"did": "did:mesh:abc"}
    client.close()


@respx.mock
def test_send_raises_on_5xx():
    respx.post("http://127.0.0.1:8443/agt/relay/send").mock(
        return_value=httpx.Response(503)
    )
    client = mesh.MeshClient()
    with pytest.raises(httpx.HTTPStatusError):
        client.send("did:mesh:x", "y")
    client.close()


@respx.mock
def test_module_send_message_uses_default_client(monkeypatch):
    monkeypatch.setenv("KARS_AGENT_DID", "did:mesh:mod")
    respx.post("http://127.0.0.1:8443/agt/relay/send").mock(
        return_value=httpx.Response(200, json={"ok": True})
    )
    out = mesh.send_message("did:mesh:peer", "ping")
    assert out == {"ok": True}


def test_build_envelope_uses_a2a_when_available():
    env = mesh._build_envelope("did:mesh:t", "body")
    # The upstream `a2a_agentmesh.TaskEnvelope.to_dict` puts the target
    # DID under the trust extension; our degraded shim puts it at the
    # top level. Accept either shape.
    target = env.get("target_did") or env.get("x-agentmesh-trust", {}).get(
        "target_did"
    )
    assert target == "did:mesh:t"
    # Either the upstream `id` or our shim's `task_id`.
    assert "task_id" in env or "id" in env
    serialized = str(env)
    assert "body" in serialized


def test_relay_url_normalizes_trailing_slash(monkeypatch):
    monkeypatch.setenv("KARS_AGT_RELAY_URL", "http://relay.local/agt/relay")
    client = mesh.MeshClient()
    assert client.relay_url.endswith("/")
    client.close()


def test_env_did_default():
    # Env unset by autouse fixture.
    assert mesh._env_did() == "did:mesh:unknown"
