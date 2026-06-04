# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Configuration for MeshClient. Runtime-neutral — never references
Hermes, kars-specific paths, or any single framework's conventions."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class MeshConfig:
    """Immutable configuration for a single MeshClient instance.

    All paths and URLs are explicit; the library never reads env vars
    so callers can compose configurations from any source (env, file,
    CLI flags, K8s downward API, secrets manager).
    """

    name: str
    """Display name registered with the relay (DNS-label, ≤63 chars)."""

    relay_url: str
    """WebSocket URL of the AGT relay (e.g. ``ws://agentmesh-relay:8765``).
    Both ``ws://`` and ``wss://`` are supported."""

    registry_url: str
    """HTTP URL of the AGT registry (e.g.
    ``http://agentmesh-registry:8080``)."""

    identity_path: Path
    """Filesystem path to the persistent Ed25519 + X25519 identity JSON.
    Created on first run if absent; loaded every subsequent boot so
    the DID is stable across pod restarts."""

    trust_threshold: int = 0
    """Minimum trust score required to accept a KNOCK from a peer.
    0 = accept all (default; appropriate for dev). Production should
    set this to 500+ to enforce per-tenant trust."""

    heartbeat_interval_seconds: float = 30.0
    """How often to send relay heartbeats so the registry knows this
    agent is alive. Matches the TS SDK default."""

    reconnect_initial_seconds: float = 1.0
    """Initial backoff after a relay disconnect (doubles each retry,
    capped at ``reconnect_max_seconds``)."""

    reconnect_max_seconds: float = 30.0
    """Cap on the exponential backoff."""

    http_timeout_seconds: float = 10.0
    """Default timeout for registry HTTP calls."""

    user_agent: str = "kars-agt-mesh/0.1.0"
    """User-Agent header for HTTP/WS requests. Override per-runtime
    (e.g. ``"kars-agt-mesh/0.1.0 (hermes/0.15.2)"``) to make
    server-side logs attribute traffic to the right framework."""

    def __post_init__(self) -> None:
        if not self.name or len(self.name) > 63:
            raise ValueError(
                f"MeshConfig.name must be 1..63 chars, got {len(self.name)}"
            )
        # DNS-label rule: lowercase alphanumeric + hyphens. Underscores
        # are not DNS-label-safe but accepted by the AGT registry, so
        # we allow them with a warning. Uppercase is rejected because
        # the registry lowercases internally and we want callers to
        # see the surprise at construction time, not at register_self.
        _allowed = set("abcdefghijklmnopqrstuvwxyz0123456789-_")
        if any(c not in _allowed for c in self.name):
            raise ValueError(
                f"MeshConfig.name must be DNS-label (lowercase alnum + "
                f"-/_), got {self.name!r}"
            )
        if not (
            self.relay_url.startswith("ws://") or self.relay_url.startswith("wss://")
        ):
            raise ValueError(
                f"MeshConfig.relay_url must start with ws:// or wss://, "
                f"got {self.relay_url!r}"
            )
        if not (
            self.registry_url.startswith("http://")
            or self.registry_url.startswith("https://")
        ):
            raise ValueError(
                f"MeshConfig.registry_url must start with http:// or https://, "
                f"got {self.registry_url!r}"
            )
        if self.trust_threshold < 0:
            raise ValueError(
                f"MeshConfig.trust_threshold must be >= 0, "
                f"got {self.trust_threshold}"
            )
        if self.heartbeat_interval_seconds <= 0:
            raise ValueError(
                f"MeshConfig.heartbeat_interval_seconds must be > 0, "
                f"got {self.heartbeat_interval_seconds}"
            )
