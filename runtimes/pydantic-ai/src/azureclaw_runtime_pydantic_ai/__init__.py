# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
AzureClaw in-pod adapter for Pydantic-AI.

Public API:
    bootstrap()                  — idempotent runtime init (called from entrypoint.sh)
    init_telemetry()             — OTel tracer/meter providers + httpx instrumentation
    get_token(scope)             — cached AAD token broker (workload identity)
    send_message(target, body)   — AgentMesh relay send (A2A TaskEnvelope)
    receive_messages()           — drain inbox for this sandbox identity

Foundry tools (web_search, code_execute, file_search, memory, image_generation,
conversations, evaluations, deployments, agents) are reachable via the
in-cluster MCP server at `http://127.0.0.1:8443/platform/mcp`. Pydantic-AI
agents can call this via any standard MCP client.
"""

from azureclaw_runtime_pydantic_ai.aad import get_token
from azureclaw_runtime_pydantic_ai.mesh import (
    MeshClient,
    receive_messages,
    send_message,
)
from azureclaw_runtime_pydantic_ai.otel import init_telemetry
from azureclaw_runtime_pydantic_ai.runtime import bootstrap

__version__ = "0.1.0"

PLATFORM_MCP_URL = "http://127.0.0.1:8443/platform/mcp"

__all__ = [
    "MeshClient",
    "PLATFORM_MCP_URL",
    "__version__",
    "bootstrap",
    "get_token",
    "init_telemetry",
    "receive_messages",
    "send_message",
]
