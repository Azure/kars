# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

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
import atexit
import base64
import json
import logging
import os
import re
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
_MESH_PENDING_CLEANUP: MeshClient | None = None
_MESH_WORKER_REQUESTED = False
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


def _get_or_init_client(*, start_worker: bool = False) -> MeshClient:
    """Process-singleton MeshClient. First call connects (registers
    with registry + opens relay WS). Subsequent calls share state."""
    global _MESH_SINGLETON, _MESH_PENDING_CLEANUP, _MESH_WORKER_REQUESTED
    if threading.current_thread() is _BACKGROUND_THREAD:
        if _MESH_SINGLETON is None:
            raise MeshTransportError("Mesh is unavailable; initialization must run outside its loop")
        if _MESH_SINGLETON.connection_auth is not None:
            _MESH_SINGLETON.connection_auth.check()
        return _MESH_SINGLETON
    with _MESH_LOCK:
        from . import sre  # noqa: PLC0415

        if sre.is_enabled():
            raise MeshTransportError("Mesh is disabled in SRE mode")
        _MESH_WORKER_REQUESTED |= start_worker
        required_entra = os.environ.get("MESH_AUTH_BACKEND") == "EntraAgentIdentity"
        skip_entra = os.environ.get("AGT_SKIP_ENTRA") == "1"
        if required_entra and skip_entra:
            raise MeshTransportError("EntraAgentIdentity conflicts with AGT_SKIP_ENTRA=1")
        if skip_entra or required_entra:
            os.environ.pop("AGT_OAUTH_TOKEN", None)
        oauth_token = os.environ.get("AGT_OAUTH_TOKEN", "").strip()
        if _MESH_SINGLETON is not None:
            client = _MESH_SINGLETON
            if client.connection_auth is not None:
                # All lease state is mutated on its owning event loop.
                asyncio.run_coroutine_threadsafe(
                    _check_client(client), _get_or_init_loop()
                ).result(timeout=15)
            if start_worker:
                from . import mesh_worker

                mesh_worker.start_worker(_get_or_init_client)
            return client
        if _MESH_PENDING_CLEANUP is not None:
            # The SDK caches clients itself. If a previous disconnect raised,
            # finish that teardown before its cached object can be reused.
            from .mesh_auth import bounded_cleanup

            asyncio.run_coroutine_threadsafe(
                bounded_cleanup(_MESH_PENDING_CLEANUP), _get_or_init_loop()
            ).result(timeout=15)
            _MESH_PENDING_CLEANUP = None
        if oauth_token:
            os.environ["AGT_OAUTH_TOKEN"] = oauth_token

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
        mission = None
        if os.environ.get("KARS_MISSION_DISPATCH_ENABLED") == "true":
            from .mission_receiver import bootstrap

            mission = bootstrap(hermes_home)
            identity_path = mission[0]
        trust_threshold = 0 if skip_entra else int(os.environ.get("AGT_TRUST_THRESHOLD", "0"))

        config = MeshConfig(
            name=name,
            relay_url=relay_url,
            registry_url=registry_url,
            identity_path=identity_path,
            trust_threshold=trust_threshold,
            user_agent=f"kars-agt-mesh/0.1.0 (hermes/{os.environ.get('HERMES_VERSION','0.15.2')})",
        )
        client = MeshClient(config)
        receiver = None
        if mission is not None:
            from .mission_receiver import MissionReceiver

            receiver = getattr(client, "_kars_mission_receiver", None) or MissionReceiver(client, *mission[1:])

        # Keep initialization and cleanup on the owning loop. The timeout
        # must cancel connect itself, not just abandon its waiting caller.
        # Do not expose the client to tools/the inbox worker until verified.
        loop = _get_or_init_loop()
        _MESH_PENDING_CLEANUP = client
        future = asyncio.run_coroutine_threadsafe(
            _connect_and_verify(client, oauth_token, required_entra), loop
        )
        future.result(timeout=60)

        asyncio.run_coroutine_threadsafe(_publish_client(client, receiver), loop).result(timeout=15)
        logger.info(
            "MeshClient ready: name=%s relay=%s registry=%s",
            name,
            relay_url,
            registry_url,
        )
        return client


async def _publish_client(client, receiver):
    global _MESH_SINGLETON, _MESH_PENDING_CLEANUP
    if client.connection_auth is not None:
        client.connection_auth.check()
    client._kars_mission_receiver = receiver
    _MESH_PENDING_CLEANUP = None
    _MESH_SINGLETON = client
    from . import mesh_worker

    if _MESH_WORKER_REQUESTED or (mesh_worker._WORKER_TASK is not None and mesh_worker._WORKER_TASK.done()):
        mesh_worker.start_worker(_get_or_init_client)


async def _check_client(client):
    client.connection_auth.check()


