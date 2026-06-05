# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Autonomous mesh worker — Hermes Act 2.2.

Long-lived background loop that lets a Hermes sub-agent **respond to
inbound mesh messages without an active session**. Without this, a
spawned sub-agent is a passive daemon: its LLM only runs when something
externally invokes ``hermes -z``. A parent doing
``kars_mesh_send(to_agent="analyst", content="research X")`` would
land the message in the child's inbox queue and nothing would happen
until somebody hand-ran a Hermes session on the child.

This worker bridges that gap:

  1. Lazy-init the shared MeshClient (same singleton the
     ``kars_mesh_send`` tool uses).
  2. Drain the inbox in a background asyncio loop.
  3. For each inbound message: invoke ``hermes -z <payload>`` as a
     subprocess, capture its stdout, and reply via
     ``kars_mesh_send(to_agent=<sender_display_name>,
     content=<reply>)``.

Opt-in: only runs when ``KARS_MESH_AUTO_RESPONDER=1`` is set on the
sub-agent container. Sub-agents are spawned with this env on by
default (set in inference-router/src/spawn/mod.rs when the parent's
KARS_RUNTIME_KIND is Hermes); the parent never sets it for itself
because the parent IS the LLM-driver and would otherwise infinite-loop
on its own outbound messages.

Security: the inbound message body is fed VERBATIM to the LLM as a
prompt. Trust gating happens upstream in the AGT pre_tool_call hook
and the AGT KNOCK trust-threshold check — by the time the message is
in the inbox, it has already cleared both layers.
"""

from __future__ import annotations

import asyncio
import base64
import json
import logging
import os
import subprocess
from pathlib import Path
from typing import Any

logger = logging.getLogger("kars.hermes.mesh_worker")

# Worker singleton (process-level). Set by `start_worker()`.
_WORKER_TASK: asyncio.Task[None] | None = None
_WORKER_LOOP: asyncio.AbstractEventLoop | None = None


def _hermes_cmd(prompt: str) -> list[str]:
    """Build the hermes -z command vector for one inbound message.

    Sets HOME=/sandbox + HERMES_HOME=/sandbox/.hermes by injecting them
    into the child env (the parent's `hermes -z` daemon already runs
    with them; subprocess workers need them explicitly because the
    plugin-load environment isn't guaranteed to carry them through)."""
    return ["hermes", "-z", prompt]


def _hermes_env() -> dict[str, str]:
    env = dict(os.environ)
    env.setdefault("HOME", "/sandbox")
    env.setdefault("HERMES_HOME", "/sandbox/.hermes")
    return env


async def _resolve_sender_name(client: Any, did: str) -> str | None:
    """Reverse-lookup a peer DID → display name via the registry.

    Used so we reply with `to_agent=<name>` (the OpenClaw convention
    other plugins also use) rather than the raw DID."""
    if client._registry is None:  # noqa: SLF001 — internal but stable
        return None
    try:
        # The registry doesn't index by DID. We fall back to scanning
        # the most-recent discover hits and matching on did.
        # This is best-effort; if we can't resolve, we still reply by DID.
        agents = await client._registry.discover("", limit=200)  # noqa: SLF001
        for a in agents:
            if a.did == did:
                return a.display_name or (a.capabilities[0] if a.capabilities else None)
    except Exception:  # noqa: BLE001
        return None
    return None


async def _handle_message(client: Any, msg: Any) -> None:
    """Run hermes -z with the inbound payload as prompt; reply with the
    captured stdout via the same mesh."""
    payload_text = msg.payload.decode("utf-8", errors="replace")
    logger.info(
        "mesh_worker: invoking hermes -z for inbound msg "
        "(from=%s bytes=%d)",
        msg.from_did,
        len(msg.payload),
    )
    # Cap the per-prompt timeout so a misbehaving inbound can't pin a
    # worker forever. 25 min matches the parent's typical patience for
    # a sub-agent doing real Foundry work (research + code + image).
    timeout_seconds = float(os.environ.get("KARS_MESH_WORKER_TIMEOUT_S", "1500"))
    proc = await asyncio.create_subprocess_exec(
        *_hermes_cmd(payload_text),
        env=_hermes_env(),
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    try:
        stdout_b, stderr_b = await asyncio.wait_for(
            proc.communicate(), timeout=timeout_seconds
        )
    except asyncio.TimeoutError:
        proc.kill()
        await proc.wait()
        reply = f"WORKER_TIMEOUT after {timeout_seconds:.0f}s"
        logger.warning("mesh_worker: %s for inbound from %s", reply, msg.from_did)
    else:
        reply = stdout_b.decode("utf-8", errors="replace").strip()
        if proc.returncode != 0:
            reply = (
                f"WORKER_ERROR rc={proc.returncode}\nstdout:\n{reply}"
                f"\nstderr:\n{stderr_b.decode(errors='replace').strip()}"
            )

    # Reply via the same MeshClient. Try the friendly name first
    # (lets the sender match by display name), fall back to DID.
    sender_name = await _resolve_sender_name(client, msg.from_did)
    try:
        if sender_name:
            await client.send_by_name(to=sender_name, payload=reply.encode("utf-8"))
        else:
            await client.send_by_did(to=msg.from_did, payload=reply.encode("utf-8"))
        logger.info(
            "mesh_worker: replied %d bytes to %s",
            len(reply),
            sender_name or msg.from_did,
        )
    except Exception as exc:  # noqa: BLE001
        logger.warning(
            "mesh_worker: failed to send reply to %s: %s",
            sender_name or msg.from_did,
            exc,
        )


async def _worker_loop(get_client: Any) -> None:
    client = get_client()
    logger.info(
        "mesh_worker: loop started for did=%s (auto-respond mode)",
        client._identity.did,  # noqa: SLF001 — internal but stable
    )
    async for msg in client.inbox():
        # Spawn the handler in its own task so a slow inbound (hermes
        # -z burning minutes on real work) doesn't block the inbox
        # drain — sibling messages can arrive concurrently when the
        # parent fans out to multiple children at once.
        asyncio.create_task(_handle_message(client, msg))


def start_worker(get_client: Any) -> None:
    """Launch the auto-responder background loop. Idempotent.

    ``get_client`` is a zero-arg callable that returns the
    process-level ``MeshClient`` singleton (we call it lazily so the
    plugin doesn't trigger client init unless the worker actually
    starts)."""
    global _WORKER_TASK, _WORKER_LOOP

    if os.environ.get("KARS_MESH_AUTO_RESPONDER", "0") not in {"1", "true", "True"}:
        logger.info(
            "mesh_worker: KARS_MESH_AUTO_RESPONDER not set — skipping worker"
        )
        return

    if _WORKER_TASK is not None and not _WORKER_TASK.done():
        logger.debug("mesh_worker: worker already running, skipping")
        return

    # Reuse the mesh module's background loop so the worker shares the
    # same singleton MeshClient instance.
    from . import mesh as _mesh  # noqa: PLC0415

    _WORKER_LOOP = _mesh._get_or_init_loop()  # noqa: SLF001
    _WORKER_TASK = asyncio.run_coroutine_threadsafe(
        _worker_loop(get_client), _WORKER_LOOP
    )  # type: ignore[assignment]
    logger.info("mesh_worker: auto-responder loop scheduled")
