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
import logging
import os
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
    other plugins also use), and so the operator's per-sandbox AGT
    trust panel shows a human-readable peer name instead of the raw
    `did:mesh:<hex>`. Prefer the direct `GET /v1/agents/<did>` path:
    it's O(1) on the registry side and works for any registered DID,
    versus the previous discover-and-scan loop which silently returned
    nothing because the AGT registry rejects `discover("")` (empty
    capability)."""
    if client._registry is None:  # noqa: SLF001 — internal but stable
        return None
    try:
        agent = await client._registry.get_agent(did)  # noqa: SLF001
    except Exception:  # noqa: BLE001
        return None
    if agent is None:
        return None
    return agent.display_name or (agent.capabilities[0] if agent.capabilities else None)


def _maybe_save_file_transfer(
    payload_text: str, msg: Any, client: Any
) -> str:
    """If ``payload_text`` is a JSON ``file_transfer`` envelope (the
    shape ``kars_mesh_transfer_file`` emits on both runtimes), decode
    the base64 file_data and save it to ``/sandbox/incoming/<file_name>``.

    Returns the prompt text to feed the LLM:
      - on success: a short human-readable summary pointing at the
        saved path (so the LLM sees what arrived without the 30 MiB
        of base64 in its context window);
      - on failure or non-file_transfer payloads: the original
        ``payload_text`` unchanged.
    """
    import base64 as _base64
    import json as _json
    from pathlib import Path as _Path

    try:
        parsed = _json.loads(payload_text)
    except (ValueError, TypeError):
        return payload_text
    if not isinstance(parsed, dict):
        return payload_text
    # OpenClaw's kars_mesh_send wraps the LLM-provided `content` in an
    # outer `task_request` envelope (runtimes/openclaw/src/core/agt-
    # tools/agt.ts). The actual file_transfer JSON is the (escaped)
    # inner `content` field. Unwrap one level before pattern-matching
    # so OC → Hermes file_transfer arrives parseable.
    if parsed.get("type") == "task_request" and isinstance(
        parsed.get("content"), str
    ):
        try:
            inner = _json.loads(parsed["content"])
        except (ValueError, TypeError):
            inner = None
        if isinstance(inner, dict) and inner.get("type") == "file_transfer":
            parsed = inner
    if parsed.get("type") != "file_transfer":
        return payload_text

    file_name = str(parsed.get("file_name") or "").strip()
    file_data_b64 = parsed.get("file_data")
    if not file_name or not isinstance(file_data_b64, str):
        logger.warning(
            "mesh_worker: malformed file_transfer envelope from %s "
            "(missing file_name/file_data)",
            msg.from_did,
        )
        return payload_text

    # Path-safety: only the basename — the sender does not control
    # where we drop files on disk. Strip any ../ or absolute prefix.
    safe_name = _Path(file_name).name
    if not safe_name or safe_name in {".", ".."}:
        logger.warning(
            "mesh_worker: rejecting file_transfer with unsafe file_name=%r",
            file_name,
        )
        return payload_text

    incoming_dir = _Path(
        os.environ.get("KARS_INCOMING_DIR", "/sandbox/incoming")
    )
    try:
        incoming_dir.mkdir(parents=True, exist_ok=True)
        file_bytes = _base64.b64decode(file_data_b64)
        out_path = incoming_dir / safe_name
        out_path.write_bytes(file_bytes)
        logger.info(
            "mesh_worker: saved file_transfer from %s → %s (%d bytes)",
            msg.from_did,
            out_path,
            len(file_bytes),
        )
    except (OSError, ValueError) as exc:
        logger.warning(
            "mesh_worker: failed to save file_transfer from %s: %s",
            msg.from_did,
            exc,
        )
        return payload_text

    # Fire-and-forget ack back to sender so the OpenClaw / Hermes
    # sender's `kars_mesh_transfer_file` retry loop sees the success
    # signal. Errors here are non-fatal — the file is saved either
    # way; the ack is operator-side bookkeeping.
    sender = parsed.get("from_agent") or ""
    if sender and sender != "unknown":
        ack = {
            "type": "file_transfer_ack",
            "file_name": safe_name,
            "saved_to": str(out_path),
            "size_bytes": len(file_bytes),
        }
        try:
            asyncio.create_task(
                client.send_by_name(
                    to=sender, payload=_json.dumps(ack).encode("utf-8")
                )
            )
        except Exception as exc:  # noqa: BLE001
            logger.debug(
                "mesh_worker: file_transfer_ack send scheduling failed: %s",
                exc,
            )

    # Hand the LLM a short human-readable summary, NOT the 30 MiB of
    # base64 the sender shipped. The agent sees the description +
    # absolute path and can pick the file up off disk.
    desc = str(parsed.get("description") or "").strip()
    summary = (
        f"file_transfer received from {parsed.get('from_agent','unknown')}: "
        f"{safe_name} ({len(file_bytes)} bytes) saved to {out_path}"
    )
    if desc:
        summary += f"\nSender description: {desc}"
    return summary


async def _handle_message(client: Any, msg: Any) -> None:
    payload_text = msg.payload.decode("utf-8", errors="replace")
    logger.info(
        "mesh_worker: handling inbound msg (from=%s bytes=%d payload[:200]=%r)",
        msg.from_did,
        len(msg.payload),
        payload_text[:200],
    )

    # ── file_transfer auto-decode ─────────────────────────────────
    # Detect file_transfer envelopes before invoking the LLM. Runs
    # UNCONDITIONALLY (no AUTO_RESPONDER gate) so a top-level Hermes
    # pod still saves files peers ship to it — matches OpenClaw,
    # whose always-on agent loop strips structural envelopes
    # regardless of any opt-in env.
    payload_text = _maybe_save_file_transfer(payload_text, msg, client)

    # ── Publish peer to router trust store (operator panel feed) ──
    # Without this, the operator's per-sandbox AGT view stays empty
    # even after a successful KNOCK + decrypted MESSAGE, because the
    # router only learns about peers from plugin pushes.
    #
    # Score convention: OpenClaw's TS plugin computes
    # `Math.round(500 + scoreDelta * 500)` for its trust pushes
    # (runtimes/openclaw/src/core/router-client.ts: pushTrustToRouter),
    # so `pushTrustToRouter(name, 0.0)` produces score=500 (router
    # baseline = at-threshold). The Python `submit_trust` helper
    # uses a different scaling: a 0.0-1.0 score multiplies to
    # 0-1000 directly. To match OpenClaw's "at-threshold-baseline"
    # convention on the FIRST interaction we send score=0.5
    # (=500 in scaled units), not score=0.0 (which would scale to 0
    # and trigger the router's anonymous-tier minimum of 10).
    try:
        from . import telemetry as _telemetry  # noqa: PLC0415

        sender_name_for_trust = await _resolve_sender_name(client, msg.from_did)
        _telemetry.submit_trust(
            agent_id=sender_name_for_trust or msg.from_did,
            score=0.5,
            interactions=1,
        )
    except Exception as exc:  # noqa: BLE001
        logger.debug("mesh_worker: trust publish failed (non-fatal): %s", exc)

    # LLM-spawning is opt-in via KARS_MESH_AUTO_RESPONDER. A top-level
    # (channel-driven) Hermes agent must NOT auto-spawn hermes -z on
    # every inbound or it would infinite-loop on its own replies. The
    # controller sets the env var on sub-agent containers (where the
    # parent expects a synchronous round-trip).
    auto_responder = os.environ.get(
        "KARS_MESH_AUTO_RESPONDER", "0"
    ) in {"1", "true", "True"}
    if not auto_responder:
        logger.info(
            "mesh_worker: inbox drained from %s (AUTO_RESPONDER off; "
            "structural envelopes saved, LLM response suppressed)",
            msg.from_did,
        )
        return

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
    # Launch the mesh inbox dispatcher (idempotent).
    #
    # Structural envelopes (file_transfer, future protocol acks)
    # are auto-saved by _handle_message regardless of
    # KARS_MESH_AUTO_RESPONDER -- infrastructure plumbing, not
    # LLM business. The LLM-spawning branch (hermes -z per
    # inbound) is gated by the env var inside _handle_message
    # (per-message check), so the structural path runs even when
    # the LLM path is suppressed.
    #
    # Without this split, a top-level Hermes agent (no
    # AUTO_RESPONDER) never saves inbound files because the
    # entire dispatcher was short-circuited -- discovered when
    # validating kars_mesh_transfer_file end-to-end on AKS where
    # the receiver was not a sub-agent.
    global _WORKER_TASK, _WORKER_LOOP

    if _WORKER_TASK is not None and not _WORKER_TASK.done():
        logger.debug("mesh_worker: dispatcher already running, skipping")
        return

    from . import mesh as _mesh  # noqa: PLC0415
    _WORKER_LOOP = _mesh._get_or_init_loop()  # noqa: SLF001
    _WORKER_TASK = asyncio.run_coroutine_threadsafe(
        _worker_loop(get_client), _WORKER_LOOP
    )  # type: ignore[assignment]
    auto = os.environ.get("KARS_MESH_AUTO_RESPONDER", "0") in {"1", "true", "True"}
    logger.info(
        "mesh_worker: dispatcher started (auto_responder=%s)",
        "on" if auto else "off (structural-envelope tap only)",
    )
