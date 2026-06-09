"""kars_spawn family — Phase A1.5.

Spawn / status / destroy / list sub-agents via the inference-router's
``/sandbox/*`` endpoints. Works identically on all three platforms —
router dispatches to the K8s API on AKS/local-k8s and to the Docker
Engine API in docker dev.

Mirror of OpenClaw's ``runtimes/openclaw/src/core/agt-tools/agt.ts``
spawn family. Trust seeding from parent + siblings happens in Act 2
once the Python AGT MeshClient lands; Act 1 spawns work without that
(child can be spawned, just can't task-delegate via E2E mesh until
Act 2).
"""

from __future__ import annotations

import json
import logging
import os
import re
import time
from typing import Any

from . import router_client

logger = logging.getLogger("kars.hermes.spawn")

# Process-local peer roster: name → role. Populated by `_kars_spawn`
# when the LLM passes a `role` arg, drained by `mesh.peer_roster()` to
# auto-prepend a `Peer roster:` block to outbound kars_mesh_send /
# kars_mesh_transfer_file. Mirrors OpenClaw's `spawnedRoster` Map in
# `runtimes/openclaw/src/core/agt-tools/agt.ts:545`.
#
# Module-level so a single Hermes daemon process sees the same roster
# across every plugin context that imports this module.
_SPAWNED_ROSTER: dict[str, str] = {}


def get_roster() -> dict[str, str]:
    """Return a defensive copy of the spawn roster. Used by
    ``mesh.py::_kars_mesh_send`` to compute the per-message
    `Peer roster:` prefix without sharing mutable state across
    plugin modules."""
    return dict(_SPAWNED_ROSTER)


def _record_in_roster(name: str, role: str | None) -> None:
    """Idempotent upsert. Empty / whitespace role is recorded as ''."""
    _SPAWNED_ROSTER[name] = (role or "").strip()


def _remove_from_roster(name: str) -> None:
    """Drop a name from the roster — called from kars_spawn_destroy so
    a sibling that was torn down doesn't leak into future roster
    headers."""
    _SPAWNED_ROSTER.pop(name, None)

# DNS-label rules — same as K8s metadata.name constraint
_NAME_RE = re.compile(r"^[a-z][a-z0-9-]*[a-z0-9]$|^[a-z]$")
_NAME_MAX_LEN = 63


def _validate_name(name: str) -> str | None:
    if not name:
        return "name is required"
    if len(name) > _NAME_MAX_LEN:
        return f"name must be ≤{_NAME_MAX_LEN} chars (k8s metadata.name limit)"
    if not _NAME_RE.match(name):
        return (
            "name must be DNS-label (lowercase alphanumeric + hyphens, "
            "must start with a letter and end alphanumeric)"
        )
    return None


def _kars_spawn(args: dict[str, Any], **_kwargs: Any) -> str:
    """Spawn a sub-agent. Returns the spawn result as a JSON string."""
    name = str(args.get("name", "")).strip()
    if err := _validate_name(name):
        return json.dumps({"error": err})

    body: dict[str, Any] = {
        "agent_id": name,
        "governance": args.get("governance", True),
        "trust_threshold": 500,
    }
    if model := args.get("model"):
        body["model"] = str(model)
    # Optional runtime override — when unset the child inherits the
    # parent's runtime (controller / docker spawn reads PARENT_RUNTIME_KIND
    # and picks the matching image).
    if runtime := args.get("runtime"):
        body["runtime_kind"] = str(runtime)
    role = args.get("role")
    if role:
        body["role"] = str(role)
    # KARS_DEV_PROFILE → relaxed sub-agent defaults
    if os.environ.get("KARS_DEV_PROFILE") == "true":
        body["learn_egress"] = True

    try:
        result = router_client.call_json("POST", "/sandbox/spawn", json=body)
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"spawn failed: {exc}"})

    # Track the freshly spawned sibling in the process-local roster
    # so kars_mesh_send can prepend a `Peer roster:` block to subsequent
    # outbound tasks. Recorded BEFORE the status poll so a "Pending"
    # child still appears in the roster (the parent's next mesh_send
    # is what gives the child its task).
    _record_in_roster(name, str(role) if role else None)

    # Poll status until Running or 45s timeout. Phase 'Pending' or
    # 'Provisioning' is normal during pod startup.
    deadline = time.monotonic() + 45.0
    phase = "Pending"
    while time.monotonic() < deadline:
        time.sleep(1.0)
        try:
            status_resp = router_client.call("GET", f"/sandbox/{name}/status")
            if status_resp.status_code == 404:
                continue
            status_resp.raise_for_status()
            status = status_resp.json()
            phase = status.get("phase", "Pending")
            if phase == "Running":
                break
        except Exception:  # noqa: BLE001 — keep polling
            continue

    out: dict[str, Any] = {**result, "phase": phase}
    if phase != "Running":
        out["warning"] = (
            f"Sub-agent '{name}' created but not yet Running (phase: {phase}). "
            "Use kars_spawn_status to check."
        )
    else:
        out["message"] = (
            f"Sub-agent '{name}' is Running. NOTE: kars_mesh_* tools are not "
            "available in Hermes v0.5.2 (Act 2 ships the Python AGT MeshClient). "
            "Coordinate with the sub-agent via shared Foundry Memory Store or "
            "Foundry Conversations until then."
        )
    return json.dumps(out)


