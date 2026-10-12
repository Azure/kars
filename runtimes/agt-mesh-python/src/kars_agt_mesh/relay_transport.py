# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""WebSocket transport to the AGT relay.

Wire-spec: ``docs/specs/AGENTMESH-WIRE-1.0.md`` §10 — relay frames
are JSON objects of the shape::

    {
        "v": 1,
        "type": "knock" | "message" | "heartbeat" | "knock_ack" | ...,
        "from": "<sender DID>",
        "to":   "<receiver DID>",
        "id":   "<UUID>",
        "ts":   "<ISO 8601 UTC>",
        ...payload-specific fields...
    }

Frames are sent as text WS messages; binary frames are reserved for
future use. The transport itself doesn't interpret frame payloads —
it just delivers them in-order to the caller and provides exponential-
backoff reconnect with auth bootstrap on every reconnect.
"""

from __future__ import annotations

import asyncio
import base64
import inspect
import json
import logging
import uuid
from datetime import datetime, timezone
from typing import Any, Awaitable, Callable

import websockets
from nacl import signing
from websockets.exceptions import ConnectionClosed, WebSocketException
from websockets.protocol import State

from .errors import MeshTransportError

logger = logging.getLogger("kars_agt_mesh.relay")

FrameHandler = Callable[[dict[str, Any]], Awaitable[None]]


class RelayTransport:
    """Manages the long-lived WebSocket connection to the AGT relay.

    Lifecycle:

    1. :meth:`connect` — opens the WS, sends an auth frame, starts the
       heartbeat task, starts the receive loop. Returns once the
       connection is established (required mode: authenticated roundtrip and
       registry verification; legacy mode preserves send-only startup).
    2. :meth:`send_frame` — fire-and-forget; raises
       :class:`MeshTransportError` if the WS isn't currently open.
    3. :meth:`disconnect` — graceful close. Pending sends are flushed.

    Reconnect happens transparently when the WS drops mid-session.
    Pending receivers see a brief gap (no exception), then frames
    resume from the relay. The relay is responsible for replaying
    any in-flight messages addressed to us during the disconnect
    window (TS SDK assumes this; Python parity).
    """

    def __init__(
        self,
        *,
        url: str,
        identity_did: str,
        identity_signing_key: signing.SigningKey,
        identity_public_key: bytes,
        user_agent: str,
        heartbeat_interval_seconds: float,
        reconnect_initial_seconds: float,
        reconnect_max_seconds: float,
        on_frame: FrameHandler,
        entra_token: str | None = None,
        connection_auth=None,
    ) -> None:
        self._url = url
        self._identity_did = identity_did
        self._signing_key = identity_signing_key
        self._public_key = identity_public_key
        self._user_agent = user_agent
        self._heartbeat_interval = heartbeat_interval_seconds
        self._reconnect_initial = reconnect_initial_seconds
        self._reconnect_max = reconnect_max_seconds
        self._on_frame = on_frame
        # Entra-signed JWT presented on the WS connect frame when the
        # AGT relay has Entra enforcement enabled
        # (AGENTMESH_ENTRA_ENFORCE=true → relay/app.py rejects the
        # connect with "Authentication required (Entra)" when this
        # field is missing). The TS SDK includes the same token under
        # the `token` key — Python parity here.
        self._entra_token = entra_token
        self._auth = connection_auth
        self._auth_timeout = 10.0

        self._ws: websockets.WebSocketClientProtocol | None = None
        self._closing_ws: websockets.WebSocketClientProtocol | None = None
        self._recv_task: asyncio.Task[None] | None = None
        self._hb_task: asyncio.Task[None] | None = None
        self._stop_event = asyncio.Event()
        self._connected_event = asyncio.Event()

    @property
    def is_connected(self) -> bool:
        return (self._ws is not None and not self._stop_event.is_set()
                and (self._auth is None or (self._auth.ready and not self._auth.closed)))

    async def connect(self) -> None:
        """Open the WS + start the background reconnect loop. Returns
        once we are connected for the first time."""
        if self._recv_task is not None:
            if not self.is_connected:
                raise MeshTransportError("Relay is not authorized or connected")
            return
        self._stop_event.clear()
        self._connected_event.clear()
        self._recv_task = asyncio.create_task(self._run_with_reconnect())
        # Wait for first successful connection (or the loop to give up).
        await self._connected_event.wait()
        if not self.is_connected:
            raise MeshTransportError("Relay connect failed (see logs)")

    async def disconnect(self) -> None:
        """Stop the reconnect loop + close any open WS."""
        self._stop_event.set()
        self._connected_event.set()
        if self._auth is not None:
            self._auth.stop()
        if self._recv_task is not None:
            self._recv_task.cancel()
        heartbeat = self._hb_task
        if heartbeat is not None:
            heartbeat.cancel()
        if self._recv_task is not None:
            await asyncio.gather(self._recv_task, return_exceptions=True)
        if heartbeat is not None:
            await asyncio.gather(heartbeat, return_exceptions=True)
        # Retain a socket whose context-manager close failed. Required auth must
        # not release the prekey writer while an old socket may still be alive.
        for ws in (self._ws, self._closing_ws):
            if ws is not None:
                try:
                    await ws.close()
                except Exception:  # noqa: BLE001
                    if self._auth is not None:
                        raise MeshTransportError("Relay teardown incomplete; client quarantined") from None
        self._ws = self._closing_ws = None
        self._recv_task = self._hb_task = None

    async def send_frame(self, frame: dict[str, Any]) -> None:
        """Send one JSON frame over the WS. Caller is responsible for
        filling ``v``, ``type``, ``from``, ``ts``, etc. — the
        transport doesn't validate frame shape."""
        if self._auth is not None:
            self._auth.check()
        if self._ws is None:
            raise MeshTransportError("Relay WS is not connected")
        try:
            await self._ws.send(json.dumps(frame))
        except (ConnectionClosed, WebSocketException, OSError) as exc:
            if self._auth is not None:
                self._auth.fail()
            raise MeshTransportError(f"send_frame failed: {exc}") from exc

    # ── Internal: reconnect loop ────────────────────────────────────────

    async def _run_with_reconnect(self) -> None:
        backoff = self._reconnect_initial
        while not self._stop_event.is_set():
            try:
                await self._open_and_serve()
                # Clean disconnect from the relay — reset backoff.
                backoff = self._reconnect_initial
            except asyncio.CancelledError:
                raise
            except Exception as exc:  # noqa: BLE001
                if self._closing_ws is not None and self._closing_ws.state is State.CLOSED:
                    self._closing_ws = None
                if self._auth is not None and (not self._session_was_authorized or self._closing_ws is not None):
                    self._auth.fail()
                    self._stop_event.set()
                    break
                logger.warning(
                    "Relay WS dropped (%s); reconnecting in %.1fs",
                    exc,
                    backoff,
                )
            if self._stop_event.is_set():
                break
            try:
                await asyncio.wait_for(self._stop_event.wait(), timeout=backoff)
            except asyncio.TimeoutError:
                pass
            backoff = min(backoff * 2, self._reconnect_max)
        # Final signal so connect() unblocks if we never connected.
        self._connected_event.set()

    async def _open_and_serve(self) -> None:
        """Open one WS, authorize, then dispatch application frames."""
        self._session_was_authorized = False
        if self._auth is not None:
            self._entra_token = await self._auth.prepare_relay()
        logger.info("Opening relay WS: %s", self._url)
        # Preserve direct router connections when newer clients auto-detect
        # proxies; websockets 12/13 don't accept this keyword at all.
        proxy_options = {"proxy": None} if "proxy" in inspect.signature(websockets.connect).parameters else {}
        async with websockets.connect(
            self._url,
            **proxy_options,
            user_agent_header=self._user_agent,
            open_timeout=10,
            close_timeout=3,
            ping_interval=30,
            ping_timeout=10,
            max_size=1 * 1024 * 1024,
        ) as ws:
            self._ws = ws
            try:
                await self._send_connect_frame()
                pending = []
                if self._auth is not None:
                    async with asyncio.timeout(self._auth_timeout):
                        pending = await self._auth_roundtrip(ws)
                        await self._auth.verify_relay()
                    self._session_was_authorized = True
                self._connected_event.set()
                self._hb_task = asyncio.create_task(self._heartbeat_loop())
                for raw in pending:
                    await self._handle_raw(raw)
                async for raw in ws:
                    await self._handle_raw(raw)
            finally:
                # Revoke before any cleanup awaits: a concurrent tool must never
                # use the socket while close/heartbeat teardown is in progress.
                self._connected_event.clear()
                self._closing_ws = ws
                self._ws = None
                self._entra_token = None if self._auth is not None else self._entra_token
                if self._auth is not None:
                    self._auth.suspend()
                if self._hb_task is not None:
                    self._hb_task.cancel()
                    await asyncio.gather(self._hb_task, return_exceptions=True)
                    self._hb_task = None
        self._closing_ws = None

    async def _auth_roundtrip(self, ws) -> list[str]:
        # Pinned AGT relay/app.py:389-423 has no connect ACK. A nonce-bound
        # self-routed KNOCK proves it reached the authenticated frame loop
        # (:468-490). Consume it here, never as peer or mission delivery.
        probe = {
            "v": 1, "type": "knock", "from": self._identity_did,
            "to": self._identity_did, "id": str(uuid.uuid4()), "ts": _iso_utc(),
            "intent": {"action": "kars_auth_probe"},
        }
        await ws.send(json.dumps(probe))
        pending, size = [], 0
        while True:
            raw = await ws.recv()
            if not isinstance(raw, str):
                raise MeshTransportError("Unexpected relay authentication frame")
            frame = json.loads(raw)
            if frame == probe:
                return pending
            if not isinstance(frame, dict) or frame.get("type") == "error":
                raise MeshTransportError("Relay authentication rejected")
            # AGT delivers its offline inbox before reading our probe. Preserve
            # those frames, bounded and undispatched until verification succeeds.
            size += len(raw.encode("utf-8"))
            if len(pending) >= 128 or size > 1024 * 1024:
                raise MeshTransportError("Relay authentication backlog exceeded")
            pending.append(raw)

    async def reconnect(self) -> None:
        """Rotate only the socket; the owning client's prekeys remain untouched."""
        if self._auth is not None:
            self._auth.suspend()
        self._connected_event.clear()
        if self._ws is None:
            raise MeshTransportError("Relay WS is not connected")
        await self._ws.close()
        await self._connected_event.wait()
        if self._auth is not None:
            self._auth.check()

    async def _send_connect_frame(self) -> None:
        """First frame after the WS opens identifies the DID and
        proves possession of the corresponding Ed25519 secret.

        Required fields per AGT relay (relay/app.py::_verify_connect_pop,
        AGENTMESH-WIRE §10.1):

        - ``from``       — DID (did:mesh:<sha256(pub)[:32]>)
        - ``public_key`` — standard (NOT urlsafe) base64 Ed25519 public key
        - ``timestamp``  — ISO-8601 UTC within 5-minute replay window
        - ``signature``  — standard base64 Ed25519 sig over ``timestamp``

        Optional field used by Entra-enforcing relays
        (AGENTMESH_ENTRA_ENFORCE=true on the relay deployment):

        - ``token``      — Entra-signed JWT for this agent identity. The
                           relay verifies the JWT against Entra JWKS,
                           extracts ``appid`` to seed verified tier, and
                           rejects the WS with code 4003
                           "Authentication required (Entra)" if absent.
        """
        ts = _iso_utc()
        sig = self._signing_key.sign(ts.encode("utf-8")).signature
        frame: dict[str, object] = {
            "v": 1,
            "type": "connect",
            "from": self._identity_did,
            "public_key": base64.b64encode(self._public_key).decode("ascii"),
            "timestamp": ts,
            "signature": base64.b64encode(sig).decode("ascii"),
        }
        if self._entra_token:
            frame["token"] = self._entra_token
        await self._ws.send(  # type: ignore[union-attr]
            json.dumps(frame)
        )

    async def _heartbeat_loop(self) -> None:
        try:
            while True:
                await asyncio.sleep(self._heartbeat_interval)
                if self._ws is None:
                    return
                if self._auth is not None:
                    self._auth.check()
                await self._ws.send(
                    json.dumps(
                        {
                            "v": 1,
                            "type": "heartbeat",
                            "from": self._identity_did,
                            "ts": _iso_utc(),
                        }
                    )
                )
        except asyncio.CancelledError:
            pass
        except (ConnectionClosed, WebSocketException, OSError, MeshTransportError):
            if self._auth is not None:
                self._auth.fail()
            return

    async def _handle_raw(self, raw: str | bytes) -> None:
        if self._auth is not None:
            self._auth.check()
        if isinstance(raw, bytes):
            # Binary frames not currently used; ignore so an unknown
            # extension doesn't crash the receive loop.
            return
        try:
            frame = json.loads(raw)
        except json.JSONDecodeError:
            logger.warning(
                "Discarding malformed relay frame (not JSON): %r", raw[:80]
            )
            return
        if self._auth is not None and isinstance(frame, dict) and frame.get("type") == "error":
            self._auth.fail()
            raise MeshTransportError("Authenticated relay rejected an operation")
        try:
            await self._on_frame(frame)
        except Exception as exc:  # noqa: BLE001
            # User-supplied handler errors must not kill the receive
            # loop — log and continue with the next frame.
            logger.exception(
                "Frame handler raised for type=%r: %s",
                frame.get("type"),
                exc,
            )


def _iso_utc() -> str:

    return (
        datetime.now(timezone.utc)
        .isoformat(timespec="milliseconds")
        .replace("+00:00", "Z")
    )
