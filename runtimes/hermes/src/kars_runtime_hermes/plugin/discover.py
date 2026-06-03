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


def _kars_discover(args: dict[str, Any], **_kwargs: Any) -> str:
    """Discover an agent in the AGT registry by display name or DID."""
    query = str(args.get("name") or args.get("did") or "").strip()
    if not query:
        return json.dumps({"error": "name or did is required"})

    # If it looks like a DID, look up directly; else search by display_name
    if query.startswith("did:mesh:") or query.startswith("did:agentmesh:"):
        try:
            resp = router_client.call("GET", f"/agt/registry/v1/agents/{query}")
        except Exception as exc:  # noqa: BLE001
            return json.dumps({"error": f"registry lookup failed: {exc}"})

        if resp.status_code == 404:
            return json.dumps({"error": f"agent '{query}' not found in registry"})
        if resp.status_code >= 400:
            return json.dumps({"error": f"HTTP {resp.status_code}: {resp.text[:200]}"})
        try:
            record = resp.json()
        except Exception:  # noqa: BLE001
            return json.dumps({"error": "non-JSON registry response"})
        return json.dumps({"agent": record})

    # Search by display name
    try:
        resp = router_client.call(
            "GET", "/agt/registry/v1/agents", params={"display_name": query}
        )
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"registry search failed: {exc}"})

    if resp.status_code >= 400:
        return json.dumps({"error": f"HTTP {resp.status_code}: {resp.text[:200]}"})
    try:
        result = resp.json()
    except Exception:  # noqa: BLE001
        return json.dumps({"error": "non-JSON registry response"})

    agents = result.get("agents", []) if isinstance(result, dict) else []
    if not agents:
        return json.dumps({"agents": [], "message": f"no agents matching '{query}'"})
    return json.dumps({"agents": agents, "count": len(agents)})


_DISCOVER_SCHEMA = {
    "name": "kars_discover",
    "description": (
        "Look up peer agents in the AGT registry. Pass `name` (display name) "
        "or `did` (full DID like did:mesh:abc123…). Returns the agent record "
        "including AMID, capabilities, reputation score, trust tier, and "
        "(if applicable) verified Entra app ID. Use this to find peer agents "
        "before you would send them messages via kars_mesh_send (NOTE: "
        "kars_mesh_send is not available in Hermes v0.5.2)."
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

