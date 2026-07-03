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
import json
import logging
import os
from datetime import datetime, timezone
from typing import Any

logger = logging.getLogger("kars.hermes.mesh_worker")


def _utc_now_iso() -> str:
    """RFC3339 UTC timestamp for task_response envelopes (matches OpenClaw)."""
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


# Interval between task_progress heartbeats sent to the controller while a
# delivered task runs. Must stay well under the controller's IDLE_TIMEOUT_SECS
# (180s, controller/src/mesh_peer/task_delivery.rs) or a long-running run is
# killed as "no progress heartbeat" — the original Hermes-mission failure mode.
_HEARTBEAT_INTERVAL_S = 20.0


async def _route_send(
    client: Any, msg: Any, sender_name: str | None, payload: bytes
) -> None:
    """Send a frame back to the originator using the correct transport.

    The kars controller is a PLAINTEXT control-plane peer (not registry-
    discoverable), so reply to it by DID (a plaintext frame it can decode),
    never by display-name lookup (which 404s). Real agent peers (a team
    principal delivering to a Hermes sub-agent) reply by friendly name over
    the established E2E secure channel, falling back to DID.
    """
    if client.is_plaintext_peer(msg.from_did):
        await client.send_by_did(to=msg.from_did, payload=payload)
    elif sender_name:
        await client.send_by_name(to=sender_name, payload=payload)
    else:
        await client.send_by_did(to=msg.from_did, payload=payload)


