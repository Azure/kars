"""kars_mesh_* tool implementations — Act 2 (Python AGT MeshClient).

Replaces the Act 1 stubs at ``mesh_stubs.py`` with real implementations
backed by :mod:`kars_agt_mesh`. The four mesh tools (``kars_mesh_send``,
``kars_mesh_inbox``, ``kars_mesh_await``, ``kars_mesh_transfer_file``)
delegate to a process-singleton :class:`kars_agt_mesh.MeshClient`.

The Hermes singleton lives in :data:`_MESH_SINGLETON` and is created
lazily on first tool call so plugin discovery doesn't pay the network
cost. Identity is persisted at ``$HERMES_HOME/.agt/identity.json`` so
the DID survives container restart (but not pod restart — see Act 2.2
broker design for cross-pod-restart identity).

File transfer (``kars_mesh_transfer_file``) is still a clear-error
stub in v0.1 — chunked encrypted transfer ships in v0.2.
"""

from __future__ import annotations

import asyncio
import base64
import json
import logging
import os
import threading
from pathlib import Path
from typing import Any

from kars_agt_mesh import (
    InboundMessage,
    MeshClient,
    MeshConfig,
    MeshPeerNotFoundError,
    MeshTransportError,
)

logger = logging.getLogger("kars.hermes.mesh")

_MESH_SINGLETON: MeshClient | None = None
_MESH_LOCK = threading.Lock()
_BACKGROUND_LOOP: asyncio.AbstractEventLoop | None = None
_BACKGROUND_THREAD: threading.Thread | None = None


def _get_or_init_loop() -> asyncio.AbstractEventLoop:
    """Return a dedicated background asyncio loop running in its own
    thread. Hermes' main thread is sync (the chat REPL pumps tools
    via synchronous callbacks), so we host the async MeshClient in a
    sidecar loop and bridge each tool call with ``run_coroutine_threadsafe``.
    """
    global _BACKGROUND_LOOP, _BACKGROUND_THREAD
    if _BACKGROUND_LOOP is not None and _BACKGROUND_LOOP.is_running():
        return _BACKGROUND_LOOP
    loop = asyncio.new_event_loop()
    ready = threading.Event()

    def _run() -> None:
        asyncio.set_event_loop(loop)
        ready.set()
        loop.run_forever()

    t = threading.Thread(target=_run, name="kars-mesh-loop", daemon=True)
    t.start()
    ready.wait(timeout=2.0)
    _BACKGROUND_LOOP = loop
    _BACKGROUND_THREAD = t
    return loop


def _get_or_init_client() -> MeshClient:
    """Process-singleton MeshClient. First call connects (registers
    with registry + opens relay WS). Subsequent calls share state."""
    global _MESH_SINGLETON
    with _MESH_LOCK:
        if _MESH_SINGLETON is not None:
            return _MESH_SINGLETON

        name = os.environ.get("SANDBOX_NAME", "hermes")
        # Mesh routing through the loopback inference-router. The
        # egress-guard drops all UID-1000 TCP except DNS + loopback
        # + ESTABLISHED, so direct egress to the agentmesh service
        # wouldn't work — UID 1000 is iptables-confined to localhost.
        # The router has built-in proxies at `/agt/relay` (WebSocket)
        # and `/agt/registry/*` (HTTP) that forward to the cluster
        # services using the router's own AGT_RELAY_URL/AGT_REGISTRY_URL
        # (which are the cluster-DNS targets, set on the *router*
        # container by the controller).
        #
        # AGT_RELAY_URL / AGT_REGISTRY_URL on the AGENT container
        # point at the upstream cluster services and would be unusable
        # here (port 8765/8080 are blocked by the egress-guard). We
        # deliberately do NOT honour them on the agent side — the
        # OpenClaw runtime makes the same choice in
        # ``runtimes/openclaw/src/core/mesh-registry.ts`` (always
        # ``routerUrl("/agt/registry")``).
        relay_url = "ws://127.0.0.1:8443/agt/relay"
        registry_url = "http://127.0.0.1:8443/agt/registry"
        hermes_home = Path(os.environ.get("HERMES_HOME", "/sandbox/.hermes"))
        identity_path = hermes_home / ".agt" / "identity.json"
        trust_threshold = int(os.environ.get("AGT_TRUST_THRESHOLD", "0"))

        config = MeshConfig(
            name=name,
            relay_url=relay_url,
            registry_url=registry_url,
            identity_path=identity_path,
            trust_threshold=trust_threshold,
            user_agent=f"kars-agt-mesh/0.1.0 (hermes/{os.environ.get('HERMES_VERSION','0.15.2')})",
        )
        client = MeshClient(config)

        # Connect on the background loop. Blocks until first connection
        # succeeds OR the registry rejects us.
        loop = _get_or_init_loop()
        future = asyncio.run_coroutine_threadsafe(client.connect(), loop)
        future.result(timeout=30.0)

        _MESH_SINGLETON = client
        logger.info(
            "MeshClient ready: name=%s relay=%s registry=%s",
            name,
            relay_url,
            registry_url,
        )
        return client


# ── Hermes tool handlers ────────────────────────────────────────────────


