# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
First-class AgentMesh tools for the Anthropic Claude Agent SDK.

Wraps :mod:`mesh` as ``claude_agent_sdk.tool``-decorated callables. See
``runtimes/openai-agents/.../mesh_tools.py`` for the rationale.
"""

from __future__ import annotations

import json
import logging
from typing import Any, List

from azureclaw_runtime_anthropic.mesh import receive_messages, send_message

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
    """Return ``claude_agent_sdk`` tool definitions."""
    from claude_agent_sdk import tool  # type: ignore

    @tool(
        name="mesh_inbox",
        description=_MESH_INBOX_DESCRIPTION,
        input_schema={"type": "object", "properties": {}, "required": []},
    )
    async def mesh_inbox(_args: dict) -> dict:
        msgs = receive_messages()
        return {"content": [{"type": "text", "text": json.dumps(msgs)}]}

    @tool(
        name="mesh_send",
        description=_MESH_SEND_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "target_agent": {"type": "string"},
                "content": {"type": "string"},
                "skill_id": {"type": "string", "default": "chat"},
            },
            "required": ["target_agent", "content"],
        },
    )
    async def mesh_send(args: dict) -> dict:
        ack = send_message(
            args["target_agent"],
            args["content"],
            skill_id=args.get("skill_id", "chat"),
        )
        return {"content": [{"type": "text", "text": json.dumps(ack)}]}

    return [mesh_inbox, mesh_send]
