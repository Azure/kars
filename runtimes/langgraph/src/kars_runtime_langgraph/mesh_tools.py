# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
First-class AgentMesh tools for LangGraph / LangChain.

Wraps :mod:`mesh` as ``langchain_core.tools.tool``-decorated callables
that LangGraph nodes can register via the standard ``ToolNode`` flow.
See ``runtimes/openai-agents/.../mesh_tools.py`` for the rationale.
"""

from __future__ import annotations

import json
import logging
from typing import Any, List

from kars_runtime_langgraph.mesh import receive_messages, send_message

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
    """Return ``langchain_core.tools.tool``-decorated callables."""
    from langchain_core.tools import tool  # type: ignore

    @tool("mesh_inbox", description=_MESH_INBOX_DESCRIPTION)
    def mesh_inbox() -> str:
        """Drain pending AgentMesh messages."""
        msgs = receive_messages()
        return json.dumps(msgs)

    @tool("mesh_send", description=_MESH_SEND_DESCRIPTION)
    def mesh_send(target_agent: str, content: str, skill_id: str = "chat") -> str:
        """Send a message to a peer agent."""
        ack = send_message(target_agent, content, skill_id=skill_id)
        return json.dumps(ack)

    return [mesh_inbox, mesh_send]