def _kars_spawn_status(args: dict[str, Any], **_kwargs: Any) -> str:
    name = str(args.get("name", "")).strip()
    if err := _validate_name(name):
        return json.dumps({"error": err})
    try:
        resp = router_client.call("GET", f"/sandbox/{name}/status")
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"status check failed: {exc}"})

    if resp.status_code == 404:
        return json.dumps({"error": f"sub-agent '{name}' not found"})
    if resp.status_code >= 400:
        return json.dumps({"error": f"HTTP {resp.status_code}: {resp.text[:200]}"})

    try:
        result = resp.json()
    except Exception:  # noqa: BLE001
        return json.dumps({"error": "non-JSON status response"})

    return json.dumps(result)


def _kars_spawn_destroy(args: dict[str, Any], **_kwargs: Any) -> str:
    name = str(args.get("name", "")).strip()
    if err := _validate_name(name):
        return json.dumps({"error": err})
    try:
        resp = router_client.call("DELETE", f"/sandbox/{name}")
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"destroy failed: {exc}"})

    # Drop the torn-down sibling from the roster so the next outbound
    # kars_mesh_send doesn't list it as a reachable peer.
    _remove_from_roster(name)

    if resp.status_code == 404:
        return json.dumps({"warning": f"sub-agent '{name}' was already gone"})
    if resp.status_code >= 400:
        return json.dumps({"error": f"HTTP {resp.status_code}: {resp.text[:200]}"})

    # Best-effort cleanup of router-side trust entry; failure is non-fatal.
    try:
        router_client.call("DELETE", f"/agt/trust/{name}")
    except Exception:  # noqa: BLE001
        pass

    return json.dumps({"deleted": name, "message": f"Sub-agent '{name}' destroyed."})


def _kars_spawn_list(_args: dict[str, Any], **_kwargs: Any) -> str:
    try:
        result = router_client.call_json("GET", "/sandbox/list")
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"error": f"list failed: {exc}"})
    return json.dumps(result)


# ── Tool schemas (what the LLM sees) ────────────────────────────────


_SPAWN_SCHEMA = {
    "name": "kars_spawn",
    "description": (
        "Spawn a secure isolated sub-agent. The sub-agent runs in its own "
        "container with a SEPARATE filesystem — it CANNOT see your files. "
        "Pass `role` describing the sub-agent's persona (e.g. 'data analyst', "
        "'technical writer') so siblings can find it by role.\n\n"
        "NOTE: Inter-agent E2E messaging (kars_mesh_*) is not available "
        "in Hermes v0.5.2; the sub-agent will be running but cannot be "
        "task-delegated via mesh. Use Foundry Memory Store or Foundry "
        "Conversations to share data until Hermes v0.5.3."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "DNS-safe name (lowercase alphanumeric + hyphens, ≤63 chars)",
            },
            "model": {
                "type": "string",
                "description": "Optional model override; defaults to parent's model",
            },
            "role": {
                "type": "string",
                "description": "Short persona/role description",
            },
            "runtime": {
                "type": "string",
                "description": "Optional runtime override; defaults to parent's runtime kind",
            },
            "governance": {
                "type": "boolean",
                "description": "Enable AGT governance on the sub-agent (default: true)",
            },
        },
        "required": ["name"],
    },
}

_STATUS_SCHEMA = {
    "name": "kars_spawn_status",
    "description": "Check status of a spawned sub-agent. Returns phase (Pending/Running/Failed/Terminating).",
    "parameters": {
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "Sub-agent name"},
        },
        "required": ["name"],
    },
}

_DESTROY_SCHEMA = {
    "name": "kars_spawn_destroy",
    "description": "Destroy a sub-agent. Deletes its pod/container and revokes its trust entry.",
    "parameters": {
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "Sub-agent name to destroy"},
        },
        "required": ["name"],
    },
}

_LIST_SCHEMA = {
    "name": "kars_spawn_list",
    "description": "List all sub-agents spawned by this agent.",
    "parameters": {"type": "object", "properties": {}, "required": []},
}


def register(ctx: Any) -> None:  # noqa: ANN401
    """Register the spawn family with Hermes."""
    ctx.register_tool(
        name="kars_spawn",
        toolset="kars_spawn",
        schema=_SPAWN_SCHEMA,
        handler=_kars_spawn,
        description=_SPAWN_SCHEMA["description"],
    )
    ctx.register_tool(
        name="kars_spawn_status",
        toolset="kars_spawn",
        schema=_STATUS_SCHEMA,
        handler=_kars_spawn_status,
        description=_STATUS_SCHEMA["description"],
    )
    ctx.register_tool(
        name="kars_spawn_destroy",
        toolset="kars_spawn",
        schema=_DESTROY_SCHEMA,
        handler=_kars_spawn_destroy,
        description=_DESTROY_SCHEMA["description"],
    )
    ctx.register_tool(
        name="kars_spawn_list",
        toolset="kars_spawn",
        schema=_LIST_SCHEMA,
        handler=_kars_spawn_list,
        description=_LIST_SCHEMA["description"],
    )
    logger.info("kars_spawn family registered (4 tools)")

