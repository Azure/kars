# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Router wire contracts for regular and stricter assignment-native discovery."""

import asyncio
import json
from types import SimpleNamespace

import httpx
import pytest

from kars_runtime_hermes.plugin import discover


@pytest.mark.parametrize(
    "args,path,params,response,expected",
    [
        (
            {"name": "peer + name"},
            "/v1/discover",
            {"capability": "peer + name", "limit": "50"},
            httpx.Response(200, json={"results": [{"did": "did:mesh:a", "capabilities": ["peer + name"]}], "total": 1}),
            {"agents": [{"did": "did:mesh:a", "capabilities": ["peer + name"]}], "count": 1},
        ),
        (
            {"name": "absent"},
            "/v1/discover",
            {"capability": "absent", "limit": "50"},
            httpx.Response(200, json={"results": [], "total": 0}),
            {"agents": [], "message": "no agents matching 'absent'"},
        ),
        (
            {"did": "did:mesh:a"},
            "/v1/agents/did:mesh:a",
            {},
            httpx.Response(200, json={"did": "did:mesh:a", "metadata": {"display_name": "peer"}}),
            {"agent": {"did": "did:mesh:a", "metadata": {"display_name": "peer"}}},
        ),
        (
            {"did": "did:mesh:absent"},
            "/v1/agents/did:mesh:absent",
            {},
            httpx.Response(404),
            {"error": "agent 'did:mesh:absent' not found in registry"},
        ),
        (
            {"name": "peer"},
            "/v1/discover",
            {"capability": "peer", "limit": "50"},
            httpx.Response(403, text="denied"),
            {"error": "HTTP 403: denied"},
        ),
        (
            {"name": "peer"},
            "/v1/discover",
            {"capability": "peer", "limit": "50"},
            httpx.Response(200, text="not json"),
            {"error": "non-JSON registry response"},
        ),
        (
            {"name": "peer"},
            "/v1/discover",
            {"capability": "peer", "limit": "50"},
            httpx.Response(200, json={"agents": []}),
            {"error": "invalid registry discovery response"},
        ),
        (
            {"name": "peer"},
            "/v1/discover",
            {"capability": "peer", "limit": "50"},
            httpx.Response(200, json={"results": {}}),
            {"error": "invalid registry discovery response"},
        ),
        (
            {"name": "peer"},
            "/v1/discover",
            {"capability": "peer", "limit": "50"},
            httpx.Response(200, json={"results": [None]}),
            {"error": "invalid registry discovery response"},
        ),
    ],
)
def test_regular_discovery_wire(args, path, params, response, expected, monkeypatch):
    requests = []

    def registry(request):
        requests.append(request)
        assert request.method == "GET"
        assert request.url.path == "/agt/registry" + path
        assert dict(request.url.params) == params
        return httpx.Response(response.status_code, content=response.content)

    with httpx.Client(base_url="http://127.0.0.1:8443", transport=httpx.MockTransport(registry)) as pool:
        monkeypatch.setattr(discover.router_client, "call", pool.request)
        result = json.loads(discover._kars_discover(args))
    assert result == expected
    assert len(requests) == 1


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "response,error",
    [
        (httpx.Response(403, text="denied"), "Registry HTTP 403"),
        (httpx.Response(200, text="not json"), "non-JSON registry response"),
        (httpx.Response(200, json={"agents": []}), "invalid registry discovery response"),
        (httpx.Response(200, json={"results": {}}), "invalid registry discovery response"),
        (httpx.Response(200, json={"results": [None]}), "invalid registry discovery response"),
    ],
)
async def test_native_discovery_wire_rejects_invalid_responses(response, error):
    def registry(request):
        assert request.method == "GET"
        assert request.url.path == "/agt/registry/v1/discover"
        assert dict(request.url.params) == {"capability": "peer", "limit": "16"}
        return httpx.Response(response.status_code, content=response.content)

    async with httpx.AsyncClient(
        base_url="http://127.0.0.1:8443/agt/registry",
        transport=httpx.MockTransport(registry),
    ) as pool:
        client = SimpleNamespace(_registry=SimpleNamespace(_client=pool))
        with pytest.raises(ValueError, match=error):
            await discover.discover_peer(client, {"name": "peer"})
        assert not pool.is_closed


@pytest.mark.asyncio
@pytest.mark.parametrize("direct,missing", [(False, False), (True, False), (False, True), (True, True)])
async def test_native_discovery_wire_hydrates_fresh_identity(direct, missing):
    from datetime import datetime, timezone

    identity = "did:mesh:" + "a" * 32
    agent = {
        "did": identity,
        "metadata": {"display_name": "peer + name"},
        "last_seen": datetime.now(timezone.utc).isoformat(),
    }
    requests = []

    def registry(request):
        requests.append(request)
        assert request.method == "GET"
        if request.url.path == "/agt/registry/v1/discover":
            assert not direct
            assert dict(request.url.params) == {"capability": "peer + name", "limit": "16"}
            return httpx.Response(200, json={"results": [] if missing else [{"did": identity}]})
        assert request.url.path == "/agt/registry/v1/agents/" + identity
        assert not request.url.params
        return httpx.Response(404 if missing else 200, json=None if missing else agent)

    async with httpx.AsyncClient(
        base_url="http://127.0.0.1:8443/agt/registry",
        transport=httpx.MockTransport(registry),
    ) as pool:
        client = SimpleNamespace(_registry=SimpleNamespace(_client=pool))
        result = await discover.discover_peer(client, {"did": identity} if direct else {"name": "peer + name"})
        assert not pool.is_closed
    if direct:
        assert result == ({"error": f"agent '{identity}' not found in registry"} if missing else {"agent": agent})
    else:
        assert result == {"agents": [] if missing else [agent], "count": 0 if missing else 1, "ambiguous": False}
    assert len(requests) == (1 if direct or missing else 2)


def test_native_discovery_keeps_response_bound():
    async def run():
        async with httpx.AsyncClient(
            base_url="http://127.0.0.1:8443/agt/registry",
            transport=httpx.MockTransport(lambda _: httpx.Response(200, content=b" " * (128 * 1024 + 1))),
        ) as pool:
            client = SimpleNamespace(_registry=SimpleNamespace(_client=pool))
            with pytest.raises(ValueError, match="Registry response exceeds native peer limit"):
                await discover.discover_peer(client, {"name": "peer"})
            assert not pool.is_closed

    asyncio.run(run())