def _kars_mesh_send(args: dict[str, Any], **_kwargs: Any) -> str:
    """``kars_mesh_send(to=<display_name>, payload=<base64-or-string>)``"""
    try:
        client = _get_or_init_client()
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"Mesh client init failed: {exc}"})

    peer = str(args.get("to", "")).strip()
    if not peer:
        return json.dumps({"error": "missing required arg: to=<display_name>"})

    payload_raw = args.get("payload", "")
    payload = (
        payload_raw.encode("utf-8") if isinstance(payload_raw, str) else bytes(payload_raw)
    )

    loop = _get_or_init_loop()
    try:
        future = asyncio.run_coroutine_threadsafe(
            client.send_by_name(to=peer, payload=payload), loop
        )
        future.result(timeout=30.0)
        return json.dumps({"ok": True, "to": peer, "bytes": len(payload)})
    except MeshPeerNotFoundError as exc:
        return json.dumps({"error": f"Peer {peer!r} not found: {exc}"})
    except MeshTransportError as exc:
        return json.dumps({"error": f"Transport error: {exc}"})
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"send failed: {exc}"})


def _kars_mesh_inbox(_args: dict[str, Any], **_kwargs: Any) -> str:
    """``kars_mesh_inbox()`` — drain all currently-queued messages
    without blocking. Returns ``{"messages": [...]}``."""
    try:
        client = _get_or_init_client()
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"Mesh client init failed: {exc}"})

    loop = _get_or_init_loop()
    drained: list[dict[str, Any]] = []

    async def _drain() -> None:
        # Non-blocking drain: try to pull as many messages as are
        # immediately available, no waiting.
        queue = client._inbox  # noqa: SLF001 — internal but stable
        while not queue.empty():
            msg: InboundMessage = await queue.get()
            drained.append(
                {
                    "from_did": msg.from_did,
                    "from_display_name": msg.from_display_name,
                    "payload_b64": base64.b64encode(msg.payload).decode("ascii"),
                    "message_id": msg.message_id,
                    "received_at": msg.received_at.isoformat(),
                }
            )

    future = asyncio.run_coroutine_threadsafe(_drain(), loop)
    future.result(timeout=5.0)
    return json.dumps({"messages": drained, "count": len(drained)})


def _kars_mesh_await(args: dict[str, Any], **_kwargs: Any) -> str:
    """``kars_mesh_await(senders=[name,...], timeout_seconds=N)`` —
    block until at least one message arrives from each listed sender
    or the timeout fires."""
    try:
        client = _get_or_init_client()
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"Mesh client init failed: {exc}"})

    senders = list(args.get("senders") or [])
    timeout = float(args.get("timeout_seconds", 300))
    expected: set[str] = set(senders)

    loop = _get_or_init_loop()
    drained: list[dict[str, Any]] = []

    async def _wait() -> None:
        deadline = asyncio.get_event_loop().time() + timeout
        seen_names: set[str] = set()
        async for msg in client.inbox():
            drained.append(
                {
                    "from_did": msg.from_did,
                    "from_display_name": msg.from_display_name,
                    "payload_b64": base64.b64encode(msg.payload).decode("ascii"),
                    "message_id": msg.message_id,
                }
            )
            if msg.from_display_name:
                seen_names.add(msg.from_display_name)
            if expected and seen_names.issuperset(expected):
                return
            remaining = deadline - asyncio.get_event_loop().time()
            if remaining <= 0:
                return

    future = asyncio.run_coroutine_threadsafe(_wait(), loop)
    try:
        future.result(timeout=timeout + 5.0)
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"await failed: {exc}", "partial": drained})
    return json.dumps(
        {"messages": drained, "count": len(drained), "completed": True}
    )


def _kars_mesh_transfer_file(_args: dict[str, Any], **_kwargs: Any) -> str:
    """Chunked encrypted file transfer — deferred to v0.2."""
    return json.dumps(
        {
            "error": (
                "kars_mesh_transfer_file not yet implemented in mesh "
                "v0.1 (small-messages only). Use kars_mesh_send with "
                "base64-encoded chunks for now, or wait for v0.2."
            ),
        }
    )


_MESH_TOOLS = [
    (
        "kars_mesh_send",
        "Send an encrypted message to a peer agent by display name "
        "(real impl, Act 2.1 — Python AGT MeshClient).",
        _kars_mesh_send,
    ),
    (
        "kars_mesh_inbox",
        "Drain currently-queued mesh messages without blocking.",
        _kars_mesh_inbox,
    ),
    (
        "kars_mesh_await",
        "Block until messages arrive from the requested senders or "
        "the timeout fires.",
        _kars_mesh_await,
    ),
    (
        "kars_mesh_transfer_file",
        "Transfer file via mesh (NOT yet implemented in v0.1; v0.2 "
        "adds chunked encrypted transfer).",
        _kars_mesh_transfer_file,
    ),
]


def register(ctx: Any) -> None:  # noqa: ANN401
    """Register all four kars_mesh_* tools with the Hermes plugin
    context. Lazy client init means a Hermes process that never
    invokes a mesh tool never opens a relay connection."""
    for name, desc, handler in _MESH_TOOLS:
        ctx.register_tool(
            name=name,
            toolset="kars_mesh",
            schema={
                "type": "object",
                "description": desc,
                "properties": {
                    "to": {"type": "string", "description": "Peer display name (send/transfer only)"},
                    "payload": {"type": "string", "description": "Message bytes (UTF-8 string or base64)"},
                    "senders": {"type": "array", "items": {"type": "string"}, "description": "Senders to await (await only)"},
                    "timeout_seconds": {"type": "number", "description": "Await timeout (await only, default 300)"},
                },
            },
            handler=handler,
            emoji="🕸",
        )
    logger.info("kars_mesh_* family registered (4 tools, Act 2.1 — real MeshClient)")
