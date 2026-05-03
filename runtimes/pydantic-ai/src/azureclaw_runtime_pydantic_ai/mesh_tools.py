# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
First-class AgentMesh tools for Pydantic-AI.

Wraps :mod:`mesh` as ``pydantic_ai.Tool`` instances. See
``runtimes/openai-agents/.../mesh_tools.py`` for the rationale.
"""

from __future__ import annotations

import json
import logging
from typing import Any, List

from azureclaw_runtime_pydantic_ai.mesh import receive_messages, send_message

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


def _mesh_inbox_impl() -> str:
    """Drain pending AgentMesh messages."""
    return json.dumps(receive_messages())


def _mesh_send_impl(target_agent: str, content: str, skill_id: str = "chat") -> str:
    """Send a message to a peer agent."""
    return json.dumps(send_message(target_agent, content, skill_id=skill_id))


def build_mesh_tools() -> List[Any]:
    """Return ``pydantic_ai.Tool`` instances for the mesh tools."""
    from pydantic_ai import Tool  # type: ignore

    return [
        Tool(
            _mesh_inbox_impl,
            name="mesh_inbox",
            description=_MESH_INBOX_DESCRIPTION,
        ),
        Tool(
            _mesh_send_impl,
            name="mesh_send",
            description=_MESH_SEND_DESCRIPTION,
        ),
    ]
