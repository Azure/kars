# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""
AgentMesh bridge for the in-pod adapter.

`a2a_agentmesh` ships A2A *types* (AgentCard, TaskEnvelope, TrustGate)
but no transport — the relay/registry are HTTP services. The router
sidecar reverse-proxies them at `/agt/relay` and `/agt/registry` so we
have one endpoint for governance/auth.

This module is a thin transport over those endpoints. It serializes a
`TaskEnvelope` for outbound messages and deserializes inbox payloads
back to dicts (we keep envelopes opaque to the user — they only see the
content). When the upstream `a2a_agentmesh` package is available we use
its types; if not, the module degrades to a dict-only shim so the rest
of the adapter still imports.
"""

from __future__ import annotations

import logging
import os
import time
from typing import Any, Dict, List, Optional
from urllib.parse import urljoin

import httpx

logger = logging.getLogger(__name__)

DEFAULT_RELAY_URL = "http://127.0.0.1:8443/agt/relay/"
DEFAULT_REGISTRY_URL = "http://127.0.0.1:8443/agt/registry/"
DEFAULT_TIMEOUT_SECONDS = 10.0

# Identity headers — populated by the controller via the pod env. The
# router uses them to authorize the sandbox identity against AGT.
ENV_AGENT_DID = "AZURECLAW_AGENT_DID"
ENV_AGENT_NAME = "AZURECLAW_AGENT_NAME"

try:  # pragma: no cover - import guard exercised in degraded test
    from a2a_agentmesh import TaskEnvelope, TaskMessage, TaskState  # type: ignore

    _HAS_A2A = True
except Exception:  # pragma: no cover - exercised only when wheel missing
    _HAS_A2A = False
    TaskEnvelope = None  # type: ignore[assignment]
    TaskMessage = None  # type: ignore[assignment]
    TaskState = None  # type: ignore[assignment]


def _env_did() -> str:
    return os.environ.get(ENV_AGENT_DID, "did:mesh:unknown")


def _env_name() -> str:
    return os.environ.get(ENV_AGENT_NAME, "azureclaw-agent")


def _build_envelope(
    target_did: str,
    content: str,
    skill_id: str = "chat",
) -> Dict[str, Any]:
    """Return a JSON-serializable A2A task envelope.

    Uses the upstream `a2a_agentmesh` types when present; falls back to
    a hand-rolled dict that is wire-compatible with the relay so the
    transport keeps working in degraded environments.
    """
    if _HAS_A2A:
        envelope = TaskEnvelope.create(
            skill_id=skill_id,
            source_did=_env_did(),
            target_did=target_did,
            source_trust_score=0,
            input_text=content,
        )
        # TaskEnvelope.to_dict() exists on the upstream type.
        if hasattr(envelope, "to_dict"):
            return envelope.to_dict()  # type: ignore[no-any-return]
        # Defensive: dataclass fallback.
        return {
            "task_id": getattr(envelope, "task_id", None),
            "state": "submitted",
            "skill_id": skill_id,
            "source_did": _env_did(),
            "target_did": target_did,
            "messages": [{"role": "user", "parts": [{"type": "text/plain", "text": content}]}],
        }
    # Degraded shim — wire-compatible enough for the relay to accept it.
    return {
        "task_id": f"task-{int(time.time() * 1000)}",
        "state": "submitted",
        "skill_id": skill_id,
        "source_did": _env_did(),
        "target_did": target_did,
        "messages": [
            {"role": "user", "parts": [{"type": "text/plain", "text": content}]},
        ],
        "created_at": time.time(),
    }


class MeshClient:
    """HTTP client for the AgentMesh relay/registry, reverse-proxied by the router."""

    def __init__(
        self,
        relay_url: Optional[str] = None,
        registry_url: Optional[str] = None,
        *,
        client: Optional[httpx.Client] = None,
        timeout: float = DEFAULT_TIMEOUT_SECONDS,
    ) -> None:
        self.relay_url = (relay_url or os.environ.get("AZURECLAW_AGT_RELAY_URL") or DEFAULT_RELAY_URL)
        if not self.relay_url.endswith("/"):
            self.relay_url = self.relay_url + "/"
        self.registry_url = (
            registry_url
            or os.environ.get("AZURECLAW_AGT_REGISTRY_URL")
            or DEFAULT_REGISTRY_URL
        )
        if not self.registry_url.endswith("/"):
            self.registry_url = self.registry_url + "/"
        self._client = client or httpx.Client(timeout=timeout)
        self._owns_client = client is None

    def close(self) -> None:
        if self._owns_client:
            self._client.close()

    def __enter__(self) -> "MeshClient":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()

    def send(self, target_agent: str, content: str, *, skill_id: str = "chat") -> Dict[str, Any]:
        envelope = _build_envelope(target_agent, content, skill_id=skill_id)
        url = urljoin(self.relay_url, "send")
        resp = self._client.post(url, json=envelope)
        resp.raise_for_status()
        return resp.json()

    def receive(self) -> List[Dict[str, Any]]:
        url = urljoin(self.relay_url, "inbox")
        params = {"agent_did": _env_did()}
        resp = self._client.get(url, params=params)
        resp.raise_for_status()
        body = resp.json()
        # Relay returns either {"messages": [...]} or a bare list — accept both.
        if isinstance(body, dict):
            return list(body.get("messages", []))
        if isinstance(body, list):
            return body
        return []

    def lookup(self, agent_name: str) -> Optional[Dict[str, Any]]:
        """Resolve a friendly agent name to its registry record (incl. DID)."""
        url = urljoin(self.registry_url, "lookup")
        resp = self._client.get(url, params={"name": agent_name})
        if resp.status_code == 404:
            return None
        resp.raise_for_status()
        return resp.json()


# Module-level convenience API ------------------------------------------------

_default_client: Optional[MeshClient] = None


def _client() -> MeshClient:
    global _default_client
    if _default_client is None:
        _default_client = MeshClient()
    return _default_client


def send_message(target_agent: str, content: str, *, skill_id: str = "chat") -> Dict[str, Any]:
    """Send `content` to `target_agent` via the AgentMesh relay."""
    return _client().send(target_agent, content, skill_id=skill_id)


def receive_messages() -> List[Dict[str, Any]]:
    """Drain the inbox for the current sandbox identity."""
    return _client().receive()


def reset_default_client() -> None:
    """Test hook — drop the cached MeshClient."""
    global _default_client
    if _default_client is not None:
        _default_client.close()
    _default_client = None
