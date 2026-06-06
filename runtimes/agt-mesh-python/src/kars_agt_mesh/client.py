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
import errno
import json
import logging
import os
import uuid
from pathlib import Path
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
        # Filesystem handle for the prekey-writer lock (acquired by
        # ``_acquire_prekey_writer_lock``, released on ``disconnect``
        # or process exit). Kept open for the lifetime of the client so
        # the kernel-held flock stays valid.
        self._prekey_lock_fd: int | None = None

    def _acquire_prekey_writer_lock(self) -> None:
        """Acquire an exclusive fcntl flock on the per-identity prekey
        writer sentinel so only one process per pod can upload prekeys.

        Why: ``register_self`` is idempotent — restart-safe — but
        ``upload_prekeys`` clobbers whatever the registry has for our
        DID, including the bundle a sibling Python process uploaded
        moments ago from its own (fresh) ``X3DHKeyManager`` state. The
        daemon's in-memory ``signed_pre_key.private`` then no longer
        matches the public bytes any peer fetches from the registry,
        every X3DH-derived shared secret diverges, every AEAD frame
        fails ``InvalidTag`` on receive.

        Fail-loud here is much cheaper than the silent corruption: the
        operator immediately sees ``MeshTransportError: another mesh
        client process holds <path>`` and stops the secondary process,
        instead of spending hours hunting a ``Decrypt failed`` log that
        gives no traceback. See
        ``docs/internal/security-audits/2026-06-06-cross-runtime-mesh-aks.md``
        for the full debugging post-mortem this guard prevents from
        recurring.

        Linux-only (``fcntl``). Best-effort on platforms without
        ``fcntl`` (Windows): logs a warning and continues — the test
        scenarios this protects against don't occur on Windows pods.
        """
        try:
            import fcntl  # noqa: PLC0415  Linux-only
        except ImportError:
            logger.warning(
                "fcntl unavailable on this platform — prekey-writer "
                "lock disabled. The MeshClient will run unprotected; "
                "if a second process starts a MeshClient for the same "
                "identity it WILL silently corrupt registry state."
            )
            return

        lock_path = (
            Path(self._config.identity_path).parent / ".mesh-prekeys.lock"
        )
        lock_path.parent.mkdir(parents=True, exist_ok=True)
        fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as exc:
            os.close(fd)
            if exc.errno in {errno.EWOULDBLOCK, errno.EAGAIN}:
                holder_pid = "<unknown>"
                try:
                    holder_pid = lock_path.read_text().strip() or "<empty>"
                except OSError:
                    pass
                raise MeshTransportError(
                    f"Another mesh-client process already holds "
                    f"{lock_path} (pid={holder_pid}). Refusing to start "
                    f"a second MeshClient for did={self._identity.did} — "
                    f"would clobber the running daemon's prekey bundle "
                    f"and break its ability to decrypt incoming frames. "
                    f"If you ran `python3 -c 'mesh._get_or_init_client()'` "
                    f"from `kubectl exec` for debugging, that is the "
                    f"trap this guard protects against — query the daemon "
                    f"via the gateway HTTP API or `kubectl logs` instead."
                ) from None
            raise
        # Record our PID inside the lock file so a future operator
        # `cat .mesh-prekeys.lock` reveals which process holds it.
        os.ftruncate(fd, 0)
        os.write(fd, f"{os.getpid()}\n".encode("ascii"))
        os.fsync(fd)
        self._prekey_lock_fd = fd
        logger.debug(
            "Prekey-writer lock acquired: %s (pid=%d)",
            lock_path,
            os.getpid(),
        )

    def _release_prekey_writer_lock(self) -> None:
        """Release the flock (no-op if never acquired). Called from
        ``disconnect()``; the OS releases on process exit too."""
        if self._prekey_lock_fd is None:
            return
        try:
            import fcntl  # noqa: PLC0415

            fcntl.flock(self._prekey_lock_fd, fcntl.LOCK_UN)
        except (ImportError, OSError):
            pass
        try:
            os.close(self._prekey_lock_fd)
        except OSError:
            pass
        self._prekey_lock_fd = None

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
        transient transport errors (retried internally).

        Single-writer guard: acquires an exclusive ``fcntl.flock`` on
        ``<identity_dir>/.mesh-prekeys.lock`` before uploading prekeys.
        If a second process tries to start a MeshClient for the same
        identity (e.g. someone runs ``python3 -c "from
        kars_runtime_hermes.plugin import mesh; mesh._get_or_init_client()"``
        from ``kubectl exec`` while the daemon is running), this raises
        :class:`MeshTransportError` instead of silently overwriting the
        registry's prekey bundle and breaking the daemon's ability to
        decrypt KNOCKs. Cost us hours of debugging before the guard —
        documented in
        ``docs/internal/security-audits/2026-06-06-cross-runtime-mesh-aks.md``.
        """
        async with _SINGLETON_LOCK:
            if self._is_connected:
                return

            # ── Single-writer flock guard ────────────────────────────────
            self._acquire_prekey_writer_lock()
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

            # Pass through the Entra-signed JWT so Entra-enforcing
            # relays (AGENTMESH_ENTRA_ENFORCE=true) accept the WS
            # connect frame. Mirror of the TS SDK's behaviour
            # (mesh-client.ts passes the same token under the
            # ``token`` key on the connect frame). The entrypoint
            # script populates AGT_OAUTH_TOKEN via workload-identity
            # exchange when the operator opts into Entra-verified mesh
            # peers via `MESH_AUTH_BACKEND=EntraAgentIdentity` on the
            # KarsAuthConfig.
            import os as _os
            _entra_token = _os.environ.get("AGT_OAUTH_TOKEN") or None

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
                entra_token=_entra_token,
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
        self._release_prekey_writer_lock()
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

        First call to a new peer sends TWO frames in succession:
          1. ``type=knock`` with ``establishment={ik,ek,otk}`` (NO ciphertext)
          2. ``type=message`` with the encrypted payload

        This matches the TypeScript SDK (``@microsoft/agent-governance-sdk``)
        wire convention exactly so a Python sender can interop with a TS
        receiver and vice versa. An earlier draft fused KNOCK + first
        ciphertext into a single frame which only Python receivers
        understood — TS receivers ignored the bundled ciphertext and
        the first message was lost on cross-runtime sends.
        """
        if not self._is_connected:
            raise MeshTransportError("Not connected — call connect() first")
        if self._relay is None or self._registry is None:
            raise MeshTransportError("Internal state corrupted: registry/relay missing")

        channel = self._channels.get(to)
        if channel is None:
            channel, establishment = await self._initiate_session(to)
            self._channels[to] = channel
            knock_frame = {
                "v": 1,
                "type": "knock",
                "from": self._identity.did,
                "to": to,
                "id": str(uuid.uuid4()),
                "ts": _iso_utc(),
                "intent": {"action": "establish_session"},
                "establishment": _establishment_to_wire(establishment),
            }
            await self._relay.send_frame(knock_frame)
            logger.debug("Sent KNOCK to %s (id=%s)", to, knock_frame["id"])

        wire_payload = _payload_to_wire_bytes(payload)
        encrypted = channel.send(wire_payload)
        message_frame = _encrypted_to_message_frame(
            encrypted, self._identity.did, to
        )
        await self._relay.send_frame(message_frame)
        logger.debug(
            "Sent MESSAGE frame (%d bytes payload, %d bytes on wire) to %s (id=%s)",
            len(payload),
            len(wire_payload),
            to,
            message_frame["id"],
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
            logger.warning(
                "Dropping message from %s: no SecureChannel (KNOCK first)",
                from_did,
            )
            return
        try:
            encrypted = _message_frame_to_encrypted(frame)
        except Exception as exc:  # noqa: BLE001
            logger.warning("Malformed message from %s: %s", from_did, exc)
            return
        try:
            plaintext = channel.receive(encrypted)
        except Exception as exc:  # noqa: BLE001
            logger.warning("Decrypt failed for %s: %s", from_did, exc)
            return
        # Mirror of _payload_to_wire_bytes — strip the JSON envelope the
        # TS SDK puts on every message so callers see the original bytes
        # they would have sent.
        app_payload = _wire_bytes_to_payload(plaintext)
        await self._inbox.put(
            InboundMessage.new(
                from_did=from_did,
                from_display_name=None,
                payload=app_payload,
                message_id=str(frame.get("id", "")),
            )
        )

    async def _handle_knock_frame(self, frame: dict) -> None:
        """Auto-accept a KNOCK frame.

        The KNOCK carries the initiator's :class:`ChannelEstablishment`
        in the ``establishment`` field, which lets us rebuild the
        responder-side SecureChannel via
        ``SecureChannel.create_receiver``. After this, the actual
        first message arrives in a separate ``type=message`` frame
        (TS SDK wire convention — `mesh-client.js` sends KNOCK and
        first message as two distinct frames). Earlier Python builds
        fused them into one frame; that was incompatible with TS
        receivers and has been removed.
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
        # we mirror that exact byte string. Matches TS
        # `new TextEncoder().encode(\`${peerId}|${this.activeDid}\`)`
        # in mesh-client.js::acceptSession.
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

        try:
            await self._top_up_otks()
        except Exception as exc:  # noqa: BLE001
            logger.warning("OTK top-up failed: %s", exc)

        self._channels[from_did] = channel
        logger.info("Accepted KNOCK from %s", from_did)

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


def _b64std(b: bytes) -> str:
    """Standard (NOT urlsafe) base64 with padding — matches what JS's
    ``btoa`` / Node's ``Buffer.toString('base64')`` produce on the
    TS-side SDK. The relay envelope expects std-base64 for the
    ``ciphertext`` + ``header.dh`` fields of a ``type=message`` frame
    and for the ``establishment.{ik,ek}`` fields of a KNOCK frame.
    """
    return base64.b64encode(b).decode("ascii")


def _b64std_decode(s: str) -> bytes:
    """Tolerant std-base64 decode: accepts both urlsafe and standard
    alphabets so we interop with senders that prefer either (Python
    libraries default to urlsafe, browsers default to standard)."""
    s = s.strip()
    # Restore canonical alphabet (urlsafe → standard)
    s = s.replace("-", "+").replace("_", "/")
    pad = "=" * (-len(s) % 4)
    return base64.b64decode(s + pad)


def _payload_to_wire_bytes(payload: bytes) -> bytes:
    """JSON-wrap ``payload`` so the TS SDK can decode it.

    The AGT TypeScript SDK's ``MeshClient.handleMessage`` hardcodes a
    ``JSON.parse(new TextDecoder().decode(plaintext))`` on every
    successfully-decrypted frame (see
    ``agent-governance-typescript/src/encryption/mesh-client.ts``
    handleMessage, the line beginning ``payload = JSON.parse(...)``).
    A Python sender that hands raw bytes straight to
    ``SecureChannel.send()`` produces a perfectly-encrypted frame
    that fails *silently* at the receiver's JSON.parse step: the
    plaintext is recovered, but the SDK's message handlers are never
    invoked because the parse throws before reaching them.

    We mirror the TS SDK's outbound convention
    (``mesh-client.ts::send`` line ``session.channel.send(new
    TextEncoder().encode(JSON.stringify(payload)))``) by JSON-encoding
    here. UTF-8 byte payloads become JSON strings; binary payloads
    are wrapped in a ``{"raw_b64": "..."}`` envelope.
    """
    try:
        text = payload.decode("utf-8")
        return json.dumps(text).encode("utf-8")
    except UnicodeDecodeError:
        envelope = {"raw_b64": base64.b64encode(payload).decode("ascii")}
        return json.dumps(envelope).encode("utf-8")


def _wire_bytes_to_payload(wire_bytes: bytes) -> bytes:
    """Inverse of :func:`_payload_to_wire_bytes`.

    A Python receiver should hand the application the bytes the
    sender originally passed to :meth:`MeshClient.send_by_did`, not
    the JSON-encoded transport form. We try the inverse of both
    encoding paths (UTF-8 string → bytes; ``{"raw_b64": ...}``
    envelope → bytes) and fall back to the raw plaintext when the
    payload didn't come from a TS-convention sender (other Python
    senders pre-dating this wrapper, or future opt-out callers).
    """
    try:
        decoded = json.loads(wire_bytes.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return wire_bytes
    if isinstance(decoded, str):
        return decoded.encode("utf-8")
    if isinstance(decoded, dict) and isinstance(decoded.get("raw_b64"), str):
        try:
            return base64.b64decode(decoded["raw_b64"])
        except Exception:  # noqa: BLE001
            return wire_bytes
    # Anything else is a structured JSON payload (object / array / number);
    # callers that opted into structured payloads get the original bytes
    # back so they can re-parse if they want.
    return wire_bytes


def _encrypted_to_message_frame(
    em: EncryptedMessage, from_did: str, to_did: str
) -> dict:
    """Build a ``type=message`` relay frame matching the TS SDK wire
    convention (``mesh-client.js::send``).

    The header is a structured object ``{dh, pn, n}`` with std-base64
    ``dh``; the ciphertext is std-base64. An earlier Python build
    packed the header+ciphertext into a single urlsafe-b64 blob in
    the ``ciphertext`` field — only Python receivers understood that
    shape and TS receivers dropped the message on the floor.
    """
    return {
        "v": 1,
        "type": "message",
        "from": from_did,
        "to": to_did,
        "id": str(uuid.uuid4()),
        "ts": _iso_utc(),
        "header": {
            "dh": _b64std(em.header.dh_public_key),
            "pn": em.header.previous_chain_length,
            "n": em.header.message_number,
        },
        "ciphertext": _b64std(em.ciphertext),
    }


def _message_frame_to_encrypted(frame: dict) -> EncryptedMessage:
    """Inverse of :func:`_encrypted_to_message_frame`. Tolerates both
    the TS shape (``header.{dh,pn,n}`` + ``ciphertext``) and an older
    legacy shape (``ciphertext`` carrying everything b64url-packed)
    for one release cycle so in-flight messages mid-upgrade aren't
    lost.
    """
    from agentmesh.encryption.ratchet import MessageHeader

    header = frame.get("header")
    if isinstance(header, dict) and "dh" in header:
        return EncryptedMessage(
            header=MessageHeader(
                dh_public_key=_b64std_decode(header["dh"]),
                previous_chain_length=int(header.get("pn", 0)),
                message_number=int(header.get("n", 0)),
            ),
            ciphertext=_b64std_decode(frame["ciphertext"]),
        )
    # Legacy fallback: header was packed into the ciphertext blob.
    return EncryptedMessage.deserialize(_b64std_decode(frame["ciphertext"]))


def _establishment_to_wire(est: ChannelEstablishment) -> dict:
    """Serialize a :class:`ChannelEstablishment` to the TS-compatible
    JSON shape (``mesh-client.js::serializeEstablishment``):

    .. code-block:: json

        {"ik": "<std-base64>", "ek": "<std-base64>", "otk": <int|missing>}

    where ``ik`` is the initiator's X25519 identity public key, ``ek``
    is the ephemeral X25519 public key, and ``otk`` is the consumed
    one-time-prekey id (omitted if no OTK was used). Earlier Python
    used long snake_case field names that the TS SDK didn't recognise.
    """
    out: dict = {
        "ik": _b64std(est.initiator_identity_key),
        "ek": _b64std(est.ephemeral_public_key),
    }
    if est.used_one_time_key_id is not None:
        out["otk"] = int(est.used_one_time_key_id)
    return out


def _wire_to_establishment(d: dict) -> ChannelEstablishment:
    """Inverse of :func:`_establishment_to_wire`. Accepts both the
    TS short-key shape (``ik``/``ek``/``otk``) and the legacy Python
    long-key shape (``initiator_identity_key``/...) for one release
    cycle to bridge mid-upgrade fleets.
    """
    if "ik" in d:
        otk_raw = d.get("otk")
        return ChannelEstablishment(
            initiator_identity_key=_b64std_decode(d["ik"]),
            ephemeral_public_key=_b64std_decode(d["ek"]),
            used_one_time_key_id=int(otk_raw) if otk_raw is not None else None,
        )
    return ChannelEstablishment(
        initiator_identity_key=_b64std_decode(d["initiator_identity_key"]),
        ephemeral_public_key=_b64std_decode(d["ephemeral_public_key"]),
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
