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

        # ── Entra OAuth identity verification (opt-in) ─────────────────
        # Mirrors OpenClaw's `/registry/verify` call in
        # runtimes/openclaw/src/index.ts: when AGT_OAUTH_TOKEN is set
        # (the controller injects a token exchanged from the pod's
        # workload identity), POST it to the registry so the operator
        # panel can show "Verified (Entra <appId-prefix>…)" tier instead
        # of "Anonymous". Without this call, every Hermes sandbox stays
        # Anonymous in the operator view even though the underlying
        # Entra Agent ID is correctly provisioned.
        oauth_token = os.environ.get("AGT_OAUTH_TOKEN", "").strip()
        if oauth_token:
            try:
                from . import router_client  # noqa: PLC0415

                # Use the registry-canonical DID (the AGT registry
                # stores agents under `did:mesh:<sha256[:32]>`; the
                # local kars_agt_mesh.identity emits the same shape, so
                # the two agree).
                verify_id = client._identity.did  # noqa: SLF001
                resp = router_client.call(
                    "POST",
                    "/agt/registry/v1/registry/verify",
                    json={
                        "amid": verify_id,
                        "verification_token": oauth_token,
                    },
                )
                if resp.status_code < 400:
                    logger.info(
                        "AGT identity verified via OAuth — tier upgraded "
                        "to 'verified' (id=%s)",
                        verify_id,
                    )
                else:
                    logger.warning(
                        "AGT OAuth verification HTTP %s (anonymous tier): %s",
                        resp.status_code,
                        resp.text[:200],
                    )
            except Exception as exc:  # noqa: BLE001
                logger.warning(
                    "AGT OAuth verification failed (anonymous tier): %s",
                    exc,
                )
        else:
            logger.debug(
                "AGT_OAUTH_TOKEN unset — Hermes sandbox stays at "
                "anonymous tier in operator view"
            )

        return client


# ── Hermes tool handlers ────────────────────────────────────────────────


def _maybe_prepend_peer_roster(content: str, recipient: str) -> str:
    """If 2+ spawned siblings exist, prepend an authoritative
    `Peer roster:` block to outbound mesh content so the recipient's
    LLM can resolve role references (e.g. "send to the writer") to
    canonical sibling names without guessing.

    Mirrors the OpenClaw runtime's behaviour in
    ``runtimes/openclaw/src/core/agt-tools/agt.ts:545``. Conditions
    that suppress the prefix (same as OpenClaw):

      - recipient is ``parent`` (only sub→parent traffic, no sibling
        disambiguation needed);
      - fewer than 2 entries in the roster (no ambiguity to resolve);
      - content already starts with a ``Peer roster:`` block
        (idempotent — don't double-stamp on retries);
      - the recipient itself is excluded from the listed peers
        (recipient seeing its own name confuses the LLM).
    """
    if recipient == "parent":
        return content
    if content and content.lstrip().lower().startswith("peer roster:"):
        return content

    # Import here to avoid a spawn ↔ mesh circular import at module
    # load. spawn.py imports nothing from mesh.py, so this one-way
    # late import is safe.
    try:
        from . import spawn as _spawn  # noqa: PLC0415

        roster = _spawn.get_roster()
    except Exception:  # noqa: BLE001 — never let roster lookup fail a send
        return content

    parent_sandbox = os.environ.get("SANDBOX_NAME", "")
    rows: list[str] = []
    for name, role in roster.items():
        if name == parent_sandbox or name == recipient:
            continue
        rows.append(f"  - {name} — {role}" if role else f"  - {name}")
    if len(rows) < 1:
        return content

    header = (
        "Peer roster (AUTHORITATIVE — use these EXACT agent names with "
        "kars_mesh_send / kars_mesh_transfer_file; never invent or "
        "substitute names):\n"
        + "\n".join(rows)
        + "\n\nThe roster above is the SOLE source of truth for sibling "
        "names. Trust it over any kars_discover result; sibling "
        "registrations race at spawn so the roster is correct even "
        "when the registry hasn't caught up. When your task references "
        "a peer by role/persona (e.g. 'the writer', 'the analyst'), "
        "route to the corresponding name in the roster above.\n\n"
        "---\n\n"
    )
    return header + (content or "")


