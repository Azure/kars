# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
AzureClaw in-pod adapter for the OpenAI Agents Python SDK.

Public API:
    bootstrap()                  — idempotent runtime init (called from entrypoint.sh)
    init_telemetry()             — OTel tracer/meter providers + httpx instrumentation
    get_token(scope)             — cached AAD token broker (workload identity)
    register_foundry_tools(agent)— attach the 9 Foundry MCP tools to an agent
    send_message(target, body)   — AgentMesh relay send (A2A TaskEnvelope)
    receive_messages()           — drain inbox for this sandbox identity
"""

from azureclaw_runtime_openai_agents.aad import get_token
from azureclaw_runtime_openai_agents.mesh import (
    MeshClient,
    receive_messages,
    send_message,
)
from azureclaw_runtime_openai_agents.otel import init_telemetry
from azureclaw_runtime_openai_agents.runtime import bootstrap
from azureclaw_runtime_openai_agents.mesh_tools import (
    build_mesh_tools,
    register_mesh_tools,
)
from azureclaw_runtime_openai_agents.tools import (
    FOUNDRY_TOOL_NAMES,
    register_foundry_tools,
)

__version__ = "0.1.0"

__all__ = [
    "FOUNDRY_TOOL_NAMES",
    "MeshClient",
    "__version__",
    "bootstrap",
    "build_mesh_tools",
    "get_token",
    "init_telemetry",
    "receive_messages",
    "register_foundry_tools",
    "register_mesh_tools",
    "send_message",
]
