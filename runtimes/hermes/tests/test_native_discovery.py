# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

from datetime import datetime, timedelta, timezone
from types import SimpleNamespace

import httpx
import pytest

from kars_runtime_hermes.plugin.discover import discover_peer


def record(letter, age=0, name="peer", **overrides):
    return {"did": "did:mesh:" + letter * 32, "capabilities": [name],
            "metadata": {"display_name": name},
            "last_seen": (datetime.now(timezone.utc) - timedelta(seconds=age)).isoformat(), **overrides}


async def discover(records, args=None, lookup=None):
    args = args or {"name": "peer"}

    def handler(request):
        assert request.method == "GET"
        if request.url.path == "/v1/discover":
            assert dict(request.url.params) == {"capability": args["name"], "limit": "16"}
            return httpx.Response(200, json={"results": records})
        identity = request.url.path.removeprefix("/v1/agents/")
        if lookup:
            return lookup(identity)
        found = next((row for row in records if row["did"] == identity), None)
        return httpx.Response(200 if found else 404, json=found)

    async with httpx.AsyncClient(base_url="https://registry", transport=httpx.MockTransport(handler)) as http:
        return await discover_peer(SimpleNamespace(_registry=SimpleNamespace(_client=http)), args)


@pytest.mark.asyncio
async def test_old_first_registration_is_not_returned_as_the_live_peer():
    old, live = record("a", 120), record("b")
    result = await discover([old, live])
    assert result == {"agents": [live], "count": 1, "ambiguous": False}


@pytest.mark.asyncio
async def test_two_fresh_registrations_are_explicitly_ambiguous_newest_first():
    old, live = record("a", 70), record("b")
    assert await discover([old, live]) == {"agents": [live, old], "count": 2, "ambiguous": True}


@pytest.mark.asyncio
@pytest.mark.parametrize("changes", [
    {"last_seen": ""}, {"last_seen": None}, {"last_seen": "invalid"},
    {"last_seen": "2099-01-01T00:00:00+00:00"},
    {"last_seen": datetime.now().isoformat()}, {"status": "offline"},
    {"metadata": {"display_name": "different-peer"}},
])
async def test_invalid_or_inexact_records_are_not_peer_candidates(changes):
    assert await discover([record("a", **changes)]) == {"agents": [], "count": 0, "ambiguous": False}


@pytest.mark.asyncio
async def test_discovery_hits_are_hydrated_before_name_matching():
    hit, full = record("a"), record("a", name="different-peer")
    result = await discover([hit], lookup=lambda _: httpx.Response(200, json=full))
    assert result["count"] == 0


@pytest.mark.asyncio
async def test_duplicate_did_is_not_an_ambiguous_registration():
    live = record("a")
    assert await discover([live, live]) == {"agents": [live], "count": 1, "ambiguous": False}


@pytest.mark.asyncio
async def test_direct_lookup_requires_the_requested_fresh_identity():
    live = record("a")
    assert await discover([live], {"did": live["did"]}) == {"agent": live}
    assert "no fresh registration" in (await discover([record("a", 120)], {"did": live["did"]}))["error"]
    with pytest.raises(ValueError, match="identity mismatch"):
        await discover([live], {"did": live["did"]}, lambda _: httpx.Response(200, json=record("b")))


@pytest.mark.asyncio
@pytest.mark.parametrize("status", [401, 403, 500])
async def test_lookup_failure_cannot_hide_an_ambiguous_candidate(status):
    with pytest.raises(ValueError, match=f"Registry HTTP {status}"):
        await discover([record("a"), record("b")], lookup=lambda _: httpx.Response(status))


@pytest.mark.asyncio
async def test_missing_registration_is_not_replaced_with_unverified_search_hit():
    assert (await discover([record("a")], lookup=lambda _: httpx.Response(404)))["count"] == 0


@pytest.mark.asyncio
async def test_invalid_and_excessive_candidates_fail_closed():
    with pytest.raises(ValueError, match="Invalid registry peer DID"):
        await discover([record("a", did="../../other")])
    with pytest.raises(ValueError, match="exceeds native peer limit"):
        await discover([record("a")] * 17)
