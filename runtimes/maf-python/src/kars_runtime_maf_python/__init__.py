# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
kars in-pod adapter for the Microsoft Agent Framework Python SDK.

Public API mirrors `kars_runtime_openai_agents` so cross-runtime
helpers can call either adapter the same way.
"""

from kars_runtime_maf_python.aad import get_token
from kars_runtime_maf_python.mesh import (
    MeshClient,
    receive_messages,
    send_message,
)
from kars_runtime_maf_python.otel import init_telemetry
from kars_runtime_maf_python.runtime import bootstrap
from kars_runtime_maf_python.mesh_tools import (
    build_mesh_tools,
    register_mesh_tools,
)
from kars_runtime_maf_python.tools import (
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
