# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
Kars in-pod adapter for LangGraph (LangChain).

Public API:
    bootstrap()                  — idempotent runtime init (called from entrypoint.sh)
    init_telemetry()             — OTel tracer/meter providers + httpx instrumentation
    get_token(scope)             — cached AAD token broker (workload identity)
    send_message(target, body)   — AgentMesh relay send (A2A TaskEnvelope)
    receive_messages()           — drain inbox for this sandbox identity

Foundry tools (web_search, code_execute, file_search, memory, image_generation,
conversations, evaluations, deployments, agents) are reachable via the
in-cluster MCP server at `http://127.0.0.1:8443/platform/mcp`. LangGraph
nodes can call this via any standard MCP client.
"""

from kars_runtime_langgraph.aad import get_token
from kars_runtime_langgraph.mesh import (
    MeshClient,
    receive_messages,
    send_message,
)
from kars_runtime_langgraph.mesh_tools import build_mesh_tools
from kars_runtime_langgraph.otel import init_telemetry
from kars_runtime_langgraph.runtime import bootstrap

__version__ = "0.1.0"

PLATFORM_MCP_URL = "http://127.0.0.1:8443/platform/mcp"

__all__ = [
    "MeshClient",
    "PLATFORM_MCP_URL",
    "__version__",
    "bootstrap",
    "build_mesh_tools",
    "get_token",
    "init_telemetry",
    "receive_messages",
    "send_message",
]