def _quarantine(client):
    global _MESH_SINGLETON, _MESH_PENDING_CLEANUP
    if _MESH_SINGLETON is client:
        _MESH_SINGLETON = None
    _MESH_PENDING_CLEANUP = client


async def _connect_and_verify(client: MeshClient, oauth_token: str, required_entra: bool) -> None:
    from .mesh_auth import RequiredEntraLease, bounded_cleanup

    if os.environ.get("KARS_MISSION_DISPATCH_ENABLED") == "true":
        from .mesh_worker import _is_reserved_message

        client.partition_inbox(_is_reserved_message)
    client.connection_auth = RequiredEntraLease(client, _quarantine) if required_entra else None
    try:
        if required_entra:
            await client.connection_auth.mint_initial()
        await asyncio.wait_for(client.connect(), timeout=30.0)
        if required_entra:
            client.connection_auth.check()
        elif oauth_token:
            try:
                # The router client has finite HTTP timeouts. Keep its sync
                # request off the mesh loop so the relay can still shut down.
                await asyncio.to_thread(_verify_identity, client, oauth_token)
            except Exception:  # noqa: BLE001
                logger.warning("Optional AGT OAuth verification failed (anonymous tier)")
        else:
            logger.debug("AGT_OAUTH_TOKEN unset — anonymous mesh explicitly permitted by configuration")
    except BaseException:
        # Includes connect cancellation/timeouts and verification rejection.
        # disconnect closes the relay/HTTP client and releases the SDK's
        # exclusive prekey lock; a later getter can safely retry the same client.
        if client.connection_auth is not None:
            client.connection_auth.stop()
        await bounded_cleanup(client)
        raise


def shutdown_mesh() -> None:
    """Bounded process shutdown; failed teardown stays quarantined, never unlocked."""
    global _MESH_SINGLETON, _MESH_PENDING_CLEANUP
    from .mesh_auth import bounded_cleanup

    with _MESH_LOCK:
        client = _MESH_SINGLETON or _MESH_PENDING_CLEANUP
        if client is None or _BACKGROUND_LOOP is None or not _BACKGROUND_LOOP.is_running():
            return
        _quarantine(client)
        try:
            asyncio.run_coroutine_threadsafe(bounded_cleanup(client), _BACKGROUND_LOOP).result(timeout=15)
        except Exception:
            logger.error("Mesh shutdown incomplete; client quarantined")
        else:
            _MESH_PENDING_CLEANUP = None


# Hermes 0.16.0 has session hooks, not a process-shutdown hook. Its gateway
# handles SIGTERM/SIGINT with a graceful return; retain those signal handlers.
atexit.register(shutdown_mesh)


def _verify_identity(client: MeshClient, oauth_token: str) -> None:
    from . import router_client  # noqa: PLC0415

    verify_id = client._identity.did  # noqa: SLF001
    resp = router_client.call(
        "POST",
        "/agt/registry/v1/registry/verify",
        json={"amid": verify_id, "verification_token": oauth_token},
    )
    if not 200 <= resp.status_code < 300:
        # Do not include the response body: an upstream may echo credentials.
        raise MeshTransportError(f"AGT OAuth verification HTTP {resp.status_code}")
    logger.info("AGT identity verified via OAuth (id=%s)", verify_id)


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
    if not str(args.get("to_agent") or args.get("to") or args.get("name") or "").strip():
        return json.dumps({"error": "missing required arg: to_agent=<display_name>"})
    try:
        client = _get_or_init_client()
    except Exception as exc:
        return json.dumps({"error": f"Mesh client init failed: {exc}"})
    future = None
    try:
        future = asyncio.run_coroutine_threadsafe(_send_peer(client, args), _get_or_init_loop())
        return future.result(timeout=35)
    except Exception as exc:
        if future is not None:
            future.cancel()
        return json.dumps({"error": f"send failed: {exc}"})


async def _send_peer(client, args):
    """``kars_mesh_send(to_agent=<display_name-or-DID>, content=<utf8>)``

    Accepts both the OpenClaw arg naming (``to_agent`` / ``content``) and
    a shorter Hermes-native form (``to`` / ``payload``) so LLMs trained on
    either convention can drive the tool without reading the schema."""
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

    try:
        async with asyncio.timeout(30):
            if peer.startswith("did:"):
                if not re.fullmatch(r"did:mesh:[a-f0-9]{32}", peer):
                    return json.dumps({"error": "Invalid peer DID"})
                await client.send_by_did(to=peer, payload=payload)
            else:
                await client.send_by_name(to=peer, payload=payload)
        return json.dumps({"ok": True, "to_agent": peer, "bytes": len(payload)})
    except MeshPeerNotFoundError as exc:
        return json.dumps({"error": f"Peer {peer!r} not found: {exc}"})
    except MeshTransportError as exc:
        return json.dumps({"error": f"Transport error: {exc}"})
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"send failed: {exc}"})


