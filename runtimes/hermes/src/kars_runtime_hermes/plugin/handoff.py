"""kars_handoff_* — agent migration / escalation tools.

Thin Python port of `runtimes/openclaw/src/core/agt-tools/agt.ts`
(kars_handoff_status / kars_handoff_request / kars_handoff_confirm).
The actual handoff state machine lives in the inference router under
`/agt/handoff/*` — this plugin just forwards the agent's tool calls
to those endpoints and returns the JSON result. Mirrors the OpenClaw
shape so an LLM trained on either runtime can drive it.

Status is ALWAYS registered. Request + Confirm are gated by
`AGT_REGISTRY_MODE=global` (same gate OpenClaw uses) because they
only do useful work when the AGT registry is in cross-cluster
(global) mode; in local mode the router refuses with 409.
"""

from __future__ import annotations

import json
import logging
import os
from typing import Any

from . import router_client

logger = logging.getLogger("kars.hermes.handoff")


# ── Schema definitions ──────────────────────────────────────────────

_STATUS_SCHEMA: dict[str, Any] = {
    "type": "object",
    "description": (
        "Check handoff (live migration) progress. Returns the current phase "
        "and steps. Pass since_step (total_steps from your last call) to get "
        "only new updates. Relay each new step to the user immediately as a "
        "live update."
    ),
    "properties": {
        "since_step": {
            "type": "number",
            "description": (
                "Number of steps you already received. Pass total_steps from "
                "your last handoff_status call. Omit or pass 0 on first call."
            ),
        },
    },
    "required": [],
}


_REQUEST_SCHEMA: dict[str, Any] = {
    "type": "object",
    "description": (
        "Request a live handoff (migration) of this agent. Creates a PENDING "
        "request with a confirmation code that is sent DIRECTLY to the user "
        "via their channel (you will NOT receive the code). The user must "
        "type the code back to you, and you pass it to kars_handoff_confirm. "
        "Do NOT fabricate or guess the confirmation code. Direction: 'cloud' "
        "(local→AKS) or 'local' (AKS→local). Always call kars_handoff_status "
        "first to check if handoff is available."
    ),
    "properties": {
        "direction": {
            "type": "string",
            "description": (
                "Migration direction: 'cloud' (local→AKS) or 'local' (AKS→local)"
            ),
        },
        "reason": {
            "type": "string",
            "description": (
                "Why the handoff is requested (shown in audit log + displayed "
                "to user)"
            ),
        },
    },
    "required": ["direction"],
}


_CONFIRM_SCHEMA: dict[str, Any] = {
    "type": "object",
    "description": (
        "Confirm a pending handoff request using the confirmation code that "
        "the USER typed. You do NOT have the code — it was sent directly to "
        "the user via their channel. You MUST wait for the user to type it. "
        "After confirming, poll kars_handoff_status every 3-5 seconds and "
        "relay each new step to the user as a real-time progress update."
    ),
    "properties": {
        "confirmation_token": {
            "type": "string",
            "description": "The confirmation code the user replied with",
        },
    },
    "required": ["confirmation_token"],
}


# ── Tool handlers ───────────────────────────────────────────────────


def _kars_handoff_status(args: dict[str, Any], **_kwargs: Any) -> str:
    """Forward to the router's `/agt/handoff/status`. The router owns the
    actual state machine; we just return whatever it says verbatim so
    the LLM can poll it and relay updates."""
    try:
        params = {}
        since = args.get("since_step")
        if isinstance(since, (int, float)) and since > 0:
            params["since_step"] = int(since)
        resp = router_client.call(
            "GET",
            "/agt/handoff/status",
            params=params or None,
        )
        if resp.status_code == 404:
            # Router has no handoff state — that's the steady-state for a
            # sandbox that has never been migrated. Mirror the OpenClaw
            # "handoff_available: false" return shape so the LLM can
            # branch on a single conventional key.
            return json.dumps(
                {
                    "handoff_available": False,
                    "active": False,
                    "phase": "none",
                    "status": "idle",
                }
            )
        return resp.text or json.dumps(
            {"handoff_available": False, "error": "empty router response"}
        )
    except Exception as exc:  # noqa: BLE001
        return json.dumps(
            {"handoff_available": False, "error": str(exc)}
        )


