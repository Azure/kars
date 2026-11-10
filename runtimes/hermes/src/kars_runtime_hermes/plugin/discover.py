# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""kars_discover — Phase A1.6.

Look up peer agents in the AGT registry via the router's
``/agt/registry/*`` HTTP proxy. Works without a MeshClient because
the registry is REST-only (the mesh-aware part is the WebSocket
relay, not the registry).

Returns the agent record (name, AMID, capabilities, reputation,
tier, verified Entra app ID if applicable).
"""

from __future__ import annotations

import json
import logging
from typing import Any

from . import router_client

logger = logging.getLogger("kars.hermes.discover")


def _lookup(args):
    query = str(args.get("name") or args.get("did") or "").strip()
    if not query:
        raise ValueError("name or did is required")
    direct = query.startswith(("did:mesh:", "did:agentmesh:"))
    path = f"/v1/agents/{query}" if direct else "/v1/discover"
    params = {} if direct else {"capability": query, "limit": 50}
    return query, direct, path, params


def _lookup_result(resp, query, direct):
    if direct and resp.status_code == 404:
        return {"error": f"agent '{query}' not found in registry"}
    if resp.status_code >= 400:
        return {"error": f"HTTP {resp.status_code}: {resp.text[:200]}"}
    try:
        result = resp.json()
    except Exception:
        return {"error": "non-JSON registry response"}
    if direct:
        return {"agent": result}
    agents = result.get("results") if isinstance(result, dict) else None
    if not isinstance(agents, list) or not all(isinstance(agent, dict) for agent in agents):
        return {"error": "invalid registry discovery response"}
    return ({"agents": agents, "count": len(agents)} if agents else
            {"agents": [], "message": f"no agents matching '{query}'"})


def _kars_discover(args: dict[str, Any], **_kwargs: Any) -> str:
    """Discover an agent in the AGT registry by display name or DID."""
    try:
        query, direct, path, params = _lookup(args)
    except ValueError as exc:
        return json.dumps({"error": str(exc)})
    try:
        resp = router_client.call("GET", "/agt/registry" + path, **({"params": params} if params else {}))
        return json.dumps(_lookup_result(resp, query, direct))
    except Exception as exc:
        operation = "lookup" if direct else "search"
        return json.dumps({"error": f"registry {operation} failed: {exc}"})


async def discover_peer(client, args):
    """Same lookup on the parent's existing async registry pool, cancellable in-place."""
    import httpx
    from . import mission_protocol as wire

    from datetime import datetime, timezone
    import re

    async def lookup(path, params, query, direct):
        async with client._registry._client.stream("GET", path, params=params) as resp:
            wire.require(resp.status_code < 400 or resp.status_code == 404,
                         f"Registry HTTP {resp.status_code}")
            body = bytearray()
            async for chunk in resp.aiter_bytes():
                body.extend(chunk)
                wire.require(len(body) <= 128 * 1024, "Registry response exceeds native peer limit")
            result = _lookup_result(httpx.Response(resp.status_code, content=bytes(body)), query, direct)
            wire.require("error" not in result or resp.status_code == 404,
                         result.get("error", "Invalid registry response"))
            return result

    query, direct, path, params = _lookup(args)
    if not direct:
        params["limit"] = 16
    result = await lookup(path, params, query, direct)
    if "error" in result:
        return result
    candidates = [result["agent"]] if direct else result["agents"]
    wire.require(len(candidates) <= 16, "Registry discovery exceeds native peer limit")
    records = {}
    for candidate in candidates:
        identity = candidate.get("did")
        wire.require(isinstance(identity, str) and re.fullmatch(r"did:mesh:[a-f0-9]{32}", identity),
                     "Invalid registry peer DID")
        if direct:
            wire.require(identity == query, "Registry peer identity mismatch")
            agent = candidate
        else:
            full = await lookup(f"/v1/agents/{identity}", {}, identity, True)
            if "error" in full:
                continue
            agent = full["agent"]
            wire.require(agent.get("did") == identity, "Registry peer identity mismatch")
        metadata = agent.get("metadata") or {}
        display = metadata.get("display_name") or next(
            (value for value in agent.get("capabilities", []) if not value.startswith("did:")), None)
        if not direct and display != query:
            continue
        try:
            seen = datetime.fromisoformat(agent["last_seen"].replace("Z", "+00:00"))
            if seen.tzinfo is None:
                continue
            age = (datetime.now(timezone.utc) - seen).total_seconds()
        except (KeyError, TypeError, ValueError, AttributeError):
            continue
        if -5 <= age <= 90 and agent.get("status") != "offline":
            records[identity] = (seen, agent)
    agents = [entry[1] for entry in sorted(records.values(), key=lambda entry: entry[0], reverse=True)]
    if direct:
        return {"agent": agents[0]} if agents else {"error": f"agent '{query}' has no fresh registration"}
    return {"agents": agents, "count": len(agents), "ambiguous": len(agents) > 1}


_DISCOVER_SCHEMA = {
    "name": "kars_discover",
    "description": (
        "Look up peer agents in the AGT registry. Pass `name` (display name) "
        "or `did` (full DID like did:mesh:abc123…). Returns the agent record "
        "including AMID, capabilities, reputation score, trust tier, and "
        "(if applicable) verified Entra app ID. Use this to find peer agents "
        "before sending them messages via kars_mesh_send."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "Display name to search for (e.g. 'auditor', 'writer-bot')",
            },
            "did": {
                "type": "string",
                "description": "Full DID for direct lookup (e.g. 'did:mesh:abc123…')",
            },
        },
        "required": [],
    },
}


def register(ctx: Any) -> None:  # noqa: ANN401
    """Register kars_discover with Hermes."""
    ctx.register_tool(
        name="kars_discover",
        toolset="kars_discover",
        schema=_DISCOVER_SCHEMA,
        handler=_kars_discover,
        description=_DISCOVER_SCHEMA["description"],
    )
    logger.info("kars_discover registered")