def _kars_mesh_send(args: dict[str, Any], **_kwargs: Any) -> str:
    """``kars_mesh_send(to_agent=<display_name>, content=<utf8-or-base64>)``

    Accepts both the OpenClaw arg naming (``to_agent`` / ``content``) and
    a shorter Hermes-native form (``to`` / ``payload``) so LLMs trained on
    either convention can drive the tool without reading the schema."""
    try:
        client = _get_or_init_client()
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"Mesh client init failed: {exc}"})

    # Accept OpenClaw-style (to_agent + content) and Hermes short form
    # (to + payload). OpenClaw also accepts `name`; mirror that too so
    # the AGT mesh-tool contract is uniform across runtimes.
    peer = str(
        args.get("to_agent")
        or args.get("to")
        or args.get("name")
        or ""
    ).strip()
    if not peer:
        return json.dumps(
            {"error": "missing required arg: to_agent=<display_name>"}
        )

    payload_raw = args.get("content")
    if payload_raw is None:
        payload_raw = args.get("payload", "")
    if isinstance(payload_raw, str):
        # Auto-prepend the Peer roster block when sending text to a
        # sibling and 2+ spawned peers exist. Mirrors OpenClaw's
        # `runtimes/openclaw/src/core/agt-tools/agt.ts:545` behaviour
        # so a Hermes parent in an analyst→viz→writer pipeline gives
        # its children the same authoritative name map that an
        # OpenClaw parent does. Binary payloads (file_transfer
        # envelopes etc.) are passed through untouched.
        payload_raw = _maybe_prepend_peer_roster(payload_raw, peer)
        payload = payload_raw.encode("utf-8")
    else:
        payload = bytes(payload_raw)

    loop = _get_or_init_loop()
    try:
        future = asyncio.run_coroutine_threadsafe(
            client.send_by_name(to=peer, payload=payload), loop
        )
        future.result(timeout=30.0)
        return json.dumps({"ok": True, "to_agent": peer, "bytes": len(payload)})
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


