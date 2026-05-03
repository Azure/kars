# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
First-class AgentMesh tools for the Microsoft Agent Framework Python SDK.

Wraps :mod:`mesh` as ``agent_framework.ai_function``-decorated callables
so MAF agents pick them up automatically. See
``runtimes/openai-agents/.../mesh_tools.py`` for the design rationale.
"""

from __future__ import annotations

import json
import logging
from typing import Any, List

from azureclaw_runtime_maf_python.mesh import receive_messages, send_message

logger = logging.getLogger(__name__)


_MESH_INBOX_DESCRIPTION = (
    "Drain pending AgentMesh messages addressed to this agent. "
    "ALWAYS call this tool FIRST whenever your task description says a "
    "peer agent has sent you data, when you were just resumed after a "
    "handoff, or when you suspect a parent agent is waiting for a reply. "
    "Returns a JSON array of message envelopes; an empty array means no "
    "pending messages. Calling this is cheap and idempotent — when in "
    "doubt, call it."
)

_MESH_SEND_DESCRIPTION = (
    "Send an AgentMesh A2A TaskEnvelope to a peer agent identified by "
    "their DID (e.g. ``did:mesh:<name>``). Use this to delegate work to a "
    "sub-agent or reply to a parent agent. Returns the relay's "
    "acknowledgement."
)


def build_mesh_tools() -> List[Any]:
    """Return ``ai_function``-decorated callables for the mesh tools."""
    from agent_framework import ai_function  # type: ignore

    @ai_function(name="mesh_inbox", description=_MESH_INBOX_DESCRIPTION)
    async def mesh_inbox() -> str:
        msgs = receive_messages()
        return json.dumps(msgs)

    @ai_function(name="mesh_send", description=_MESH_SEND_DESCRIPTION)
    async def mesh_send(target_agent: str, content: str, skill_id: str = "chat") -> str:
        ack = send_message(target_agent, content, skill_id=skill_id)
        return json.dumps(ack)

    mesh_inbox.__azureclaw_tool_name__ = "mesh_inbox"  # type: ignore[attr-defined]
    mesh_send.__azureclaw_tool_name__ = "mesh_send"  # type: ignore[attr-defined]
    return [mesh_inbox, mesh_send]


def register_mesh_tools(agent: Any) -> None:
    """Attach mesh tools to *agent*'s ``tools`` list (idempotent)."""
    if not hasattr(agent, "tools"):
        raise TypeError(
            "agent must expose a mutable `tools` list (got "
            f"{type(agent).__name__})"
        )
    existing = set()
    for tool in agent.tools or []:
        nm = getattr(tool, "name", None) or getattr(
            tool, "__azureclaw_tool_name__", None
        )
        if nm:
            existing.add(nm)
    new = [
        t
        for t in build_mesh_tools()
        if (getattr(t, "name", None) or getattr(t, "__azureclaw_tool_name__", None))
        not in existing
    ]
    if agent.tools is None:
        agent.tools = []
    agent.tools = list(agent.tools) + new
    logger.info("registered %d mesh tools on agent", len(new))