def _message_result(msg: InboundMessage, names: dict[str, str] | None = None) -> dict[str, Any]:
    result = {
        "from_did": msg.from_did,
        "from_display_name": msg.from_display_name or (names or {}).get(msg.from_did),
        "payload_b64": base64.b64encode(msg.payload).decode("ascii"),
        "message_id": msg.message_id,
        "received_at": msg.received_at.isoformat(),
    }
    try:
        result["content"] = msg.payload.decode("utf-8")
    except UnicodeDecodeError:
        pass  # Binary messages retain their lossless base64 representation.
    return result


def _kars_mesh_inbox(_args: dict[str, Any], **_kwargs: Any) -> str:
    """Drain live peer messages, never installed mission protocol envelopes."""
    future = None
    try:
        client = _get_or_init_client()

        future = asyncio.run_coroutine_threadsafe(_drain_peer(client), _get_or_init_loop())
        return json.dumps(future.result(timeout=5.0))
    except Exception as exc:  # noqa: BLE001
        if future is not None:
            future.cancel()
        return json.dumps({"error": f"inbox failed: {exc}"})


class _PeerResultBudget:
    def __init__(self, limit):
        self.limit, self.used, self.truncated = limit, 0, False

    def accepts(self, msg):
        if self.limit is None:
            return True
        # Check raw length before allocating base64 for a large file transfer.
        size = len(msg.payload) * 4 // 3
        if size <= self.limit:
            size = len(json.dumps(_message_result(msg)).encode()) + 512
        if self.used + size > self.limit:
            self.truncated = True
            return False
        self.used += size
        return True

    def result(self, messages, **fields):
        result = {"messages": messages, "count": len(messages), **fields}
        if self.truncated:
            result["truncated"] = True
        return result


async def _drain_peer(client, *, max_bytes=None):
    budget = _PeerResultBudget(max_bytes)
    messages = [_message_result(msg) for msg in client.drain_inbox(budget.accepts)]
    return budget.result(messages)


async def _await_peer(client, senders, timeout, *, max_bytes=None):
    stamp = client._auth_stamp()
    names: dict[str, str] = {}
    remaining = set(senders)
    unresolved = {name for name in senders if not name.startswith("did:mesh:")}
    loop = asyncio.get_running_loop()
    retry_at = 0.0
    drained = []
    budget = _PeerResultBudget(max_bytes)
    try:
        async with asyncio.timeout(timeout):
            def sender(msg):
                return names.get(msg.from_did) or msg.from_display_name or msg.from_did

            def match(msg):
                return (not remaining or sender(msg) in remaining or msg.from_did in remaining) and budget.accepts(msg)

            while True:
                client._check_auth(stamp)
                if unresolved and loop.time() >= retry_at:
                    for name in tuple(unresolved):
                        peer = await client._registry.find_by_display_name(name)
                        client._check_auth(stamp)
                        if peer is not None:
                            names[peer.did] = name
                            unresolved.remove(name)
                    retry_at = loop.time() + 1.0
                retry_delay = max(0.0, retry_at - loop.time()) if unresolved else None
                try:
                    # Peers may register after this wait starts; keep their messages
                    # queued until discovery binds the name to an authenticated DID.
                    async with asyncio.timeout(retry_delay):
                        msg = await client.receive_inbox(match)
                except TimeoutError:
                    client._check_auth(stamp)
                    continue
                client._check_auth(stamp)
                drained.append(_message_result(msg, names))
                remaining.discard(sender(msg))
                remaining.discard(msg.from_did)
                if not remaining:
                    return budget.result(drained, completed=True)
    except TimeoutError:
        client._check_auth(stamp)
        return budget.result(drained, completed=False)


def _kars_mesh_await(args: dict[str, Any], **_kwargs: Any) -> str:
    """Wait for each requested sender (or one peer if none), at most 300s.

    Unmatched senders remain queued. Registry resolution matches authenticated
    DIDs, not untrusted names in peer payloads. Timeout includes that lookup.
    """
    import math

    future = None
    try:
        senders = args.get("senders") or []
        if not isinstance(senders, list) or any(not isinstance(s, str) or not s.strip() for s in senders):
            raise ValueError("senders must be a list of nonempty names or DIDs")
        timeout = float(args.get("timeout_seconds", 300))
        if not math.isfinite(timeout) or not 0 < timeout <= 300:
            raise ValueError("timeout_seconds must be greater than 0 and at most 300")
        client = _get_or_init_client()

        future = asyncio.run_coroutine_threadsafe(_await_peer(client, senders, timeout), _get_or_init_loop())
        return json.dumps(future.result(timeout=timeout + 5.0))
    except Exception as exc:  # noqa: BLE001
        if future is not None:
            future.cancel()
        # A failed authorization/cancelled wait must not publish buffered data
        # from its former epoch. Normal timeouts return their live partial set.
        return json.dumps({"error": f"await failed: {exc}", "partial": []})


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
