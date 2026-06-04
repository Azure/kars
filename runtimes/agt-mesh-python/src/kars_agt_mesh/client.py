# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Top-level :class:`MeshClient` — the public façade every Python
agent framework imports.

This module is intentionally THIN: it owns the lifecycle (config →
identity → registry → relay → ratchets → public API) and orchestrates
the lower-level pieces (:mod:`registry_client`, :mod:`relay_transport`,
the upstream ``agentmesh.encryption.*`` modules) but contains no
crypto of its own. That separation is what lets every Python runtime
import this one class and get TS-SDK-equivalent mesh behaviour
without re-implementing Signal Protocol.
"""

from __future__ import annotations

import asyncio
import base64
import json
import logging
import uuid
from typing import AsyncIterator

# Upstream AGT Python crypto — built locally via runtimes/build-agt-wheels.sh.
# The encryption primitives ARE the source of truth for byte-on-the-wire
# compatibility with the TS SDK (both speak the same Signal Protocol
# variant). We never re-implement these.
from agentmesh.encryption.channel import SecureChannel

from .config import MeshConfig
from .errors import MeshPeerNotFoundError, MeshTransportError
from .identity import Identity, IdentityStore
from .messages import InboundMessage
from .registry_client import RegistryClient

logger = logging.getLogger("kars_agt_mesh.client")

# Process-level singleton registry so a re-import of the package
# inside the same Python process (common in frameworks that lazy-load
# plugins from multiple subsystems) shares one MeshClient + one WS
# connection. Cache key = (name, relay_url, registry_url). Mirrors the
# ``Symbol.for("agt-mesh-client")`` pattern from
# ``runtimes/openclaw/src/index.ts``.
_SINGLETONS: dict[tuple[str, str, str], "MeshClient"] = {}
_SINGLETON_LOCK = asyncio.Lock()


class MeshClient:
    """Async E2E-encrypted mesh client for any Python agent framework.

    Lifecycle::

        client = MeshClient(config)
        await client.connect()             # registers + opens WS
        await client.send_by_name("peer", b"hello")
        async for msg in client.inbox():
            handle(msg)
        await client.disconnect()

    or, equivalently::

        async with MeshClient(config) as client:
            ...

    Singleton: multiple ``MeshClient(config)`` calls with the same
    name/relay/registry return the same instance.
    """

    def __new__(cls, config: MeshConfig) -> MeshClient:
        key = (config.name, config.relay_url, config.registry_url)
        if key in _SINGLETONS:
            return _SINGLETONS[key]
        instance = super().__new__(cls)
        _SINGLETONS[key] = instance
        return instance

    def __init__(self, config: MeshConfig) -> None:
        # __new__ may return an existing singleton — guard re-init.
        if getattr(self, "_initialised", False):
            return
        self._initialised = True

        self._config = config
        self._identity: Identity = IdentityStore.load_or_create(config.identity_path)
        self._registry: RegistryClient | None = None
        self._relay = None  # type: ignore[var-annotated]
        # Per-peer SecureChannel state. The upstream ``SecureChannel``
        # encapsulates X3DH + Double Ratchet, so we just keep one
        # channel per peer DID and let the channel own the state.
        self._channels: dict[str, SecureChannel] = {}
        # Inbox queue — drained by `inbox()` async iterator.
        self._inbox: asyncio.Queue[InboundMessage] = asyncio.Queue()
        self._is_connected = False

    # ── Lifecycle ────────────────────────────────────────────────────────

    async def __aenter__(self) -> MeshClient:
        await self.connect()
        return self

    async def __aexit__(self, *_exc: object) -> None:
        await self.disconnect()

    async def connect(self) -> None:
        """Register self with the registry and open the relay WS.

        Idempotent — calling twice is a no-op. Raises on auth failure
        (operator needs to fix identity/clock/RBAC), transparent on
        transient transport errors (retried internally)."""
        async with _SINGLETON_LOCK:
            if self._is_connected:
                return
            self._registry = RegistryClient(
                base_url=self._config.registry_url,
                identity_signing_key=self._identity.signing_key,
                identity_did=self._identity.did,
                timeout_seconds=self._config.http_timeout_seconds,
                user_agent=self._config.user_agent,
            )
            await self._registry.register_self(
                # Display name is registered as a capability so other
                # agents can discover us via /v1/discover. This is the
                # convention the TS SDK adopted and what the kars
                # operator UX queries.
                capabilities=[self._config.name],
                metadata={
                    "display_name": self._config.name,
                    "runtime": "python",
                    "library": "kars-agt-mesh/0.1.0",
                },
            )

            # Lazy-import to keep transport optional in unit tests.
            from .relay_transport import RelayTransport

            self._relay = RelayTransport(
                url=self._config.relay_url,
                identity_did=self._identity.did,
                user_agent=self._config.user_agent,
                heartbeat_interval_seconds=self._config.heartbeat_interval_seconds,
                reconnect_initial_seconds=self._config.reconnect_initial_seconds,
                reconnect_max_seconds=self._config.reconnect_max_seconds,
                on_frame=self._handle_frame,
            )
            await self._relay.connect()
            self._is_connected = True
            logger.info(
                "MeshClient connected: name=%s did=%s",
                self._config.name,
                self._identity.did,
            )

    async def disconnect(self) -> None:
        """Close the relay WS and HTTP client. Per-peer ratchet state
        is preserved in memory so a subsequent :meth:`connect` resumes
        sessions without re-running X3DH."""
        if self._relay is not None:
            await self._relay.disconnect()
            self._relay = None
        if self._registry is not None:
            await self._registry.aclose()
            self._registry = None
        self._is_connected = False

    # ── Public API: discovery + send ────────────────────────────────────

    async def discover(self, capability: str) -> list[str]:
        """Return DIDs of registered agents advertising ``capability``.

        Convenience: use the agent's display name as capability for a
        name → DID lookup."""
        if self._registry is None:
            raise MeshTransportError("Not connected — call connect() first")
        agents = await self._registry.discover(capability)
        return [a.did for a in agents]

    async def send_by_name(self, *, to: str, payload: bytes) -> None:
        """Look up ``to`` in the registry by display name then send
        an encrypted payload."""
        if self._registry is None:
            raise MeshTransportError("Not connected — call connect() first")
        peer = await self._registry.find_by_display_name(to)
        if peer is None:
            raise MeshPeerNotFoundError(
                f"No agent registered with display_name={to!r}"
            )
        await self.send_by_did(to=peer.did, payload=payload)

    async def send_by_did(self, *, to: str, payload: bytes) -> None:
        """Encrypt ``payload`` for ``to`` and dispatch via the relay.

        First call to a new peer performs the X3DH handshake +
        KNOCK frame; subsequent calls reuse the established
        :class:`SecureChannel`."""
        if not self._is_connected:
            raise MeshTransportError("Not connected — call connect() first")
        if self._relay is None or self._registry is None:
            raise MeshTransportError("Internal state corrupted: registry/relay missing")

        channel = self._channels.get(to)
        if channel is None:
            channel = await self._initiate_session(to)
            self._channels[to] = channel

        # SecureChannel.encrypt returns the wire-ready EncryptedMessage
        # (the same shape SecureChannel.decrypt accepts on the other
        # side). We wrap it in the relay frame envelope.
        encrypted = channel.encrypt(payload)
        frame = {
            "v": 1,
            "type": "message",
            "from": self._identity.did,
            "to": to,
            "id": str(uuid.uuid4()),
            "ts": _iso_utc(),
            "ciphertext": _encrypted_to_wire(encrypted),
        }
        await self._relay.send_frame(frame)
        logger.debug("Sent %d bytes to %s (via %s)", len(payload), to, frame["id"])

    def inbox(self) -> AsyncIterator[InboundMessage]:
        """Async iterator over decrypted inbound messages.

        Order-preserving: messages are yielded in the order they
        arrived at the local relay receive loop. Backpressure: the
        underlying ``asyncio.Queue`` is unbounded by default; callers
        with strict memory budgets should consume in a tight loop."""
        return _InboxIterator(self._inbox)

    # ── Internals ───────────────────────────────────────────────────────

    async def _initiate_session(self, peer_did: str) -> SecureChannel:
        """Run X3DH against the peer's published prekey bundle and
        return a SecureChannel ready for encrypt/decrypt."""
        assert self._registry is not None
        bundle = await self._registry.fetch_prekeys(peer_did)
        # Domain-separator AAD mirrors the TS SDK:
        # ``${selfDid}|${peerDid}`` (note: directionality matters —
        # the AAD must match what the peer derives on its side).
        aad = f"{self._identity.did}|{peer_did}".encode("utf-8")
        channel, _establishment = SecureChannel.create_sender(
            self._identity.x25519_private,
            self._identity.signing_key,
            _peer_bundle_to_upstream(bundle),
            aad,
        )
        logger.info("Established session with %s", peer_did)
        return channel

    async def _handle_frame(self, frame: dict) -> None:
        ftype = frame.get("type")
        if ftype == "message":
            await self._handle_message_frame(frame)
        elif ftype == "knock":
            await self._handle_knock_frame(frame)
        elif ftype in {"connect_ack", "heartbeat_ack", "knock_ack"}:
            # No-op; ack frames are bookkeeping for the TS-side state
            # machine, not something we need to act on.
            return
        else:
            logger.debug("Ignoring unhandled relay frame type=%r", ftype)

    async def _handle_message_frame(self, frame: dict) -> None:
        from_did = frame.get("from")
        if not isinstance(from_did, str):
            logger.warning("Dropping message frame: missing 'from'")
            return
        channel = self._channels.get(from_did)
        if channel is None:
            # We don't have a session for this peer yet — they probably
            # KNOCK'd us first and we haven't accepted. Defer.
            logger.warning(
                "Dropping message from %s: no SecureChannel (KNOCK first)",
                from_did,
            )
            return
        try:
            plaintext = channel.decrypt(_wire_to_encrypted(frame["ciphertext"]))
        except Exception as exc:  # noqa: BLE001
            logger.warning("Decrypt failed for %s: %s", from_did, exc)
            return
        await self._inbox.put(
            InboundMessage.new(
                from_did=from_did,
                from_display_name=None,  # TODO: cache + look up via registry
                payload=plaintext,
                message_id=str(frame.get("id", "")),
            )
        )

    async def _handle_knock_frame(self, frame: dict) -> None:
        # Accept logic (X3DH responder + trust-score gate) is implemented
        # in Act 2.2 — for v0.1 we only handle the SENDER side of KNOCK
        # (initiator). Inbound KNOCKs are logged and dropped.
        logger.info(
            "Received KNOCK from %s (responder path not implemented in v0.1)",
            frame.get("from"),
        )


class _InboxIterator:
    def __init__(self, queue: asyncio.Queue[InboundMessage]) -> None:
        self._queue = queue

    def __aiter__(self) -> _InboxIterator:
        return self

    async def __anext__(self) -> InboundMessage:
        return await self._queue.get()


def _iso_utc() -> str:
    from datetime import datetime, timezone

    return (
        datetime.now(timezone.utc)
        .isoformat(timespec="milliseconds")
        .replace("+00:00", "Z")
    )


def _encrypted_to_wire(em) -> str:  # noqa: ANN001
    """Serialize an upstream :class:`EncryptedMessage` to the wire-format
    string the relay accepts (base64url of the canonical JSON encoding
    of the message header + ciphertext + auth tag)."""
    return base64.urlsafe_b64encode(
        json.dumps(em.to_dict() if hasattr(em, "to_dict") else em.__dict__).encode(
            "utf-8"
        )
    ).rstrip(b"=").decode("ascii")


def _wire_to_encrypted(s: str):  # noqa: ANN201
    """Inverse of :func:`_encrypted_to_wire` — used on the receive
    side. Returns the upstream ``EncryptedMessage`` instance ready
    for :meth:`SecureChannel.decrypt`."""
    from agentmesh.encryption.ratchet import EncryptedMessage

    pad = "=" * (-len(s) % 4)
    raw = base64.urlsafe_b64decode(s + pad)
    data = json.loads(raw.decode("utf-8"))
    if hasattr(EncryptedMessage, "from_dict"):
        return EncryptedMessage.from_dict(data)
    return EncryptedMessage(**data)


def _peer_bundle_to_upstream(bundle):  # noqa: ANN001
    """Translate our :class:`PeerBundle` into the
    ``agentmesh.encryption.x3dh.PreKeyBundle`` shape that
    :meth:`SecureChannel.create_sender` accepts."""
    from agentmesh.encryption.x3dh import (
        OneTimePreKey,
        PreKeyBundle,
        SignedPreKey,
    )

    spk = bundle.signed_pre_key
    signed = SignedPreKey(
        key_id=spk["key_id"],
        public_key=_b64url_decode(spk["public_key"]),
        signature=_b64url_decode(spk["signature"]),
    )
    otps = [
        OneTimePreKey(key_id=otp["key_id"], public_key=_b64url_decode(otp["public_key"]))
        for otp in bundle.one_time_pre_keys
    ]
    return PreKeyBundle(
        identity_key=bundle.identity_key_x25519,
        identity_key_ed=bundle.identity_key_ed25519,
        signed_pre_key=signed,
        one_time_pre_keys=otps,
    )


def _b64url_decode(s: str) -> bytes:
    pad = "=" * (-len(s) % 4)
    return base64.urlsafe_b64decode(s + pad)
