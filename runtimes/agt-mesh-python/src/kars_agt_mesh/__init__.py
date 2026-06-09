# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""
kars-agt-mesh — Python AGT MeshClient for any Python agent framework.

The upstream Microsoft Agent Governance Toolkit (AGT) ships a full
E2E encrypted mesh client in TypeScript. The Python distribution
ships only the crypto primitives (X3DH, Double Ratchet, SecureChannel)
and the relay+registry servers — no client. This package fills that
gap with a runtime-neutral library every Python agent framework can
import.

Public API:

    from kars_agt_mesh import MeshClient, MeshConfig, InboundMessage

    async with MeshClient(MeshConfig(name="my-agent", ...)) as mesh:
        await mesh.send_by_name(to="peer", payload=b"hello")
        async for msg in mesh.inbox():
            handle(msg.from_did, msg.payload)
"""

from .client import MeshClient
from .config import MeshConfig
from .errors import (
    KarsAgtMeshError,
    MeshAuthError,
    MeshPeerNotFoundError,
    MeshRegistryError,
    MeshTransportError,
    MeshTrustGateError,
)
from .messages import InboundMessage

__version__ = "0.1.0"

__all__ = [
    "InboundMessage",
    "KarsAgtMeshError",
    "MeshAuthError",
    "MeshClient",
    "MeshConfig",
    "MeshPeerNotFoundError",
    "MeshRegistryError",
    "MeshTransportError",
    "MeshTrustGateError",
    "__version__",
]
