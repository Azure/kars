# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Exceptions raised by kars-agt-mesh. All inherit from
:class:`KarsAgtMeshError` so callers can catch the whole family at
once with a single except clause."""

from __future__ import annotations


class KarsAgtMeshError(Exception):
    """Base class for every kars-agt-mesh exception."""


class MeshTransportError(KarsAgtMeshError):
    """Raised when the relay WebSocket fails to connect, disconnects
    unexpectedly, or returns a protocol-level error.

    The library auto-reconnects on transient transport errors; this
    exception only surfaces when reconnect attempts have been
    exhausted, when the caller explicitly disabled retry, or when the
    error is non-recoverable (e.g. relay rejected the registration
    token)."""


class MeshAuthError(KarsAgtMeshError):
    """Raised when the registry rejects our Ed25519-Timestamp auth
    header (clock skew, malformed signature, unknown DID).

    Distinct from :class:`MeshTransportError` because the operator's
    remediation is different: auth errors mean fix the identity store
    / NTP / registry config, transport errors mean fix network."""


class MeshPeerNotFoundError(KarsAgtMeshError):
    """Raised by :meth:`MeshClient.send_by_name` when the registry
    has no agent with the requested display name registered."""


class MeshRegistryError(KarsAgtMeshError):
    """Raised when the registry returns an unexpected HTTP status (5xx,
    malformed JSON, etc.) that isn't an auth or not-found error."""


class MeshTrustGateError(KarsAgtMeshError):
    """Raised when a peer's trust score is below the configured
    threshold during KNOCK acceptance.

    Wraps the peer's DID + observed score so callers can log /
    surface the gate decision."""

    def __init__(self, peer_did: str, peer_score: int, threshold: int) -> None:
        self.peer_did = peer_did
        self.peer_score = peer_score
        self.threshold = threshold
        super().__init__(
            f"Peer {peer_did} has trust score {peer_score} < threshold {threshold}"
        )
