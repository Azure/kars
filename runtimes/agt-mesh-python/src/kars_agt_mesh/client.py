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
import logging
import uuid
from typing import AsyncIterator

# Upstream AGT Python crypto — built locally via runtimes/build-agt-wheels.sh.
# The encryption primitives ARE the source of truth for byte-on-the-wire
# compatibility with the TS SDK (both speak the same Signal Protocol
# variant). We never re-implement these.
from agentmesh.encryption.channel import ChannelEstablishment, SecureChannel
from agentmesh.encryption.ratchet import EncryptedMessage
from agentmesh.encryption.x3dh import PreKeyBundle, X3DHKeyManager

from .config import MeshConfig
from .errors import MeshPeerNotFoundError, MeshTransportError
from .identity import Identity, IdentityStore
from .messages import InboundMessage
from .registry_client import PeerBundle, RegistryClient

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
        # X3DH key manager (responder-side material lives here): owns
        # the signed pre-key + one-time pre-keys, derives X25519
        # identity key from Ed25519 identity. Built once at connect().
        self._key_manager: X3DHKeyManager | None = None
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

            # X3DH bootstrap: build key manager from our persistent
            # Ed25519 identity, generate a signed pre-key + a small
            # batch of one-time pre-keys, then publish them so peers
            # can initiate sessions to us. The signed pre-key signature
            # is over the X25519 public key with our Ed25519 identity
            # — the upstream `generate_signed_pre_key` handles this.
            seed = self._identity.ed25519_seed
            full_ed_private = bytes(self._identity.signing_key) + self._identity.verify_key_bytes
            self._key_manager = X3DHKeyManager.from_ed25519_keys(
                full_ed_private if len(full_ed_private) == 64 else seed,
                self._identity.verify_key_bytes,
            )
            self._key_manager.generate_signed_pre_key()
            otks = self._key_manager.generate_one_time_pre_keys(count=10)
            spk = self._key_manager.signed_pre_key
            assert spk is not None  # just generated
            await self._registry.upload_prekeys(
                identity_key_x25519=self._key_manager.identity_key.public_key,
                identity_key_ed25519=self._identity.verify_key_bytes,
                signed_pre_key={
                    "key_id": spk.key_id,
                    "public_key": _b64url(spk.key_pair.public_key),
                    "signature": _b64url(spk.signature),
                },
                one_time_pre_keys=[
                    {
                        "key_id": otk.key_id,
                        "public_key": _b64url(otk.key_pair.public_key),
                    }
                    for otk in otks
                ],
            )

            # Lazy-import to keep transport optional in unit tests.
            from .relay_transport import RelayTransport

            self._relay = RelayTransport(
                url=self._config.relay_url,
                identity_did=self._identity.did,
                identity_signing_key=self._identity.signing_key,
                identity_public_key=self._identity.verify_key_bytes,
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

        First call to a new peer performs the X3DH handshake and
        sends a fused KNOCK + first-message frame; subsequent calls
        reuse the established :class:`SecureChannel`."""
        if not self._is_connected:
            raise MeshTransportError("Not connected — call connect() first")
        if self._relay is None or self._registry is None:
            raise MeshTransportError("Internal state corrupted: registry/relay missing")

        channel = self._channels.get(to)
        knock_payload: dict | None = None
        if channel is None:
            channel, establishment = await self._initiate_session(to)
            self._channels[to] = channel
            knock_payload = _establishment_to_wire(establishment)

        # SecureChannel.send returns the wire-ready EncryptedMessage
        # (the same shape SecureChannel.receive accepts on the other
        # side). We wrap it in the relay frame envelope.
        encrypted = channel.send(payload)
        frame: dict = {
            "v": 1,
            "type": "knock" if knock_payload is not None else "message",
            "from": self._identity.did,
            "to": to,
            "id": str(uuid.uuid4()),
            "ts": _iso_utc(),
            "ciphertext": _encrypted_to_wire(encrypted),
        }
        if knock_payload is not None:
            frame["establishment"] = knock_payload
        await self._relay.send_frame(frame)
        logger.debug(
            "Sent %s frame (%d bytes payload) to %s (id=%s)",
            frame["type"],
            len(payload),
            to,
            frame["id"],
        )

    def inbox(self) -> AsyncIterator[InboundMessage]:
        """Async iterator over decrypted inbound messages.

        Order-preserving: messages are yielded in the order they
        arrived at the local relay receive loop. Backpressure: the
        underlying ``asyncio.Queue`` is unbounded by default; callers
        with strict memory budgets should consume in a tight loop."""
        return _InboxIterator(self._inbox)

    # ── Internals ───────────────────────────────────────────────────────

    async def _initiate_session(
        self, peer_did: str
    ) -> tuple[SecureChannel, ChannelEstablishment]:
        """Run X3DH against the peer's published prekey bundle and
        return a SecureChannel + ChannelEstablishment. The
        establishment data must be forwarded to the peer in the KNOCK
        frame so they can rebuild their side."""
        assert self._registry is not None
        assert self._key_manager is not None
        bundle = await self._registry.fetch_prekeys(peer_did)
        # Domain-separator AAD mirrors the TS SDK convention
        # ``${initiator}|${responder}``. The responder reconstructs
        # the same AAD using ``${from_did}|${self_did}`` so both sides
        # derive byte-identical inputs to the Double Ratchet.
        aad = f"{self._identity.did}|{peer_did}".encode("utf-8")
        channel, establishment = SecureChannel.create_sender(
            self._key_manager,
            _peer_bundle_to_upstream(bundle),
            aad,
        )
        logger.info("Established session with %s", peer_did)
        return channel, establishment

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
            # We don't have a session for this peer yet — they should
            # have sent a KNOCK first. Drop (the relay will not
            # re-deliver, so this is fail-loud rather than fail-quiet).
            logger.warning(
                "Dropping message from %s: no SecureChannel (KNOCK first)",
                from_did,
            )
            return
        try:
            plaintext = channel.receive(_wire_to_encrypted(frame["ciphertext"]))
        except Exception as exc:  # noqa: BLE001
            logger.warning("Decrypt failed for %s: %s", from_did, exc)
            return
        await self._inbox.put(
            InboundMessage.new(
                from_did=from_did,
                from_display_name=None,
                payload=plaintext,
                message_id=str(frame.get("id", "")),
            )
        )

    async def _handle_knock_frame(self, frame: dict) -> None:
        """Auto-accept a KNOCK + decrypt the bundled first message.

        The KNOCK carries the initiator's :class:`ChannelEstablishment`
        which lets us rebuild the responder-side SecureChannel via
        ``SecureChannel.create_receiver``. The same frame also carries
        the first ciphertext so the round-trip latency is one RTT
        (initiator → responder → reply), not two.
        """
        from_did = frame.get("from")
        if not isinstance(from_did, str):
            logger.warning("Dropping KNOCK frame: missing 'from'")
            return
        if self._key_manager is None:
            logger.warning("Dropping KNOCK from %s: not connected yet", from_did)
            return

        est_raw = frame.get("establishment")
        if not isinstance(est_raw, dict):
            logger.warning(
                "Dropping KNOCK from %s: missing 'establishment'", from_did
            )
            return
        try:
            establishment = _wire_to_establishment(est_raw)
        except Exception as exc:  # noqa: BLE001
            logger.warning("Malformed KNOCK from %s: %s", from_did, exc)
            return

        # Responder AAD: initiator computed `${initiator_did}|${self_did}`;
        # we mirror that exact byte string.
        aad = f"{from_did}|{self._identity.did}".encode("utf-8")
        try:
            channel = SecureChannel.create_receiver(
                self._key_manager, establishment, aad
            )
        except Exception as exc:  # noqa: BLE001
            logger.warning(
                "KNOCK from %s rejected at X3DH responder: %s", from_did, exc
            )
            return

        # Replenish the OTK pool eagerly so the next KNOCK doesn't
        # have to wait for one. Fire-and-forget — failure here only
        # affects future sessions, not this one.
        try:
            await self._top_up_otks()
        except Exception as exc:  # noqa: BLE001
            logger.warning("OTK top-up failed: %s", exc)

        self._channels[from_did] = channel
        logger.info("Accepted KNOCK from %s", from_did)

        # KNOCKs always carry the first message ciphertext (initiator
        # never sends a bare KNOCK in our wire format).
        cipher = frame.get("ciphertext")
        if isinstance(cipher, str):
            try:
                plaintext = channel.receive(_wire_to_encrypted(cipher))
            except Exception as exc:  # noqa: BLE001
                logger.warning(
                    "First-message decrypt failed for %s: %s", from_did, exc
                )
                return
            await self._inbox.put(
                InboundMessage.new(
                    from_did=from_did,
                    from_display_name=None,
                    payload=plaintext,
                    message_id=str(frame.get("id", "")),
                )
            )

    async def _top_up_otks(self, *, threshold: int = 3, batch: int = 10) -> None:
        """Re-publish a fresh batch of OTKs when the unused pool is
        getting low. AGT registry consumes one OTK per X3DH; running
        out would force peers to skip the OPK step (weaker security)."""
        assert self._registry is not None
        assert self._key_manager is not None
        # The X3DHKeyManager doesn't track which OTKs were consumed
        # by remote peers — we just keep generating. The registry
        # overwrites the bundle on PUT, so the latest call is what
        # peers will fetch. Lightweight scheme: always top up to
        # `batch` fresh keys whenever a new KNOCK lands.
        otks = self._key_manager.generate_one_time_pre_keys(count=batch)
        spk = self._key_manager.signed_pre_key
        assert spk is not None
        await self._registry.upload_prekeys(
            identity_key_x25519=self._key_manager.identity_key.public_key,
            identity_key_ed25519=self._identity.verify_key_bytes,
            signed_pre_key={
                "key_id": spk.key_id,
                "public_key": _b64url(spk.key_pair.public_key),
                "signature": _b64url(spk.signature),
            },
            one_time_pre_keys=[
                {
                    "key_id": otk.key_id,
                    "public_key": _b64url(otk.key_pair.public_key),
                }
                for otk in otks
            ],
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


def _encrypted_to_wire(em: EncryptedMessage) -> str:
    """Serialize an upstream :class:`EncryptedMessage` to the wire-format
    string the relay accepts (base64url of the binary serialization)."""
    return _b64url(em.serialize())


def _wire_to_encrypted(s: str) -> EncryptedMessage:
    """Inverse of :func:`_encrypted_to_wire` — used on the receive
    side. Returns the upstream ``EncryptedMessage`` instance ready
    for :meth:`SecureChannel.receive`."""
    return EncryptedMessage.deserialize(_b64url_decode(s))


def _establishment_to_wire(est: ChannelEstablishment) -> dict:
    """Serialize a :class:`ChannelEstablishment` to a JSON-safe dict
    suitable for embedding inside a relay frame's ``establishment``
    field. Keys mirror the upstream dataclass fields."""
    return {
        "initiator_identity_key": _b64url(est.initiator_identity_key),
        "ephemeral_public_key": _b64url(est.ephemeral_public_key),
        "used_one_time_key_id": est.used_one_time_key_id,
    }


def _wire_to_establishment(d: dict) -> ChannelEstablishment:
    return ChannelEstablishment(
        initiator_identity_key=_b64url_decode(d["initiator_identity_key"]),
        ephemeral_public_key=_b64url_decode(d["ephemeral_public_key"]),
        used_one_time_key_id=d.get("used_one_time_key_id"),
    )


def _peer_bundle_to_upstream(bundle: PeerBundle) -> PreKeyBundle:
    """Translate our :class:`PeerBundle` into the
    ``agentmesh.encryption.x3dh.PreKeyBundle`` shape that
    :meth:`SecureChannel.create_sender` accepts. The upstream bundle
    is FLAT — fields live directly on PreKeyBundle, not nested."""
    return PreKeyBundle(
        identity_key=bundle.identity_key_x25519,
        identity_key_ed=bundle.identity_key_ed25519,
        signed_pre_key=bundle.signed_pre_key_public,
        signed_pre_key_signature=bundle.signed_pre_key_signature,
        signed_pre_key_id=bundle.signed_pre_key_id,
        one_time_pre_key=bundle.one_time_pre_key_public,
        one_time_pre_key_id=bundle.one_time_pre_key_id,
    )


def _b64url(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode("ascii")


def _b64url_decode(s: str) -> bytes:
    pad = "=" * (-len(s) % 4)
    return base64.urlsafe_b64decode(s + pad)