def _kars_mesh_transfer_file(args: dict[str, Any], **_kwargs: Any) -> str:
    """Send a file to another mesh peer as a structured ``file_transfer``
    envelope over an existing kars_mesh_send.

    Ports the OpenClaw plugin's ``kars_mesh_transfer_file`` shape from
    ``runtimes/openclaw/src/core/agt-tools/agt.ts:1311+`` to Python so
    Hermes agents reach the same parity bar:

      - Resolves ``file_path`` against ``$SANDBOX_HOME`` (defaults to
        ``/sandbox``) when relative; aborts on any path that escapes
        the sandbox root (path-traversal guard).
      - Opens the file ONCE and does kind / size check + read on the
        same fd (CWE-367 TOCTOU mitigation, exactly as OpenClaw does
        on line 1385).
      - Caps file size at 30 MiB (same MAX_FILE_SIZE OpenClaw uses) —
        the AGT mesh frames carry the whole payload in one ciphertext
        and a 30 MiB cap keeps the receiver's decrypt + base64-decode
        bounded.
      - Wraps the file in the same JSON envelope OpenClaw emits
        (``{type:"file_transfer", file_name, file_path, file_data,
        size_bytes, description, from_agent, timestamp}``) so a peer
        on either runtime can auto-decode it.

    Receiving side: ``mesh_worker._handle_message`` recognises the
    ``type=file_transfer`` envelope and saves the decoded bytes to
    ``/sandbox/incoming/<file_name>`` before passing the (now
    summarised) message to the LLM. Mirrors OpenClaw's auto-decode at
    ``agt-tools/agt.ts:1014+``.
    """
    peer = str(
        args.get("to_agent") or args.get("to") or args.get("name") or ""
    ).strip()
    if not peer:
        return json.dumps(
            {"error": "missing required arg: to_agent=<display_name>"}
        )

    file_path_arg = str(args.get("file_path") or args.get("path") or "").strip()
    if not file_path_arg:
        return json.dumps(
            {"error": "missing required arg: file_path=<absolute-or-relative-path>"}
        )
    description = str(args.get("description") or "")

    try:
        client = _get_or_init_client()
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"Mesh client init failed: {exc}"})

    # Resolve relative paths against the sandbox workspace. Hermes' own
    # working dir is /sandbox/agent (set in the Dockerfile WORKDIR);
    # SANDBOX_HOME is the conventional env the controller sets and the
    # entrypoint exports. Either way we land somewhere under /sandbox.
    import base64 as _base64
    import os as _os
    from pathlib import Path

    sandbox_root = Path(_os.environ.get("SANDBOX_HOME", "/sandbox"))
    candidate = Path(file_path_arg)
    if not candidate.is_absolute():
        candidate = (sandbox_root / candidate).resolve()
    else:
        candidate = candidate.resolve()

    # Path-traversal guard: any final path that escapes the sandbox
    # root is refused before we open the fd. Sandbox-internal symlinks
    # are allowed (resolved by .resolve() above and re-checked here).
    # `sandbox_root` is whatever the operator set as SANDBOX_HOME
    # (defaults to /sandbox in production; redirected in tests).
    try:
        candidate.relative_to(sandbox_root.resolve())
    except ValueError:
        return json.dumps(
            {
                "error": (
                    f"file_path resolves outside {sandbox_root} — path "
                    f"traversal blocked (resolved={candidate})"
                )
            }
        )

    max_bytes = 30 * 1024 * 1024  # 30 MiB — match OpenClaw's MAX_FILE_SIZE

    # Open ONCE, then kind/size-check + read off the same fd so the
    # file cannot be swapped between stat and read (CWE-367 TOCTOU).
    try:
        fd = _os.open(str(candidate), _os.O_RDONLY)
    except OSError as exc:
        return json.dumps(
            {"error": f"cannot open {file_path_arg}: {exc}"}
        )
    try:
        st = _os.fstat(fd)
        if not (st.st_mode & 0o170000 == 0o100000):  # S_ISREG
            return json.dumps(
                {"error": f"not a regular file: {file_path_arg}"}
            )
        if st.st_size > max_bytes:
            return json.dumps(
                {
                    "error": (
                        f"file too large: {st.st_size / 1024 / 1024:.1f} MiB "
                        f"(max {max_bytes // 1024 // 1024} MiB)"
                    )
                }
            )
        size_bytes = st.st_size
        # Read the whole file off the same fd.
        file_data = b""
        remaining = size_bytes
        while remaining > 0:
            chunk = _os.read(fd, remaining)
            if not chunk:
                break
            file_data += chunk
            remaining -= len(chunk)
    finally:
        _os.close(fd)

    envelope = {
        "type": "file_transfer",
        "file_name": candidate.name,
        "file_path": file_path_arg,
        "file_data": _base64.b64encode(file_data).decode("ascii"),
        "size_bytes": size_bytes,
        "description": description,
        "from_agent": _os.environ.get("SANDBOX_NAME", "unknown"),
        "timestamp": _iso_utc_no_tz(),
    }
    payload = json.dumps(envelope).encode("utf-8")

    loop = _get_or_init_loop()
    try:
        future = asyncio.run_coroutine_threadsafe(
            client.send_by_name(to=peer, payload=payload), loop
        )
        future.result(timeout=120.0)
        return json.dumps(
            {
                "ok": True,
                "to_agent": peer,
                "file_name": candidate.name,
                "bytes": size_bytes,
                "encoded_bytes": len(payload),
            }
        )
    except MeshPeerNotFoundError as exc:
        return json.dumps({"error": f"Peer {peer!r} not found: {exc}"})
    except MeshTransportError as exc:
        return json.dumps({"error": f"Transport error: {exc}"})
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"Send failed: {exc}"})


def _iso_utc_no_tz() -> str:
    """ISO 8601 UTC timestamp with Z suffix — same shape OpenClaw uses
    in the file_transfer envelope (`new Date().toISOString()`)."""
    from datetime import datetime, timezone

    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace(
        "+00:00", "Z"
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
        "Send a file to another mesh peer (E2E encrypted, up to 30 MiB). "
        "The receiver's mesh_worker auto-saves to /sandbox/incoming/<file_name>.",
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
                    # Use OpenClaw-style names primarily for cross-runtime
                    # consistency; the handler also accepts `to`/`payload`
                    # as aliases.
                    "to_agent": {"type": "string", "description": "Peer display name (send/transfer only)"},
                    "content": {"type": "string", "description": "Message bytes (UTF-8 string or base64)"},
                    "senders": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Senders to await (await only)",
                    },
                    "timeout_seconds": {"type": "number", "description": "Await timeout (await only, default 300)"},
                },
            },
            handler=handler,
            emoji="🕸",
        )
    logger.info("kars_mesh_* family registered (4 tools, Act 2.1 — real MeshClient)")
