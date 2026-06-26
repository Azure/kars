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

# The Foundry Memory Store contract — store + scope resolution from the
# KarsMemory binding, the request body shapes, auto-provision on 404,
# retry, and 401/403 RBAC surfacing — is owned by the inference router's
# platform MCP ``foundry.memory`` tool. This wrapper is a thin client: it
# forwards intent to ``POST /platform/mcp`` and returns the router's text
# result. A single contract implementation (in the router) means the
# OpenClaw and Hermes runtimes can never drift apart on the wire shape,
# and a custom storeName/scope on the KarsMemory CR is honoured centrally
# without the agent process needing any Foundry knowledge.


def _call_platform_tool(name: str, arguments: dict[str, Any]) -> str:
    """Invoke a router platform-MCP tool via one JSON-RPC ``tools/call``.

    Returns the flattened text content. A JSON-RPC error (unknown tool,
    invalid arguments) or a tool-level ``isError`` is surfaced as the
    returned text so the LLM sees the real reason.
    """
    rpc = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    }
    try:
        resp = router_client.call("POST", "/platform/mcp", json=rpc)
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"{name} request failed: {exc}"})
    if resp.status_code >= 400:
        return json.dumps(
            {"error": f"{name} HTTP {resp.status_code}", "body": resp.text[:500]}
        )
    try:
        payload = resp.json()
    except Exception:  # noqa: BLE001
        return resp.text[:2000]
    if isinstance(payload, dict) and payload.get("error"):
        err = payload["error"] or {}
        reason = (
            (err.get("data") or {}).get("reason")
            or err.get("message")
            or "unknown error"
        )
        return json.dumps({"error": f"{name}: {reason}"})
    result = payload.get("result") if isinstance(payload, dict) else None
    content = result.get("content") if isinstance(result, dict) else None
    if isinstance(content, list):
        texts = [
            c["text"]
            for c in content
            if isinstance(c, dict)
            and c.get("type") == "text"
            and isinstance(c.get("text"), str)
        ]
        if texts:
            return "\n\n".join(texts)
    return json.dumps(result if result is not None else payload)


def _foundry_memory(args: dict[str, Any], **_kwargs: Any) -> str:
    """Persistent agent memory — thin client over the router's
    ``foundry.memory`` platform-MCP tool. The router owns the Memory
    Store contract, store/scope resolution from the KarsMemory binding,
    auto-provision, retry, and CRD status."""
    forwarded: dict[str, Any] = {"operation": str(args.get("operation", "")).strip()}
    text = args.get("text")
    if isinstance(text, str) and text:
        forwarded["text"] = text
    query = args.get("query")
    if isinstance(query, str) and query:
        forwarded["query"] = query
    scope = args.get("scope")
    if isinstance(scope, str) and scope:
        forwarded["scope"] = scope
    top_k = args.get("top_k")
    if isinstance(top_k, int):
        forwarded["top_k"] = top_k
    return _call_platform_tool("foundry.memory", forwarded)


_FOUNDRY_MEMORY_SCHEMA = {
    "name": "foundry_memory",
    "description": (
        "Persistent agent memory backed by Azure AI Foundry Memory Store. "
        "Operations: 'search' (semantic search over past memories matching "
        "scope), 'update' (write a new memory), 'delete_scope' (wipe all "
        "memories under a scope). The store name follows the kars convention "
        "memory-${SANDBOX_NAME} and is auto-provisioned on first use. The "
        "scope defaults to agent_${SANDBOX_NAME}; pass scope to namespace "
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
                    "Memory namespace (default: agent_${SANDBOX_NAME}). "
                    "Use 'session_<id>' for per-conversation memory, "
                    "'user_<id>' for per-user memory across sessions. "
                    "Use '_' as the separator — colons are rejected by the API."
                ),
            },
        },
        "required": ["operation"],
    },
}


_RESPONSES_PATH = f"/openai/responses?api-version={_FOUNDRY_API_VERSION}"


def _extract_response_text(result: Any) -> str:
    """Pull human-readable text out of an /openai/responses payload.

    Mirrors the OpenClaw plugin's parsing at agt-tools/foundry.ts:504-514:
    flattens `output[].content[].{output_text,text}` into a single
    newline-joined string; falls back to JSON.dumps(output) when no
    text parts are present (model used a non-text content type)."""
    output = result.get("output", result) if isinstance(result, dict) else result
    text_parts: list[str] = []
    if isinstance(output, list):
        for item in output:
            if isinstance(item, dict) and item.get("type") == "message":
                for c in item.get("content", []) or []:
                    if isinstance(c, dict) and c.get("type") in {"output_text", "text"}:
                        t = c.get("text")
                        if isinstance(t, str):
                            text_parts.append(t)
    if text_parts:
        return "\n\n".join(text_parts)
    return json.dumps(output, indent=2)


# ── foundry_web_search ────────────────────────────────────────────


