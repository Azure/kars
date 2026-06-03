"""kars_mesh_* tool stubs — return informative errors.

The real implementations land in Act 2 once the Python AGT MeshClient
ships at TypeScript parity. Until then, every mesh tool returns:

    {"error": "Mesh not available in Hermes runtime v0.5.2; requires Python
              AGT MeshClient (Act 2). See docs/runtimes/CONTRACT.md."}

This is intentional — the contract says runtimes without mesh ship the
tools as clear-error stubs rather than omitting them, so the LLM gets a
helpful message instead of "unknown tool".
"""

from __future__ import annotations

import json
from typing import Any

_ACT2_ERROR = {
    "error": (
        "Mesh communication not available in Hermes runtime v0.5.2. "
        "kars_mesh_* tools require the Python AGT MeshClient which is "
        "scheduled for Hermes v0.5.3 (Act 2 — Python AGT SDK at TypeScript "
        "parity). Until then, coordinate with peer agents through shared "
        "Foundry Memory Store or Foundry Conversations. "
        "See docs/runtimes/CONTRACT.md § Cross-runtime mesh compatibility."
    ),
    "hermes_version": "0.5.2",
    "available_in": "0.5.3",
}


def _stub(_args: dict, **_kwargs: Any) -> str:
    return json.dumps(_ACT2_ERROR)


_MESH_TOOLS = [
    ("kars_mesh_send", "Send a mesh message to a peer agent (NOT available in v0.5.2 — see error message)."),
    ("kars_mesh_inbox", "Drain mesh inbox (NOT available in v0.5.2 — see error message)."),
    ("kars_mesh_await", "Block until peer message arrives (NOT available in v0.5.2 — see error message)."),
    ("kars_mesh_transfer_file", "Transfer file via mesh (NOT available in v0.5.2 — see error message)."),
]


def register(ctx: Any) -> None:  # noqa: ANN401
    """Register clear-error stubs for the mesh tools."""
    for name, desc in _MESH_TOOLS:
        ctx.register_tool(
            name=name,
            toolset="kars_mesh_stub",
            schema={
                "name": name,
                "description": desc,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "target_agent": {"type": "string"},
                        "payload": {"type": "string"},
                    },
                    # No required fields — the tool always errors; the LLM
                    # discovers the limitation from the error message.
                    "required": [],
                },
            },
            handler=_stub,
            description=desc,
        )
