# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""HTTP client for the AGT registry.

Wire-spec: ``docs/specs/AGENTMESH-WIRE-1.0.md`` §11 (in the upstream
``microsoft/agent-governance-toolkit`` repo). This client speaks the
exact same protocol as ``@microsoft/agent-governance-sdk``'s
``RegistryClient`` — the TS implementation is the authoritative
reference for what fields go where; this is a faithful Python port.
"""

from __future__ import annotations

import base64
import logging
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any

import httpx
from nacl import signing

from .errors import MeshAuthError, MeshPeerNotFoundError, MeshRegistryError

logger = logging.getLogger("kars_agt_mesh.registry")


def _b64url(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode("ascii")


def _iso_utc() -> str:
    """RFC 3339 UTC timestamp with millisecond precision, no offset suffix.
    Mirrors the TS SDK's `new Date().toISOString()` output to keep
    Ed25519-Timestamp signature inputs byte-identical."""
    return (
        datetime.now(timezone.utc)
        .isoformat(timespec="milliseconds")
        .replace("+00:00", "Z")
    )


@dataclass(frozen=True)
class PeerBundle:
    """A peer's prekey bundle, as returned by
    ``GET /v1/agents/{did}/prekeys``. Shape matches AGT Python's
    flat ``PreKeyBundle`` — the registry consumes ONE OTK per fetch
    and returns it as a single ``one_time_pre_key`` dict (or None).
    """

    did: str
    identity_key_x25519: bytes
    identity_key_ed25519: bytes
    signed_pre_key_public: bytes
    signed_pre_key_signature: bytes
    signed_pre_key_id: int
    one_time_pre_key_public: bytes | None
    one_time_pre_key_id: int | None

    @classmethod
    def from_response(cls, did: str, body: dict[str, Any]) -> PeerBundle:
        spk = body["signed_pre_key"]
        otp_raw = body.get("one_time_pre_key")
        otp_pub: bytes | None = None
        otp_id: int | None = None
        if otp_raw:
            otp_pub = _b64url_decode(otp_raw["public_key"])
            otp_id = int(otp_raw["key_id"])

        identity_ed_raw = body.get("identity_key_ed") or ""
        return cls(
            did=did,
            identity_key_x25519=_b64url_decode(body["identity_key"]),
            identity_key_ed25519=_b64url_decode(identity_ed_raw) if identity_ed_raw else b"",
            signed_pre_key_public=_b64url_decode(spk["public_key"]),
            signed_pre_key_signature=_b64url_decode(spk["signature"]),
            signed_pre_key_id=int(spk["key_id"]),
            one_time_pre_key_public=otp_pub,
            one_time_pre_key_id=otp_id,
        )


@dataclass(frozen=True)
class DiscoveredAgent:
    """One entry in a ``/v1/discover?capability=...`` result."""

    did: str
    capabilities: list[str]
    metadata: dict[str, str]
    last_seen: str = ""
    reputation_score: float = 0.0

    @property
    def display_name(self) -> str | None:
        return self.metadata.get("display_name")


def _b64url_decode(s: str) -> bytes:
    pad = "=" * (-len(s) % 4)
    return base64.urlsafe_b64decode(s + pad)


class RegistryClient:
    """Async HTTP client for the AGT registry.

    Wire endpoints used:

    - ``POST   /v1/agents``                — register self
    - ``PUT    /v1/agents/{did}/prekeys``  — upload our prekey bundle
    - ``GET    /v1/agents/{did}``          — fetch peer metadata
    - ``GET    /v1/agents/{did}/prekeys``  — fetch peer prekey bundle
    - ``GET    /v1/discover``              — search by capability
    - ``POST   /v1/agents/{did}/heartbeat``— liveness ping

    All authenticated writes (PUT/POST except register) carry an
    ``Ed25519-Timestamp`` header pair the upstream AGT registry
    requires (see ``registry/app.py`` proof verification logic).
    """

    def __init__(
        self,
        *,
        base_url: str,
        identity_signing_key: signing.SigningKey,
        identity_did: str,
        timeout_seconds: float = 10.0,
        user_agent: str = "kars-agt-mesh/0.1.0",
    ) -> None:
        self._base_url = base_url.rstrip("/")
        self._signing_key = identity_signing_key
        self._identity_did = identity_did
        # Reuse the HTTP/2 connection pool across all calls.
        self._client = httpx.AsyncClient(
            base_url=self._base_url,
            timeout=timeout_seconds,
            headers={"User-Agent": user_agent},
        )

    async def aclose(self) -> None:
        await self._client.aclose()

    async def __aenter__(self) -> RegistryClient:
        return self

    async def __aexit__(self, *_exc: object) -> None:
        await self.aclose()

    # ── Self-registration ────────────────────────────────────────────────

    async def register_self(
        self,
        *,
        capabilities: list[str],
        metadata: dict[str, str],
    ) -> None:
        """``POST /v1/agents`` — register our Ed25519 public key.

        Proof = Ed25519 signature over the ASCII bytes of
        ``base64url(public_key) || iso_timestamp`` (the b64url string
        + the timestamp string, NOT the raw key bytes). AGT registry's
        verifier (``registry/app.py::register_agent``) reconstructs
        the message as ``req.public_key.encode() + req.proof_timestamp.encode()``,
        so we have to match that exactly. The registry replay window
        is 5 minutes by default.
        """
        public_key = self._signing_key.verify_key.encode()
        public_key_b64 = _b64url(public_key)
        ts = _iso_utc()
        proof_input = public_key_b64.encode("ascii") + ts.encode("ascii")
        proof = self._signing_key.sign(proof_input).signature

        body = {
            "public_key": public_key_b64,
            "proof": _b64url(proof),
            "proof_timestamp": ts,
            "capabilities": capabilities,
            "metadata": metadata,
        }
        resp = await self._client.post("/v1/agents", json=body)
        if resp.status_code == 201:
            logger.info(
                "Registered self did=%s capabilities=%s",
                self._identity_did,
                capabilities,
            )
            return
        if resp.status_code == 409:
            # Already registered — happens on every restart when the
            # identity file is persisted across pod recreations. The
            # subsequent prekey PUT (which carries Ed25519-Timestamp
            # auth proving we own the same key) is enough to refresh
            # our session; we don't need to re-create the agent record.
            logger.info(
                "Registry already has did=%s — reusing existing record",
                self._identity_did,
            )
            return
        if resp.status_code in {400, 401, 403}:
            raise MeshAuthError(
                f"Registry rejected register_self: {resp.status_code} {resp.text[:200]}"
            )
        raise MeshRegistryError(
            f"register_self failed: HTTP {resp.status_code} {resp.text[:200]}"
        )

    # ── Prekey bundle management ────────────────────────────────────────

    async def upload_prekeys(
        self,
        *,
        identity_key_x25519: bytes,
        identity_key_ed25519: bytes,
        signed_pre_key: dict[str, Any],
        one_time_pre_keys: list[dict[str, Any]],
    ) -> None:
        """``PUT /v1/agents/{did}/prekeys`` — upload our prekey bundle.

        Carries Ed25519-Timestamp auth so the registry knows the
        request came from the bundle's owner."""
        body = {
            "identity_key": _b64url(identity_key_x25519),
            "identity_key_ed": _b64url(identity_key_ed25519),
            "signed_pre_key": signed_pre_key,
            "one_time_pre_keys": one_time_pre_keys,
        }
        headers = self._auth_headers("PUT", f"/v1/agents/{self._identity_did}/prekeys")
        resp = await self._client.put(
            f"/v1/agents/{self._identity_did}/prekeys",
            json=body,
            headers=headers,
        )
        if resp.status_code in {200, 204}:
            logger.info(
                "Uploaded %d one-time prekeys for did=%s",
                len(one_time_pre_keys),
                self._identity_did,
            )
            return
        if resp.status_code in {401, 403}:
            raise MeshAuthError(
                f"Registry rejected prekey upload: {resp.status_code} {resp.text[:200]}"
            )
        raise MeshRegistryError(
            f"upload_prekeys failed: HTTP {resp.status_code} {resp.text[:200]}"
        )

    async def fetch_prekeys(self, peer_did: str) -> PeerBundle:
        """``GET /v1/agents/{did}/prekeys`` — fetch a peer's bundle.

        The registry pops a one-time prekey on each fetch so each
        X3DH handshake is forward-secure. Re-fetch returns a new
        OTP-key as long as the peer keeps the bundle replenished."""
        resp = await self._client.get(f"/v1/agents/{peer_did}/prekeys")
        if resp.status_code == 200:
            return PeerBundle.from_response(peer_did, resp.json())
        if resp.status_code == 404:
            raise MeshPeerNotFoundError(
                f"Peer {peer_did} has no published prekey bundle"
            )
        raise MeshRegistryError(
            f"fetch_prekeys({peer_did}) failed: HTTP {resp.status_code} {resp.text[:200]}"
        )

    # ── Discovery ──────────────────────────────────────────────────────

    async def discover(self, capability: str, *, limit: int = 50) -> list[DiscoveredAgent]:
        """``GET /v1/discover?capability=...`` — search agents.

        The AGT convention is to register display name as a capability
        string, so this also serves as ``find_by_display_name``.

        Returns agents in **freshest-first** order (most recent
        ``last_seen`` first) so callers picking the first hit get the
        live peer when stale registry entries exist for the same
        capability — a common situation when sandboxes restart and
        the old registration hasn't aged out yet.
        """
        resp = await self._client.get(
            "/v1/discover",
            params={"capability": capability, "limit": limit},
        )
        if resp.status_code != 200:
            raise MeshRegistryError(
                f"discover({capability!r}) failed: HTTP {resp.status_code} {resp.text[:200]}"
            )
        results = resp.json().get("results", [])
        agents = [
            DiscoveredAgent(
                did=r["did"],
                capabilities=r.get("capabilities", []),
                metadata=r.get("metadata", {}),
                last_seen=r.get("last_seen", ""),
                reputation_score=float(r.get("reputation_score", 0.0)),
            )
            for r in results
        ]
        agents.sort(key=lambda a: a.last_seen, reverse=True)
        return agents

    async def find_by_display_name(self, name: str) -> DiscoveredAgent | None:
        """Convenience wrapper around :meth:`discover`. Returns the
        first agent whose advertised capabilities include ``name`` —
        ``MeshClient.connect`` registers the display name as a
        capability string (the TS SDK does the same), so this serves
        as ``find_by_display_name`` without needing the registry to
        return ``metadata`` (the AGT Python registry's ``/v1/discover``
        endpoint returns ``did/capabilities/last_seen`` only — no
        ``metadata`` field).
        """
        candidates = await self.discover(name, limit=200)
        for c in candidates:
            if name in c.capabilities:
                return c
        return candidates[0] if candidates else None

    # ── Heartbeat ──────────────────────────────────────────────────────

    async def heartbeat(self) -> None:
        """``POST /v1/agents/{did}/heartbeat`` — ping the registry so
        our agent record stays marked live."""
        headers = self._auth_headers(
            "POST", f"/v1/agents/{self._identity_did}/heartbeat"
        )
        resp = await self._client.post(
            f"/v1/agents/{self._identity_did}/heartbeat",
            headers=headers,
        )
        if resp.status_code in {200, 204}:
            return
        if resp.status_code in {401, 403}:
            raise MeshAuthError(
                f"Registry rejected heartbeat: {resp.status_code} {resp.text[:200]}"
            )
        # Heartbeat 404 means the agent was unregistered server-side; let
        # the caller decide whether to re-register.
        raise MeshRegistryError(
            f"heartbeat failed: HTTP {resp.status_code} {resp.text[:200]}"
        )

    # ── Direct DID lookup ──────────────────────────────────────────────

    async def get_agent(self, did: str) -> DiscoveredAgent | None:
        """``GET /v1/agents/{did}`` — fetch a single agent record by DID.

        Returns ``None`` if the registry has no entry (404). Raises for
        other HTTP errors. Used by callers that need to reverse-resolve
        a peer's display name from an inbound message's ``from_did``
        without scanning the full ``/v1/discover`` result set
        (``discover("")`` returns nothing on the AGT registry — the
        ``capability`` query parameter is mandatory).
        """
        resp = await self._client.get(f"/v1/agents/{did}")
        if resp.status_code == 404:
            return None
        if resp.status_code != 200:
            raise MeshRegistryError(
                f"get_agent({did!r}) failed: HTTP {resp.status_code} {resp.text[:200]}"
            )
        body = resp.json()
        return DiscoveredAgent(
            did=did,
            capabilities=body.get("capabilities", []),
            metadata=body.get("metadata", {}),
            last_seen=body.get("last_seen", ""),
            reputation_score=float(body.get("reputation_score", 0.0)),
        )

    # ── Ed25519-Timestamp auth ─────────────────────────────────────────

    def _auth_headers(self, _method: str, _path: str) -> dict[str, str]:
        """Build the Ed25519-Timestamp Authorization header per AGT
        spec (registry/app.py::verify_ed25519_timestamp_auth, AGENTMESH-WIRE
        §13.1):

        ``Authorization: Ed25519-Timestamp <did> <iso_ts> <b64url(sig)>``

        The signature is over the **timestamp string only** (UTF-8
        bytes), NOT method/path. The replay window is 5 minutes. We
        keep the ``method`` and ``path`` parameters in the signature
        for source compat / future extension but they're unused.
        """
        ts = _iso_utc()
        sig = self._signing_key.sign(ts.encode("utf-8")).signature
        header = f"Ed25519-Timestamp {self._identity_did} {ts} {_b64url(sig)}"
        return {"Authorization": header}