_FOUNDRY_WEB_SEARCH_SCHEMA: dict[str, Any] = {
    "type": "object",
    "description": (
        "Search the web in real-time via Azure AI Foundry's Bing grounding. "
        "Returns answers with inline URL citations. Runs server-side — no "
        "egress policy exceptions needed. Use for current events, news, "
        "recent changes, verifying facts, or any query needing up-to-date "
        "information."
    ),
    "properties": {
        "query": {
            "type": "string",
            "description": "The search query or question to look up on the web.",
        },
        "model": {
            "type": "string",
            "description": "Model to use (default: gpt-4.1).",
        },
    },
    "required": ["query"],
}


def _foundry_web_search(args: dict[str, Any], **_kwargs: Any) -> str:
    query = str(args.get("query") or "").strip()
    if not query:
        return json.dumps({"error": "missing required arg: query"})
    model = str(args.get("model") or "gpt-4.1")

    # Resolve the Bing Grounding connection ID, same auto-discovery
    # OpenClaw does at foundry.ts:481. Env override → first
    # GroundingWithBingSearch connection on the Foundry project.
    conn_id = os.environ.get("BING_CONNECTION_ID")
    if not conn_id:
        try:
            conns = router_client.call_json(
                "GET", "/connections?api-version=2025-05-15-preview"
            )
            seq = conns.get("value") if isinstance(conns, dict) else conns
            if isinstance(seq, list):
                for c in seq:
                    if not isinstance(c, dict):
                        continue
                    is_bing = (
                        c.get("type") == "GroundingWithBingSearch"
                        or (
                            isinstance(c.get("properties"), dict)
                            and c["properties"].get("category")
                            == "GroundingWithBingSearch"
                        )
                    )
                    if is_bing:
                        conn_id = c.get("id")
                        break
        except Exception:  # noqa: BLE001 — fall through; the router will 4xx if missing
            pass

    body: dict[str, Any] = {
        "model": model,
        "input": query,
        "tools": [
            {
                "type": "bing_grounding",
                "bing_grounding": {
                    "search_configurations": [
                        {"project_connection_id": conn_id}
                    ]
                },
            }
        ],
        "store": False,
    }
    try:
        result = router_client.call_json("POST", _RESPONSES_PATH, json=body)
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"foundry_web_search failed: {exc}"})
    return _extract_response_text(result)


# ── foundry_code_execute ──────────────────────────────────────────


_FOUNDRY_CODE_EXECUTE_SCHEMA: dict[str, Any] = {
    "type": "object",
    "description": (
        "Execute Python code server-side via Azure AI Foundry's "
        "code_interpreter. Pandas, numpy, matplotlib, scipy pre-installed. "
        "Use for data analysis, charts, complex math, and file processing. "
        "Runs in a managed Foundry sandbox (NOT the local sandbox). "
        "Write any output files (charts/CSV/images) under /mnt/data/<name> — "
        "the tool result includes a downloaded_files list. Do NOT shell out "
        "to copy files into /sandbox/...; the Foundry container cannot see "
        "your filesystem."
    ),
    "properties": {
        "input": {
            "type": "string",
            "description": (
                "Natural language instruction or Python code. To produce a "
                "downloadable artifact, write it to /mnt/data/<filename>."
            ),
        },
        "model": {
            "type": "string",
            "description": "Model to use (default: gpt-4.1).",
        },
    },
    "required": ["input"],
}


def _foundry_code_execute(args: dict[str, Any], **_kwargs: Any) -> str:
    input_str = str(args.get("input") or "").strip()
    if not input_str:
        return json.dumps({"error": "missing required arg: input"})
    # Same guard OpenClaw applies at foundry.ts:89 — block code that
    # writes to /sandbox/ or /tmp/ inside the Foundry container (those
    # paths exist in BOTH containers but are NOT the same FS; the
    # written file vanishes silently). Force /mnt/data/ instead.
    import re as _re

    if _re.search(r"""(["'])(\/sandbox\/|\/tmp\/)""", input_str):
        return json.dumps(
            {
                "error": (
                    "code references '/sandbox/' or '/tmp/' as a destination "
                    "path. Foundry's code-interpreter container has its OWN "
                    "/sandbox and /tmp that are NOT visible to your agent. "
                    "Save files ONLY under /mnt/data/ — the wrapper auto-"
                    "downloads them."
                )
            }
        )

    model = str(args.get("model") or "gpt-4.1")
    body: dict[str, Any] = {
        "model": model,
        "input": input_str,
        "tools": [{"type": "code_interpreter", "container": {"type": "auto"}}],
        # Force tool invocation — without this the model often
        # describes code in prose without running it (foundry.ts:97).
        "tool_choice": {"type": "code_interpreter"},
        "store": False,
    }
    try:
        result = router_client.call_json("POST", _RESPONSES_PATH, json=body)
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"foundry_code_execute failed: {exc}"})
    return _extract_response_text(result)


# ── foundry_image_generation ──────────────────────────────────────


