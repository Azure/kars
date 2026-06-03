"""Foundry tool wrappers — Phase A1.7.

**Design**: Hermes ships with a strong native MCP client; the kars
inference router exposes all 9 Foundry tools at ``POST /platform/mcp``
already (controller routes Foundry data-plane via the platform MCP
gateway). The entrypoint registers ``platform`` as an MCP server in
Hermes' config, so Hermes' MCP client discovers + exposes the full
Foundry catalogue automatically as ``mcp:platform:foundry_*``.

This module adds ONE native wrapper — ``foundry_memory`` — because:
  1. It's the most frequently invoked Foundry tool by far.
  2. It honors the kars ``memory-${SANDBOX_NAME}`` convention (per
     ``docs/runtimes/CONTRACT.md`` § KarsMemory binding) so the
     KarsMemory CR binding is preserved without operator intervention.
  3. Sub-second latency vs. round-tripping through the MCP gateway.

The other 8 Foundry tools (foundry_code_execute, foundry_web_search,
foundry_file_search, foundry_image_generation, foundry_conversations,
foundry_evaluations, foundry_deployments, foundry_agents,
foundry_download_file) reach the LLM via the platform MCP path.

Registration is gated on ``KARS_PROVIDER``: skipped when value is
``github-models`` or ``github-copilot`` (slim modes — no Foundry
project bound).
"""

from __future__ import annotations

import json
import logging
import os
from typing import Any

from . import router_client

logger = logging.getLogger("kars.hermes.foundry")

_FOUNDRY_API_VERSION = "2025-11-15-preview"


def _store_name() -> str:
    """Resolve the Foundry Memory Store this sandbox is bound to.

    Convention (matches OpenClaw plugin + KarsMemory CR ``storeName``):
    ``memory-${SANDBOX_NAME}``. Operators bind via
    ``KarsMemory.spec.storeName: memory-<sandbox>``; the router
    auto-provisions on first 404.
    """
    sandbox = os.environ.get("SANDBOX_NAME", "unknown")
    # Defensive truncation: K8s DNS-label limit is 63 chars; we use 7
    # chars for the "memory-" prefix, leaving 56 for the sandbox name.
    return f"memory-{sandbox[:56]}"


def _foundry_memory(args: dict[str, Any], **_kwargs: Any) -> str:
    """Memory CRUD against the Foundry Memory Store bound to this sandbox."""
    op = str(args.get("operation", "")).lower().strip()
    store = str(args.get("store_name") or _store_name()).strip()
    scope = str(args.get("scope") or f"agent:{os.environ.get('SANDBOX_NAME', 'unknown')}").strip()

    if op not in ("search", "update", "delete_scope"):
        return json.dumps(
            {
                "error": "operation must be 'search', 'update', or 'delete_scope'",
                "got": op,
            }
        )

    api_ver = f"api-version={_FOUNDRY_API_VERSION}"
    base = f"/memory_stores/{store}"

    try:
        if op == "search":
            query = args.get("query", "")
            body = {
                "query_text": str(query) if query else "",
                "scope": scope,
                "top_k": int(args.get("top_k", 10)),
            }
            resp = router_client.call(
                "POST", f"{base}:search_memories?{api_ver}", json=body
            )
        elif op == "update":
            text = args.get("text", "")
            if not text:
                return json.dumps({"error": "operation=update requires 'text'"})
            body = {
                "messages": [{"role": "user", "content": str(text)}],
                "scope": scope,
            }
            resp = router_client.call(
                "POST", f"{base}:update_memories?{api_ver}", json=body
            )
        else:  # delete_scope
            resp = router_client.call(
                "POST", f"{base}:delete_scope?{api_ver}", json={"scope": scope}
            )
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"foundry_memory request failed: {exc}"})

    if resp.status_code == 404 and op != "delete_scope":
        # First-time use: router auto-creates the store on next call.
        # Retry once.
        try:
            router_client.call(
                "POST",
                f"/memory_stores?{api_ver}",
                json={
                    "name": store,
                    "description": f"kars memory binding for sandbox {os.environ.get('SANDBOX_NAME', '')}",
                },
            )
        except Exception:  # noqa: BLE001
            pass  # the next call below will surface the real error

    if resp.status_code >= 400:
        return json.dumps(
            {
                "error": f"foundry_memory HTTP {resp.status_code}",
                "body": resp.text[:500],
                "operation": op,
                "store": store,
                "scope": scope,
            }
        )

    try:
        return json.dumps(resp.json())
    except Exception:  # noqa: BLE001
        return json.dumps(
            {"operation": op, "store": store, "scope": scope, "raw": resp.text[:500]}
        )


_FOUNDRY_MEMORY_SCHEMA = {
    "name": "foundry_memory",
    "description": (
        "Persistent agent memory backed by Azure AI Foundry Memory Store. "
        "Operations: 'search' (semantic search over past memories matching "
        "scope), 'update' (write a new memory), 'delete_scope' (wipe all "
        "memories under a scope). The store name follows the kars convention "
        "memory-${SANDBOX_NAME} and is auto-provisioned on first use. The "
        "scope defaults to agent:${SANDBOX_NAME}; pass scope to namespace "
        "memories per-session or per-user."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "operation": {
                "type": "string",
                "enum": ["search", "update", "delete_scope"],
                "description": "What to do",
            },
            "text": {
                "type": "string",
                "description": "[operation=update] Memory text to write",
            },
            "query": {
                "type": "string",
                "description": "[operation=search] Query text (empty = list all in scope)",
            },
            "top_k": {
                "type": "integer",
                "description": "[operation=search] Max results (default 10)",
            },
            "scope": {
                "type": "string",
                "description": (
                    "Memory namespace (default: agent:${SANDBOX_NAME}). "
                    "Use 'session:<id>' for per-conversation memory, "
                    "'user:<id>' for per-user memory across sessions."
                ),
            },
            "store_name": {
                "type": "string",
                "description": (
                    "Override the kars store name convention "
                    "(default: memory-${SANDBOX_NAME}). Usually leave unset."
                ),
            },
        },
        "required": ["operation"],
    },
}


def register(ctx: Any) -> None:  # noqa: ANN401
    """Register foundry_memory natively. Other foundry_* tools via platform MCP."""
    provider = os.environ.get("KARS_PROVIDER", "")
    if provider in {"github-models", "github-copilot"}:
        logger.info(
            "Foundry tools skipped (KARS_PROVIDER=%s — slim mode, no Foundry project)",
            provider,
        )
        return
    if not os.environ.get("FOUNDRY_PROJECT_ENDPOINT"):
        logger.info(
            "Foundry tools skipped (no FOUNDRY_PROJECT_ENDPOINT bound)"
        )
        return

    ctx.register_tool(
        name="foundry_memory",
        toolset="foundry",
        schema=_FOUNDRY_MEMORY_SCHEMA,
        handler=_foundry_memory,
        description=_FOUNDRY_MEMORY_SCHEMA["description"],
    )
    logger.info(
        "foundry_memory registered natively (other 8 Foundry tools via platform MCP)"
    )

