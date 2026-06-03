"""Trust + signing-counter telemetry pushes — Phase A1.10.

After successful peer interactions: push trust update to the router's
``/agt/trust`` endpoint. On Ed25519 sign/verify/reject: push counter to
``/agt/signing-counter``. Both already runtime-agnostic on the router
side (see ``inference-router/src/routes/governance.rs::agt_trust_update``
+ ``agt_signing_counter``).

Mirror of OpenClaw's pushTrustToRouter + signing-counter wiring at
``runtimes/openclaw/src/index.ts:1070,2155-2200`` (approximate).

Act 1 note: most trust + signing events fire from the mesh path which
is stubbed in Act 1. This module ships the wiring so Act 2's
MeshClient can hook into it without further work, and exposes a
``submit_trust()`` helper that other plugin modules (handoff,
spawn-destroy) call now.
"""

from __future__ import annotations

import logging
from typing import Any

from . import router_client

logger = logging.getLogger("kars.hermes.telemetry")


def submit_trust(agent_id: str, score: float, interactions: int = 1) -> bool:
    """Push a trust update to the router. Returns True on success.

    Best-effort — failures are logged but never raised; trust telemetry
    must never block the agent loop.
    """
    if not agent_id:
        return False
    body = {
        "agent_id": agent_id,
        "score": int(max(0, min(1000, score * 1000)) if score <= 1.0 else score),
        "interactions": int(max(0, interactions)),
    }
    try:
        resp = router_client.call("POST", "/agt/trust", json=body)
    except Exception as exc:  # noqa: BLE001
        logger.debug("trust push failed (transient — ok): %s", exc)
        return False
    if resp.status_code >= 400:
        logger.debug(
            "trust push HTTP %s (transient — ok): %s",
            resp.status_code,
            resp.text[:200],
        )
        return False
    return True


def submit_signing_counter(action: str) -> bool:
    """Push a signing event to the router.

    ``action`` must be one of ``signed`` | ``verified`` | ``rejected``
    (matches the router contract at ``routes/governance.rs:213+``).
    Best-effort like ``submit_trust``.
    """
    if action not in ("signed", "verified", "rejected"):
        return False
    try:
        resp = router_client.call("POST", "/agt/signing-counter", json={"action": action})
    except Exception as exc:  # noqa: BLE001
        logger.debug("signing-counter push failed: %s", exc)
        return False
    return resp.status_code < 400


def _post_tool_call_hook(
    tool_name: str,
    _params: dict[str, Any],
    result: Any,
    **_kwargs: Any,
) -> None:
    """Hermes ``post_tool_call`` hook — record success/failure as trust signal.

    Successful tool calls bump the agent's self-trust (interactions+1,
    score steady at 0.8). Failures don't downgrade — that would amplify
    false positives during transient router errors. Real trust loss
    happens via mesh peer feedback (Act 2).
    """
    # Failures are signalled by tool handlers returning JSON with an
    # 'error' key. Don't treat those as positive interactions.
    if isinstance(result, str) and '"error"' in result[:100]:
        return
    # Self-trust signal: kars_* + foundry_* tools count toward our
    # interaction tally. http_fetch + shell don't (they're agent-side
    # side effects, not peer interactions).
    if tool_name.startswith(("kars_", "foundry_")):
        agent_id = _kwargs.get("agent_id") or _self_agent_id()
        if agent_id:
            submit_trust(agent_id, score=0.8, interactions=1)


def _self_agent_id() -> str | None:
    """Resolve this sandbox's agent identifier for self-trust telemetry.

    Returns SANDBOX_NAME — same convention OpenClaw uses for self-
    interactions. May be refined in Act 2 once we have the canonical
    did:mesh:<hex32> derived from the Ed25519 key pair.
    """
    import os as _os

    return _os.environ.get("SANDBOX_NAME") or None


def register(ctx: Any) -> None:  # noqa: ANN401
    """Register the post_tool_call telemetry hook with Hermes."""
    ctx.register_hook("post_tool_call", _post_tool_call_hook)
    logger.info("telemetry post_tool_call hook registered")