_FOUNDRY_IMAGE_GEN_SCHEMA: dict[str, Any] = {
    "type": "object",
    "description": (
        "Generate an image via Azure AI Foundry's image_generation tool "
        "(gpt-image-1 by default). Returns the image data inline. Use for "
        "diagrams, hero images, illustrations, charts that don't need "
        "specific data."
    ),
    "properties": {
        "prompt": {
            "type": "string",
            "description": "Description of the image to generate.",
        },
        "model": {
            "type": "string",
            "description": "Image model (default: gpt-image-1).",
        },
        "size": {
            "type": "string",
            "description": "Image size, e.g. '1024x1024' (default).",
        },
    },
    "required": ["prompt"],
}


def _foundry_image_generation(args: dict[str, Any], **_kwargs: Any) -> str:
    prompt = str(args.get("prompt") or "").strip()
    if not prompt:
        return json.dumps({"error": "missing required arg: prompt"})
    model = str(args.get("model") or "gpt-image-1")
    size = str(args.get("size") or "1024x1024")
    body: dict[str, Any] = {
        "model": "gpt-4.1",  # orchestration model — picks image_generation
        "input": prompt,
        "tools": [
            {
                "type": "image_generation",
                "image_generation": {"model": model, "size": size},
            }
        ],
        "tool_choice": {"type": "image_generation"},
        "store": False,
    }
    try:
        result = router_client.call_json("POST", _RESPONSES_PATH, json=body)
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"foundry_image_generation failed: {exc}"})
    return _extract_response_text(result)


# ── foundry_file_search ───────────────────────────────────────────


_FOUNDRY_FILE_SEARCH_SCHEMA: dict[str, Any] = {
    "type": "object",
    "description": (
        "Knowledge retrieval (RAG) over Foundry vector stores. Provide a "
        "natural-language query and a vector_store_id; returns relevant "
        "passages with citations. Useful when the operator has pre-uploaded "
        "a document collection and you need to ground answers in it."
    ),
    "properties": {
        "query": {"type": "string", "description": "Question to look up."},
        "vector_store_id": {
            "type": "string",
            "description": (
                "ID of the Foundry vector store to search. Get from the "
                "operator or from a prior foundry_agents call."
            ),
        },
        "model": {
            "type": "string",
            "description": "Model to use (default: gpt-4.1).",
        },
    },
    "required": ["query", "vector_store_id"],
}


def _foundry_file_search(args: dict[str, Any], **_kwargs: Any) -> str:
    query = str(args.get("query") or "").strip()
    vs_id = str(args.get("vector_store_id") or "").strip()
    if not query:
        return json.dumps({"error": "missing required arg: query"})
    if not vs_id:
        return json.dumps({"error": "missing required arg: vector_store_id"})
    model = str(args.get("model") or "gpt-4.1")
    body: dict[str, Any] = {
        "model": model,
        "input": query,
        "tools": [
            {
                "type": "file_search",
                "file_search": {"vector_store_ids": [vs_id]},
            }
        ],
        "tool_choice": {"type": "file_search"},
        "store": False,
    }
    try:
        result = router_client.call_json("POST", _RESPONSES_PATH, json=body)
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"foundry_file_search failed: {exc}"})
    return _extract_response_text(result)


def register(ctx: Any) -> None:  # noqa: ANN401
    """Register the native Foundry tool surface.

    Five tools (memory + web_search + code_execute + image_generation +
    file_search) are registered natively under ``foundry_*`` names so
    the LLM sees the same surface OpenClaw exposes. The remaining four
    Foundry-data-plane tools (conversations, evaluations, deployments,
    agents, download_file) reach the agent via the platform MCP
    server at ``http://127.0.0.1:8443/platform/mcp`` — they are
    operator-tier surfaces rarely invoked directly by an LLM and the
    MCP path is parity-equivalent for tool calling.
    """
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
    ctx.register_tool(
        name="foundry_web_search",
        toolset="foundry",
        schema=_FOUNDRY_WEB_SEARCH_SCHEMA,
        handler=_foundry_web_search,
        description=_FOUNDRY_WEB_SEARCH_SCHEMA["description"],
    )
    ctx.register_tool(
        name="foundry_code_execute",
        toolset="foundry",
        schema=_FOUNDRY_CODE_EXECUTE_SCHEMA,
        handler=_foundry_code_execute,
        description=_FOUNDRY_CODE_EXECUTE_SCHEMA["description"],
    )
    ctx.register_tool(
        name="foundry_image_generation",
        toolset="foundry",
        schema=_FOUNDRY_IMAGE_GEN_SCHEMA,
        handler=_foundry_image_generation,
        description=_FOUNDRY_IMAGE_GEN_SCHEMA["description"],
    )
    ctx.register_tool(
        name="foundry_file_search",
        toolset="foundry",
        schema=_FOUNDRY_FILE_SEARCH_SCHEMA,
        handler=_foundry_file_search,
        description=_FOUNDRY_FILE_SEARCH_SCHEMA["description"],
    )
    logger.info(
        "Foundry tools registered natively (5: memory, web_search, "
        "code_execute, image_generation, file_search). Operator tools "
        "(conversations/evaluations/deployments/agents/download_file) "
        "remain on the platform MCP path."
    )

