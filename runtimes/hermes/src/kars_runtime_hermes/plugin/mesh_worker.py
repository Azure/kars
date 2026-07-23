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
import hashlib
import hmac
import json
import logging
import os
import stat
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

logger = logging.getLogger("kars.hermes.mesh_worker")


def _utc_now_iso() -> str:
    """RFC3339 UTC timestamp for task_response envelopes (matches OpenClaw)."""
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _read_checkpoint() -> dict[str, Any] | None:
    try:
        checkpoint_path = _artifact_root() / "task-checkpoint.json"
        if not checkpoint_path.exists():
            return None
        parsed = json.loads(checkpoint_path.read_text(encoding="utf-8"))
        if isinstance(parsed, dict) and parsed.get("schema") == "kars.checkpoint/v1":
            return parsed
    except (OSError, json.JSONDecodeError, ValueError):
        return None
    return None


# Interval between task_progress heartbeats sent to the controller while a
# delivered task runs. Must stay well under the controller's IDLE_TIMEOUT_SECS
# (180s, controller/src/mesh_peer/task_delivery.rs) or a long-running run is
# killed as "no progress heartbeat" — the original Hermes-mission failure mode.
_HEARTBEAT_INTERVAL_S = 20.0
_MAX_ARTIFACTS = 12
_MAX_ARTIFACT_SET_BYTES = 900 * 1024
_MAX_WORKSPACE_FILES = 1000
_MAX_WORKSPACE_DEPTH = 6
_ARTIFACT_SEND_TIMEOUT_S = 15.0
_ARTIFACT_TOTAL_TIMEOUT_S = 60.0
_TASK_EXECUTION_LOCK = asyncio.Lock()


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
        checkpoint = _read_checkpoint()
        frame = json.dumps(
            {
                "type": "task_progress",
                "stage": "executing",
                "tick": tick,
                "elapsed_seconds": int(tick * _HEARTBEAT_INTERVAL_S),
                "from_agent": from_agent,
                "timestamp": _utc_now_iso(),
                **({"checkpoint": checkpoint, "stage": "checkpoint"} if checkpoint else {}),
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


def _run_hermes_agent_inprocess(prompt: str) -> tuple[str, bool]:
    """Run the Hermes agent loop IN-PROCESS and return ``(output, ok)``.

    This executes inside the plugin-loaded process that owns the single
    per-pod ``MeshClient``, so the agent's ``kars_*`` tools (``kars_mesh_send``,
    ``kars_spawn``, ``kars_mesh_inbox``/``await``) reuse THIS process's client —
    the same single-process model OpenClaw uses. No ``hermes -z`` subprocess, no
    second ``MeshClient``, no prekey-lock contention, no ephemeral identity.

    ``agent.chat`` is synchronous (it drives its own tool-calling loop), so the
    caller must invoke this in an executor thread to keep the mesh event loop
    responsive while the agent works.
    """
    # Non-interactive posture. hermes_cli.oneshot.run_oneshot sets these before
    # calling _run_agent; we call _run_agent directly (run_oneshot also does a
    # process-global stdout/stderr redirect + logging.disable that is unsafe for
    # concurrent in-process use), so replicate just the env it relies on.
    os.environ.setdefault("HERMES_YOLO_MODE", "1")
    os.environ.setdefault("HERMES_ACCEPT_HOOKS", "1")
    os.environ.setdefault("HOME", "/sandbox")
    os.environ.setdefault("HERMES_HOME", "/sandbox/.hermes")
    try:
        from hermes_cli.oneshot import _run_agent  # noqa: PLC0415
    except Exception as exc:  # noqa: BLE001
        return f"WORKER_ERROR: hermes oneshot API unavailable: {exc}", False
    try:
        out = (_run_agent(prompt) or "").strip()
        return out, bool(out)
    except Exception as exc:  # noqa: BLE001
        logger.exception("mesh_worker: in-process hermes agent failed")
        return f"WORKER_ERROR: in-process agent failed: {exc}", False


def _telemetry_cursor() -> int:
    """Current router task-telemetry cursor, so we can capture only the events
    THIS run produces (the router is the honest source of token/round/tool
    usage — every model call flows through it)."""
    try:
        from . import router_client  # noqa: PLC0415

        data = router_client.call_json("GET", "/telemetry/cursor")
        return int(data.get("cursor", 0) or 0)
    except Exception:  # noqa: BLE001
        return 0


def _telemetry_since(cursor: int) -> list[dict[str, Any]]:
    """Router telemetry events recorded since `cursor` — the run's real
    per-round + per-tool trace (the same shape the Bridge renders as activity)."""
    try:
        from . import router_client  # noqa: PLC0415

        data = router_client.call_json(
            "GET", "/telemetry/trace", params={"since": cursor}
        )
        events = data.get("events") or []
        return events if isinstance(events, list) else []
    except Exception:  # noqa: BLE001
        return []


def _summarize_telemetry(
    events: list[dict[str, Any]],
) -> tuple[dict[str, int], list[dict[str, Any]]]:
    """Fold router telemetry events into (RunTelemetry dict, capped trace).

    A run that produced tokens/rounds is `did_work` on the controller side, so
    a real Hermes deliverable is scored as a substantive success (Healthy)
    instead of `barren` ('low yield'); the trace powers the Activity tab.
    """
    prompt = completion = total = rounds = tool_calls = 0
    for ev in events:
        if not isinstance(ev, dict):
            continue
        if ev.get("kind") == "round":
            prompt += int(ev.get("prompt_tokens", 0) or 0)
            completion += int(ev.get("completion_tokens", 0) or 0)
            total += int(ev.get("total_tokens", 0) or 0)
            rounds += 1
        elif ev.get("kind") == "tool":
            tool_calls += 1
    telemetry = {
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": total,
        "rounds": rounds,
        "tool_calls": tool_calls,
    }
    # Bound the trace carried on the wire (the router already caps per task).
    return telemetry, events[-400:]


def _artifact_root() -> Path:
    configured = os.environ.get("KARS_HERMES_ARTIFACT_DIR")
    if configured:
        return Path(configured)
    # Use the same durable workspace contract as OpenClaw so task authors and
    # generated objectives never need harness-specific output paths. The Hermes
    # image creates this writable directory; retain the historical runtime path
    # only as a compatibility fallback for older custom images.
    shared = Path("/sandbox/.openclaw/workspace")
    if shared.exists():
        return shared
    return Path("/sandbox/.hermes/artifacts")


def _prepare_task_contract(content: str) -> str:
    try:
        value = json.loads(content)
    except (json.JSONDecodeError, ValueError):
        return content
    if not isinstance(value, dict) or value.get("schema") != "kars.task/v1":
        return content
    objective = value.get("objective") if isinstance(value.get("objective"), str) else ""
    instructions = (
        value.get("instructions") if isinstance(value.get("instructions"), str) else ""
    )
    checkpoint_json = (
        value.get("checkpoint_json")
        if isinstance(value.get("checkpoint_json"), str)
        else ""
    )
    digest = value.get("digest") if isinstance(value.get("digest"), str) else ""

    def frame(field: str) -> str:
        return f"{len(field.encode('utf-8'))}:{field}"

    canonical = "".join(
        frame(field)
        for field in ("kars.task/v1", objective, instructions, checkpoint_json)
    )
    expected = hashlib.sha256(canonical.encode("utf-8")).hexdigest()
    if not objective.strip() or not hmac.compare_digest(digest, expected):
        raise ValueError(
            "Invalid kars.task/v1 execution contract: objective missing or digest mismatch"
        )
    root = _artifact_root()
    root.mkdir(parents=True, exist_ok=True)
    target = root / "execution-contract.json"
    staging = root / "execution-contract.json.tmp"
    staging.write_text(json.dumps(value, indent=2), encoding="utf-8")
    os.chmod(staging, 0o600)
    staging.replace(target)
    prompt = (
        f"Execution contract: kars.task/v1 ({digest})\n"
        f"Verified contract persisted at {target}.\n\nObjective:\n{objective}"
    )
    if instructions.strip():
        prompt += f"\n\nStanding instructions:\n{instructions}"
    if checkpoint_json.strip():
        prompt += (
            "\n\nResume checkpoint (continue from this state; do not repeat completed "
            f"milestones):\n{checkpoint_json}"
        )
    return prompt


def _open_workspace_root() -> int:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    return os.open(_artifact_root(), flags)


def _walk_workspace(root_fd: int) -> list[tuple[str, int, int]]:
    """Return bounded regular-file metadata without following symlinks."""
    files: list[tuple[str, int, int]] = []
    for dirpath, dirnames, filenames, dir_fd in os.fwalk(
        ".",
        topdown=True,
        follow_symlinks=False,
        dir_fd=root_fd,
    ):
        depth = 0 if dirpath == "." else len(Path(dirpath).parts)
        if depth >= _MAX_WORKSPACE_DEPTH:
            dirnames[:] = []
        for name in filenames:
            try:
                info = os.stat(name, dir_fd=dir_fd, follow_symlinks=False)
            except OSError:
                continue
            if not stat.S_ISREG(info.st_mode):
                continue
            rel = name if dirpath == "." else f"{dirpath.removeprefix('./')}/{name}"
            files.append((rel, info.st_mtime_ns, info.st_size))
            if len(files) >= _MAX_WORKSPACE_FILES:
                return files
    return files


def _open_workspace_file(root_fd: int, rel: str) -> int:
    """Open a relative regular file while rejecting symlinks in every component."""
    parts = Path(rel).parts
    if not parts or any(part in {"", ".", ".."} for part in parts):
        raise OSError("unsafe artifact path")
    current_fd = os.dup(root_fd)
    try:
        for component in parts[:-1]:
            flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
            if hasattr(os, "O_NOFOLLOW"):
                flags |= os.O_NOFOLLOW
            next_fd = os.open(component, flags, dir_fd=current_fd)
            os.close(current_fd)
            current_fd = next_fd
        flags = os.O_RDONLY
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        return os.open(parts[-1], flags, dir_fd=current_fd)
    finally:
        os.close(current_fd)


def _artifact_wire_name(rel: str) -> str:
    digest = hashlib.sha256(rel.encode("utf-8")).hexdigest()[:10]
    safe = "".join(c if (c.isascii() and c.isalnum()) or c in "._-" else "_" for c in rel)
    stem, dot, suffix = safe.rpartition(".")
    if dot and stem:
        return f"{stem[:150]}-{digest}.{suffix[:20]}"
    return f"{safe[:160]}-{digest}"


def _snapshot_workspace() -> dict[str, tuple[int, int]]:
    """Record regular workspace files before a task so only its changes ship."""
    try:
        root_fd = _open_workspace_root()
    except OSError:
        return {}
    try:
        return {rel: (mtime, size) for rel, mtime, size in _walk_workspace(root_fd)}
    finally:
        os.close(root_fd)


def _read_changed_artifacts(
    before: dict[str, tuple[int, int]],
    reply: str,
    reply_ok: bool,
    request_id: str,
) -> list[tuple[str, str, bytes]]:
    """Read bounded new/modified workspace files after a task completes."""
    root = _artifact_root()
    changed: list[tuple[str, str, bytes]] = []
    try:
        root.mkdir(parents=True, exist_ok=True)
        root_fd = _open_workspace_root()
    except OSError:
        return changed

    try:
        candidates = [
            (mtime, rel, size)
            for rel, mtime, size in _walk_workspace(root_fd)
            if before.get(rel) != (mtime, size)
        ]

        structured_json = False
        json_payload = False
        if reply.strip():
            try:
                parsed_reply = json.loads(reply)
                json_payload = True
                if isinstance(parsed_reply, dict):
                    acknowledgement_keys = {"ok", "status", "success", "message"}
                    structured_json = bool(parsed_reply) and not (
                        set(parsed_reply).issubset(acknowledgement_keys)
                    )
                elif isinstance(parsed_reply, list):
                    structured_json = bool(parsed_reply)
            except (json.JSONDecodeError, TypeError):
                pass
        if not candidates and reply_ok and (
            structured_json or (not json_payload and len(reply) > 400)
        ):
            safe_id = "".join(c for c in request_id if c.isascii() and c.isalnum())[:8] or "task"
            suffix = "json" if structured_json else "md"
            rel = f"task-{safe_id}-report.{suffix}"
            flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
            if hasattr(os, "O_NOFOLLOW"):
                flags |= os.O_NOFOLLOW
            fd = os.open(rel, flags, 0o600, dir_fd=root_fd)
            try:
                data = reply.encode("utf-8")
                remaining = memoryview(data)
                while remaining:
                    written = os.write(fd, remaining)
                    remaining = remaining[written:]
                info = os.fstat(fd)
            finally:
                os.close(fd)
            candidates.append((info.st_mtime_ns, rel, info.st_size))

        total = 0
        for _, rel, size in sorted(candidates, reverse=True):
            if len(changed) >= _MAX_ARTIFACTS:
                break
            if size > _MAX_ARTIFACT_SET_BYTES or total + size > _MAX_ARTIFACT_SET_BYTES:
                continue
            try:
                fd = _open_workspace_file(root_fd, rel)
                try:
                    info = os.fstat(fd)
                    if not stat.S_ISREG(info.st_mode) or info.st_size != size:
                        continue
                    with os.fdopen(fd, "rb", closefd=False) as file:
                        data = file.read(size + 1)
                finally:
                    os.close(fd)
            except OSError as exc:
                logger.warning("mesh_worker: failed to read artifact %s: %s", rel, exc)
                continue
            if len(data) != size:
                continue
            changed.append((_artifact_wire_name(rel), rel, data))
            total += size
        return changed
    except OSError as exc:
        logger.warning("mesh_worker: artifact collection failed: %s", exc)
        return []
    finally:
        os.close(root_fd)


async def _collect_and_ship_artifacts(
    client: Any,
    msg: Any,
    sender_name: str | None,
    before: dict[str, tuple[int, int]],
    reply: str,
    reply_ok: bool,
    request_id: str,
    from_agent: str,
) -> list[dict[str, Any]]:
    """Ship task-created files before the matching task_response."""
    loop = asyncio.get_running_loop()
    artifacts = await loop.run_in_executor(
        None,
        _read_changed_artifacts,
        before,
        reply,
        reply_ok,
        request_id,
    )
    manifest: list[dict[str, Any]] = []
    try:
        async with asyncio.timeout(_ARTIFACT_TOTAL_TIMEOUT_S):
            for name, rel, data in artifacts:
                frame = json.dumps(
                    {
                        "type": "file_transfer",
                        "file_name": name,
                        "file_path": rel,
                        "file_data": base64.b64encode(data).decode("ascii"),
                        "size_bytes": len(data),
                        "from_agent": from_agent,
                        "timestamp": _utc_now_iso(),
                    }
                ).encode("utf-8")
                try:
                    await asyncio.wait_for(
                        _route_send(client, msg, sender_name, frame),
                        timeout=_ARTIFACT_SEND_TIMEOUT_S,
                    )
                except Exception as exc:  # noqa: BLE001
                    logger.warning("mesh_worker: failed to ship artifact %s: %s", rel, exc)
                    continue
                manifest.append({"name": name, "path": rel, "size_bytes": len(data)})
                logger.info("mesh_worker: shipped artifact %s (%d bytes)", rel, len(data))
    except TimeoutError:
        logger.warning(
            "mesh_worker: artifact delivery exceeded %.0fs; sending partial manifest",
            _ARTIFACT_TOTAL_TIMEOUT_S,
        )
    return manifest


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


async def _execute_task_request(
    client: Any,
    msg: Any,
    prompt_text: str,
    task_request_id: str | None,
) -> None:
    """Run one task and keep artifact harvesting ordered with its response."""
    timeout_seconds = float(os.environ.get("KARS_MESH_WORKER_TIMEOUT_S", "1500"))
    sender_name = await _resolve_sender_name(client, msg.from_did)
    from_agent = os.environ.get("SANDBOX_NAME") or os.environ.get("HERMES_PROFILE") or ""
    hb_task = asyncio.create_task(_heartbeat_loop(client, msg, sender_name, from_agent))
    loop = asyncio.get_running_loop()
    tel_cursor = await loop.run_in_executor(None, _telemetry_cursor)
    artifact_snapshot = await loop.run_in_executor(None, _snapshot_workspace)
    delivery_prompt = (
        f"{prompt_text}\n\n"
        "Kars handback protocol: complete the assignment in this response. "
        "Do not call kars_mesh_send to 'parent' or to the assigning agent for the "
        "final handback; the mesh worker automatically returns your final response "
        "as the correlated task_response. Use mesh tools only for deliberate peer "
        "collaboration required by the assignment."
    )
    agent_future = loop.run_in_executor(None, _run_hermes_agent_inprocess, delivery_prompt)
    timed_out = False

    try:
        try:
            reply, reply_ok = await asyncio.wait_for(
                asyncio.shield(agent_future),
                timeout=timeout_seconds,
            )
        except asyncio.TimeoutError:
            reply = f"WORKER_TIMEOUT after {timeout_seconds:.0f}s"
            reply_ok = False
            timed_out = True
            logger.warning("mesh_worker: %s for inbound from %s", reply, msg.from_did)

        telemetry, trace = _summarize_telemetry(
            await loop.run_in_executor(None, _telemetry_since, tel_cursor)
        )
        if timed_out:
            artifacts = []
        else:
            try:
                artifacts = await _collect_and_ship_artifacts(
                    client,
                    msg,
                    sender_name,
                    artifact_snapshot,
                    reply,
                    reply_ok,
                    task_request_id or "task",
                    from_agent,
                )
            except Exception as exc:  # noqa: BLE001
                logger.warning("mesh_worker: artifact delivery failed (continuing): %s", exc)
                artifacts = []

        final_checkpoint = _read_checkpoint()
        if final_checkpoint is not None:
            checkpoint_frame = json.dumps(
                {
                    "type": "task_progress",
                    "stage": "checkpoint",
                    "task_id": task_request_id,
                    "checkpoint": final_checkpoint,
                    "from_agent": from_agent,
                    "timestamp": _utc_now_iso(),
                }
            ).encode("utf-8")
            try:
                await _route_send(client, msg, sender_name, checkpoint_frame)
            except Exception as exc:  # noqa: BLE001
                logger.warning("mesh_worker: final checkpoint send failed: %s", exc)

        reply_payload = json.dumps(
            {
                "type": "task_response",
                "content": reply,
                "ok": reply_ok,
                "in_reply_to": task_request_id or prompt_text[:256],
                "from_agent": from_agent,
                "artifacts": artifacts,
                "telemetry": telemetry,
                "trace": trace,
                "timestamp": _utc_now_iso(),
            }
        ).encode("utf-8")
        try:
            await _route_send(client, msg, sender_name, reply_payload)
            logger.info(
                "mesh_worker: replied %d bytes to %s (task_response=true)",
                len(reply_payload),
                sender_name or msg.from_did,
            )
        except Exception as exc:  # noqa: BLE001
            logger.warning(
                "mesh_worker: failed to send reply to %s: %s",
                sender_name or msg.from_did,
                exc,
            )
    finally:
        hb_task.cancel()
        try:
            await hb_task
        except asyncio.CancelledError:
            pass
        if not agent_future.done():
            # wait_for cannot terminate a running executor thread. Keep the
            # workspace transaction locked until Hermes actually returns so its
            # late file writes and telemetry cannot contaminate a later task.
            try:
                await asyncio.shield(agent_future)
            except Exception as exc:  # noqa: BLE001
                logger.warning("mesh_worker: timed-out agent exited with error: %s", exc)


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
        _rid = _envelope.get("request_id") or _envelope.get("message_id")
        task_request_id = str(_rid) if _rid is not None else None
        logger.info(
            "mesh_worker: parsed task_request (request_id=%s content[:120]=%r)",
            task_request_id,
            prompt_text[:120],
        )
        try:
            prompt_text = _prepare_task_contract(prompt_text)
        except ValueError as error:
            sender_name = await _resolve_sender_name(client, msg.from_did)
            await _route_send(
                client,
                msg,
                sender_name,
                json.dumps(
                    {
                        "type": "task_response",
                        "in_reply_to_id": task_request_id,
                        "content": str(error),
                        "ok": False,
                        "from_agent": os.environ.get("SANDBOX_NAME", "hermes"),
                        "timestamp": _utc_now_iso(),
                    }
                ).encode("utf-8"),
            )
            return

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

    # The mesh worker's job is task delivery: a `task_request` (controller→
    # principal, or principal→sub-agent) is executed via the in-process agent.
    # EVERY other inbound (a sub-agent's `task_response` reply, a `task_progress`
    # tick, free-form peer chat) is NOT executed — it is buffered to the tool
    # inbox so the in-process agent's kars_mesh_inbox / kars_mesh_await tools can
    # read it. This single-owner fan-out mirrors OpenClaw's onMessage→mesh_inbox
    # buffer: the worker is the sole `_inbox` consumer and never lets a reply the
    # delegating agent is awaiting get dropped, and never re-executes a reply
    # (which would loop). Channel chat reaches a Hermes agent through the
    # gateway's own pipeline, not this mesh worker, so there is nothing else to
    # auto-run here.
    if not _envelope_is_task:
        try:
            client._tool_inbox.put_nowait(msg)  # noqa: SLF001 — internal but stable
        except Exception as exc:  # noqa: BLE001
            logger.debug("mesh_worker: tool-inbox buffer failed (non-fatal): %s", exc)
        logger.info(
            "mesh_worker: buffered inbound from %s to tool inbox "
            "(not a task_request; for kars_mesh_inbox/await)",
            msg.from_did,
        )
        return

    # Hermes' in-process agent and workspace are process-global. Never queue a
    # second task behind a long run: the sender's idle timer could expire and
    # retry it, producing duplicate execution. Return an honest terminal busy
    # response instead; the caller can explicitly redrive later.
    if _TASK_EXECUTION_LOCK.locked():
        sender_name = await _resolve_sender_name(client, msg.from_did)
        from_agent = os.environ.get("SANDBOX_NAME") or os.environ.get("HERMES_PROFILE") or ""
        busy_payload = json.dumps(
            {
                "type": "task_response",
                "content": "WORKER_BUSY: Hermes is already executing another task",
                "ok": False,
                "in_reply_to": task_request_id or prompt_text[:256],
                "from_agent": from_agent,
                "artifacts": [],
                "telemetry": None,
                "trace": [],
                "timestamp": _utc_now_iso(),
            }
        ).encode("utf-8")
        await _route_send(client, msg, sender_name, busy_payload)
        return

    # Serialize the run, harvest, file transfers, and task_response as one
    # ordered transaction so files and telemetry cannot cross task boundaries.
    async with _TASK_EXECUTION_LOCK:
        await _execute_task_request(client, msg, prompt_text, task_request_id)


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