def _kars_handoff_request(args: dict[str, Any], **_kwargs: Any) -> str:
    """Forward to the router's `/agt/handoff/request`. Direction is
    normalised to the router's canonical names (`local_to_aks` /
    `aks_to_local`) matching OpenClaw's plugin behaviour."""
    direction_raw = str(args.get("direction", "")).strip().lower()
    if direction_raw in ("cloud", "local_to_aks", "to_aks", "aks"):
        direction = "local_to_aks"
    elif direction_raw in ("local", "aks_to_local", "to_local"):
        direction = "aks_to_local"
    else:
        return json.dumps(
            {
                "status": "error",
                "error": (
                    f"unknown direction: {direction_raw!r} "
                    "(use 'cloud' or 'local')"
                ),
            }
        )
    reason = str(args.get("reason") or "user_requested")
    try:
        resp = router_client.call(
            "POST",
            "/agt/handoff/request",
            json={"direction": direction, "reason": reason},
        )
        return resp.text or json.dumps(
            {"status": "error", "error": "empty router response"}
        )
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"status": "error", "error": str(exc)})


def _kars_handoff_confirm(args: dict[str, Any], **_kwargs: Any) -> str:
    """Forward to the router's `/agt/handoff/confirm`. The router takes
    over orchestration from here; the LLM polls kars_handoff_status to
    relay step-by-step progress to the user."""
    token = str(args.get("confirmation_token") or "").strip()
    if not token:
        return json.dumps(
            {
                "status": "error",
                "error": "missing required arg: confirmation_token",
            }
        )
    try:
        resp = router_client.call(
            "POST",
            "/agt/handoff/confirm",
            json={"confirmation_token": token},
        )
        return resp.text or json.dumps(
            {"status": "error", "error": "empty router response"}
        )
    except Exception as exc:  # noqa: BLE001
        return json.dumps({"status": "error", "error": str(exc)})


# ── Plugin registration ─────────────────────────────────────────────


def register(ctx: Any) -> None:  # noqa: ANN401
    """Register the kars_handoff_* family.

    `kars_handoff_status` is always registered (it returns
    `handoff_available: false` in local mode, which is a useful signal
    the LLM can branch on).

    `kars_handoff_request` / `kars_handoff_confirm` are gated by
    `AGT_REGISTRY_MODE=global` (same gate the OpenClaw plugin uses) —
    in local-only mode the router refuses mutations with 409, so we
    don't expose tools that would always fail.
    """
    ctx.register_tool(
        name="kars_handoff_status",
        toolset="kars_handoff",
        schema=_STATUS_SCHEMA,
        handler=_kars_handoff_status,
        description=_STATUS_SCHEMA["description"],
    )

    registry_mode = os.environ.get("AGT_REGISTRY_MODE", "local").strip().lower()
    if registry_mode == "global":
        ctx.register_tool(
            name="kars_handoff_request",
            toolset="kars_handoff",
            schema=_REQUEST_SCHEMA,
            handler=_kars_handoff_request,
            description=_REQUEST_SCHEMA["description"],
        )
        ctx.register_tool(
            name="kars_handoff_confirm",
            toolset="kars_handoff",
            schema=_CONFIRM_SCHEMA,
            handler=_kars_handoff_confirm,
            description=_CONFIRM_SCHEMA["description"],
        )
        logger.info(
            "kars_handoff_* family registered (3 tools: status, request, confirm)"
        )
    else:
        logger.info(
            "kars_handoff_status registered (1 tool); request/confirm "
            "skipped — AGT_REGISTRY_MODE=%s (set to 'global' to enable mutations)",
            registry_mode,
        )
