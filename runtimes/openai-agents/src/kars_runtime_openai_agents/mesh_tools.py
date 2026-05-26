# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
First-class AgentMesh tools for the OpenAI Agents SDK.

The plain :mod:`mesh` module exposes ``send_message`` / ``receive_messages``
as Python functions so user code can call them directly. This module
wraps them as ``agents.function_tool`` objects so they become first-class
tools the agent can pick up automatically — same UX as the Foundry tools
in :mod:`tools`.

Two tools are registered:

* ``mesh_send(target_agent, content, skill_id)`` — send an A2A
  ``TaskEnvelope`` to a peer agent via the relay.
* ``mesh_inbox()`` — drain pending messages for the current sandbox
  identity. The description includes the explicit "INBOX-FIRST" nudge:
  models routinely fabricate a fresh response instead of reading the
  parent's KNOCK message without it.
"""

from __future__ import annotations

import json
import logging
from typing import Any, List

from kars_runtime_openai_agents.mesh import receive_messages, send_message

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
    """Return ``function_tool`` objects for ``mesh_send`` + ``mesh_inbox``."""
    from agents import function_tool  # type: ignore

    @function_tool(name_override="mesh_inbox", description_override=_MESH_INBOX_DESCRIPTION)
    async def mesh_inbox() -> str:
        """Return pending AgentMesh messages as a JSON string."""
        msgs = receive_messages()
        return json.dumps(msgs)

    @function_tool(name_override="mesh_send", description_override=_MESH_SEND_DESCRIPTION)
    async def mesh_send(target_agent: str, content: str, skill_id: str = "chat") -> str:
        """Send ``content`` to ``target_agent`` (DID) via the relay."""
        ack = send_message(target_agent, content, skill_id=skill_id)
        return json.dumps(ack)

    # Stash the canonical names for introspection.
    mesh_inbox.__kars_tool_name__ = "mesh_inbox"  # type: ignore[attr-defined]
    mesh_send.__kars_tool_name__ = "mesh_send"  # type: ignore[attr-defined]
    return [mesh_inbox, mesh_send]


def register_mesh_tools(agent: Any) -> None:
    """Attach mesh tools to *agent*'s tool list (idempotent per agent)."""
    if not hasattr(agent, "tools"):
        raise TypeError(
            "agent must expose a mutable `tools` list (got "
            f"{type(agent).__name__})"
        )
    existing = set()
    for tool in agent.tools or []:
        nm = getattr(tool, "name", None) or getattr(
            tool, "__kars_tool_name__", None
        )
        if nm:
            existing.add(nm)
    new = []
    for tool in build_mesh_tools():
        nm = getattr(tool, "name", None) or getattr(
            tool, "__kars_tool_name__", None
        )
        if nm in existing:
            continue
        new.append(tool)
    if agent.tools is None:
        agent.tools = []
    agent.tools = list(agent.tools) + new
    logger.info("registered %d mesh tools on agent", len(new))