async def _heartbeat_loop(
    client: Any, msg: Any, sender_name: str | None, from_agent: str
) -> None:
    """Tick a task_progress heartbeat to the originator every ~20s.

    Runs concurrently with `hermes -z` for a delivered task so the
    originator's idle timeout (the controller's 180s, or a team principal's
    delivery wait) does not kill a run doing real work. Routed the same way
    as the terminal reply. Cancelled by the caller once the run produces its
    terminal reply.
    """
    tick = 0
    while True:
        await asyncio.sleep(_HEARTBEAT_INTERVAL_S)
        tick += 1
        frame = json.dumps(
            {
                "type": "task_progress",
                "stage": "executing",
                "tick": tick,
                "elapsed_seconds": int(tick * _HEARTBEAT_INTERVAL_S),
                "from_agent": from_agent,
                "timestamp": _utc_now_iso(),
            }
        ).encode("utf-8")
        try:
            await _route_send(client, msg, sender_name, frame)
            logger.debug(
                "mesh_worker: task_progress heartbeat #%d → %s",
                tick,
                sender_name or msg.from_did,
            )
        except Exception as exc:  # noqa: BLE001
            logger.debug("mesh_worker: heartbeat #%d send failed (non-fatal): %s", tick, exc)

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

    # ── Controller task-delivery protocol ────────────────────────────
    # The kars controller delivers work as a JSON envelope
    # {type:"task_request", content:<objective>, request_id:<id>} (see
    # controller/src/mesh_peer FederationMessage + runtimes/openclaw
    # index.ts onMessage). Extract the objective as the LLM prompt and
    # remember the request_id so the reply is a MATCHING task_response —
    # the exact shape the controller's task-delivery waiter parses
    # (base64(json(FederationMessage))). A non-task payload (peer chat)
    # passes through unchanged as the prompt and gets a raw reply.
    prompt_text = payload_text
    task_request_id: str | None = None
    _envelope_is_task = False
    try:
        _envelope = json.loads(payload_text)
    except (json.JSONDecodeError, ValueError):
        _envelope = None
    if isinstance(_envelope, dict) and _envelope.get("type") == "task_request":
        _envelope_is_task = True
        prompt_text = str(_envelope.get("content") or "")
        _rid = _envelope.get("request_id")
        task_request_id = str(_rid) if _rid is not None else None
        logger.info(
            "mesh_worker: parsed task_request (request_id=%s content[:120]=%r)",
            task_request_id,
            prompt_text[:120],
        )

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

    # LLM-spawning is opt-in via KARS_MESH_AUTO_RESPONDER for free-form peer
    # chat: a top-level (channel-driven) Hermes agent must NOT auto-spawn
    # hermes -z on every inbound or it would infinite-loop on its own replies.
    #
    # A delivered `task_request` is DIFFERENT: it is an explicit request to do
    # work — the controller delivering a mission to a principal, or a principal
    # delegating to a sub-agent — so it ALWAYS runs the LLM regardless of the
    # opt-in. This is safe against the loop the opt-in guards: a sub-agent's
    # reply is a `task_response` (not a `task_request`), so a principal receiving
    # it still falls through to the drained/inbox path below and never re-runs.
    # Without this, a Hermes MISSION principal (no parent label → no
    # AUTO_RESPONDER) drains the controller's task_request without executing it,
    # and the mission times out with "no progress heartbeat".
    auto_responder = os.environ.get(
        "KARS_MESH_AUTO_RESPONDER", "0"
    ) in {"1", "true", "True"}
    if not auto_responder and not _envelope_is_task:
        logger.info(
            "mesh_worker: inbox drained from %s (AUTO_RESPONDER off, not a "
            "task_request; structural envelopes saved, LLM response suppressed)",
            msg.from_did,
        )
        return

    # Cap the per-prompt timeout so a misbehaving inbound can't pin a
    # worker forever. 25 min matches the parent's typical patience for
    # a sub-agent doing real Foundry work (research + code + image).
    timeout_seconds = float(os.environ.get("KARS_MESH_WORKER_TIMEOUT_S", "1500"))
    # Resolve the friendly name once, up front, so both the heartbeat and the
    # terminal reply route to the originator identically.
    sender_name = await _resolve_sender_name(client, msg.from_did)
    from_agent = (
        os.environ.get("SANDBOX_NAME") or os.environ.get("HERMES_PROFILE") or ""
    )
    proc = await asyncio.create_subprocess_exec(
        *_hermes_cmd(prompt_text),
        env=_hermes_env(),
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    # Keep the originator's delivery alive while hermes -z runs. Heartbeat for
    # ANY delivered task (controller mission OR a team principal's sub-agent
    # task) so a long run isn't killed as "no progress heartbeat" — the
    # original Hermes-mission failure mode. Mirrors OpenClaw.
    hb_task: asyncio.Task[None] | None = None
    if _envelope_is_task:
        hb_task = asyncio.create_task(
            _heartbeat_loop(client, msg, sender_name, from_agent)
        )
    try:
        stdout_b, stderr_b = await asyncio.wait_for(
            proc.communicate(), timeout=timeout_seconds
        )
    except asyncio.TimeoutError:
        proc.kill()
        await proc.wait()
        reply = f"WORKER_TIMEOUT after {timeout_seconds:.0f}s"
        reply_ok = False
        logger.warning("mesh_worker: %s for inbound from %s", reply, msg.from_did)
    else:
        reply = stdout_b.decode("utf-8", errors="replace").strip()
        reply_ok = proc.returncode == 0 and bool(reply)
        if proc.returncode != 0:
            reply = (
                f"WORKER_ERROR rc={proc.returncode}\nstdout:\n{reply}"
                f"\nstderr:\n{stderr_b.decode(errors='replace').strip()}"
            )

    # Stop heartbeats now that the run has produced its terminal result.
    if hb_task is not None:
        hb_task.cancel()
        try:
            await hb_task
        except asyncio.CancelledError:
            pass

    # Wrap the reply for the delivery waiter (controller or team principal): it
    # parses base64(json(FederationMessage)) — a TaskResponse matched by the
    # sender DID — and reads content/ok (defaults true). A raw text reply is
    # dropped. When the inbound was a task_request, reply with a task_response
    # envelope mirroring OpenClaw's shape (content/ok/in_reply_to/from_agent);
    # otherwise (peer chat) send the raw text.
    if _envelope_is_task:
        reply_payload = json.dumps(
            {
                "type": "task_response",
                "content": reply,
                "ok": reply_ok,
                "in_reply_to": task_request_id or prompt_text[:256],
                "from_agent": from_agent,
                "timestamp": _utc_now_iso(),
            }
        ).encode("utf-8")
    else:
        reply_payload = reply.encode("utf-8")

    try:
        await _route_send(client, msg, sender_name, reply_payload)
        logger.info(
            "mesh_worker: replied %d bytes to %s (task_response=%s)",
            len(reply_payload),
            sender_name or msg.from_did,
            _envelope_is_task,
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
