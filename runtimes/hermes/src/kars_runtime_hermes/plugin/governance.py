"""AGT policy gate — Phase A1.4.

Every tool call goes through ``ctx.register_hook("pre_tool_call", ...)``
which POSTs the action verb to ``/agt/evaluate`` on the inference
router. The router's `Governance::evaluate` runs the `PolicyEngine` from
the loaded `ToolPolicy` and returns Allow / Deny / RequiresApproval /
RateLimited.

Denied calls short-circuit — the tool is not executed, and the LLM
sees an error result instead.

Mirror of the OpenClaw implementation at
``runtimes/openclaw/src/index.ts:2778-2822`` (the monkey-patch around
``registerTool``). Hermes' native ``pre_tool_call`` hook is cleaner —
no monkey-patch needed — but the wire shape to ``/agt/evaluate`` and
the fail-closed semantics are identical.

Contract reference: ``docs/runtimes/CONTRACT.md`` §HTTP contract +
§Action verb taxonomy.
"""

from __future__ import annotations

import logging
import os
import re
from typing import Any

from . import router_client

logger = logging.getLogger("kars.hermes.governance")

# Fail-closed counter — at FAIL_CLOSED_THRESHOLD consecutive failures
# to reach /agt/evaluate, we block. Below the threshold we allow with a
# warning so the agent doesn't wedge during a transient router restart.
#
# Configurable via env per the runtime contract; capped at 10.
FAIL_CLOSED_THRESHOLD: int = max(
    0,
    min(10, int(os.environ.get("KARS_AGT_EVALUATE_FAIL_OPEN_GRACE", "3"))),
)

# Process-wide counter (Hermes is single-process per pod, so a module
# global is correct here — same pattern as the OpenClaw plugin's
# govFailCount).
_consecutive_failures = 0

# Routes for canonical action verb construction (see CONTRACT.md
# §Canonical action construction).
_MAX_ACTION_LEN = 256
_REDACT_PATTERNS = [
    # bearer tokens
    (re.compile(r"(?i)bearer\s+[\w\-.+/=]{8,}"), "Bearer <REDACTED>"),
    # API keys: keep key name, redact value
    (re.compile(r"(?i)(api[_-]?key|api[_-]?secret|access[_-]?token|auth[_-]?token)([=:\s]+)([\w\-.+/=]{8,})"),
     r"\1\2<REDACTED>"),
    # JWT-shaped tokens
    (re.compile(r"\beyJ[A-Za-z0-9_\-]{20,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\b"),
     "<REDACTED_JWT>"),
    # ghp_/ghs_/sk- style secrets
    (re.compile(r"\b(?:ghp_|ghs_|sk-)[A-Za-z0-9]{16,}\b"), "<REDACTED_SECRET>"),
]


def _canonicalize(s: str) -> str:
    """Apply the canonical action-construction rules from the contract.

    Sequence: redact → strip newlines → utf-8-replace → truncate.
    Idempotent.
    """
    out = s
    for pat, repl in _REDACT_PATTERNS:
        out = pat.sub(repl, out)
    out = out.replace("\n", " ").replace("\r", " ").replace("\0", " ")
    out = out.encode("utf-8", errors="replace").decode("utf-8", errors="replace")
    if len(out) > _MAX_ACTION_LEN:
        out = out[: _MAX_ACTION_LEN - 3] + "..."
    return out


def _action_verb(tool_name: str, params: dict[str, Any]) -> str:
    """Derive the canonical action verb for ``/agt/evaluate``.

    Verb taxonomy (see CONTRACT.md §Action verb taxonomy):
      * exec_command / foundry_code_execute → ``shell:<command>``
      * http_fetch → ``egress:<url>``
      * foundry_memory → ``memory:<op>``
      * kars_mesh_send → ``mesh:send:<target>``
      * kars_mesh_transfer_file → ``mesh:transfer_file:<target>``
      * kars_handoff_request → ``handoff:request:<target>``
      * everything else → ``tool:<name>:<param-summary>``
    """
    if tool_name in ("exec_command", "foundry_code_execute"):
        cmd = str(params.get("command", params.get("code", "")))
        return _canonicalize(f"shell:{cmd}")

    if tool_name == "http_fetch":
        url = str(params.get("url", ""))
        return _canonicalize(f"egress:{url}")

    if tool_name == "foundry_memory":
        op = str(params.get("operation", "")).lower()
        return _canonicalize(f"memory:{op}")

    if tool_name == "kars_mesh_send":
        # Accept all three conventional arg names — the OpenClaw
        # canonical (`to_agent`), the Hermes short form (`to`), and
        # the legacy `target_agent` an earlier draft used.
        target = str(
            params.get("to_agent")
            or params.get("to")
            or params.get("target_agent")
            or ""
        )
        return _canonicalize(f"mesh:send:{target}")
    if tool_name == "kars_mesh_transfer_file":
        target = str(params.get("to_agent") or params.get("to") or "")
        return _canonicalize(f"mesh:transfer_file:{target}")
    if tool_name in ("kars_mesh_inbox", "kars_mesh_await"):
        # Drain / wait ops — capture the senders filter so audit shows
        # "who the agent is listening for".
        senders = params.get("senders") or []
        if isinstance(senders, list):
            tag = ",".join(str(s) for s in senders[:3])
        else:
            tag = str(senders)
        op = "inbox" if tool_name == "kars_mesh_inbox" else "await"
        return _canonicalize(f"mesh:{op}:{tag}")

    if tool_name == "kars_handoff_request":
        target = str(params.get("target", ""))
        return _canonicalize(f"handoff:request:{target}")
    if tool_name == "kars_handoff_confirm":
        return _canonicalize(f"handoff:confirm:{params.get('token', '')[:8]}")

    if tool_name == "kars_spawn":
        child = str(params.get("name", ""))
        return _canonicalize(f"spawn:create:{child}")

    # Default: tool:<name>:<first-significant-param>
    first_param_summary = ""
    for k in ("query", "name", "url", "path", "content", "agent_id"):
        if k in params:
            first_param_summary = str(params[k])
            break
    if first_param_summary:
        return _canonicalize(f"tool:{tool_name}:{first_param_summary}")
    return _canonicalize(f"tool:{tool_name}:")


class GovernanceDecision:
    """Decision returned by the router's /agt/evaluate."""

    __slots__ = ("allowed", "decision", "reason", "matched_rule", "rate_limited")

    def __init__(
        self,
        *,
        allowed: bool,
        decision: str = "allow",
        reason: str | None = None,
        matched_rule: str | None = None,
        rate_limited: bool = False,
    ) -> None:
        self.allowed = allowed
        self.decision = decision
        self.reason = reason
        self.matched_rule = matched_rule
        self.rate_limited = rate_limited


def _grace_or_block(failure_reason: str) -> GovernanceDecision:
    """Apply fail-closed grace period semantics."""
    global _consecutive_failures
    _consecutive_failures += 1
    if FAIL_CLOSED_THRESHOLD == 0 or _consecutive_failures >= FAIL_CLOSED_THRESHOLD:
        logger.warning(
            "AGT governance unreachable (%d/%d failures) — failing closed: %s",
            _consecutive_failures,
            FAIL_CLOSED_THRESHOLD,
            failure_reason,
        )
        return GovernanceDecision(
            allowed=False,
            decision="deny",
            reason=f"AGT governance unreachable (fail-closed): {failure_reason}",
        )
    logger.warning(
        "AGT governance unreachable (%d/%d failures) — allowing under grace: %s",
        _consecutive_failures,
        FAIL_CLOSED_THRESHOLD,
        failure_reason,
    )
    return GovernanceDecision(allowed=True, decision="allow")


def evaluate(tool_name: str, params: dict[str, Any]) -> GovernanceDecision:
    """Call /agt/evaluate for this tool call.

    Returns a ``GovernanceDecision``. The hook decides whether to
    continue the tool execution or short-circuit with an error.
    """
    global _consecutive_failures
    action = _action_verb(tool_name, params)
    body = {"action": action, "context": {"tool": tool_name}}
    try:
        resp = router_client.call("POST", "/agt/evaluate", json=body)
    except Exception as exc:  # noqa: BLE001 — fail-closed contract
        return _grace_or_block(repr(exc))

    if resp.status_code >= 400:
        return _grace_or_block(f"HTTP {resp.status_code}")

    try:
        data: dict[str, Any] = resp.json()
    except Exception as exc:  # noqa: BLE001
        return _grace_or_block(f"non-JSON response: {exc}")

    # Success path resets the consecutive-failure counter.
    _consecutive_failures = 0

    allowed = bool(data.get("allowed", True))
    return GovernanceDecision(
        allowed=allowed,
        decision=str(data.get("decision", "allow")),
        reason=data.get("reason"),
        matched_rule=data.get("matched_rule"),
        rate_limited=bool(data.get("rate_limited", False)),
    )


def _on_pre_tool_call(
    tool_name: str = "",
    args: dict[str, Any] | None = None,
    task_id: str = "",
    session_id: str = "",
    tool_call_id: str = "",
    **_kwargs: Any,
) -> Any:
    """Hermes ``pre_tool_call`` hook implementation.

    Hermes invokes hooks with KEYWORD arguments matching the names in
    its own VALID_HOOKS spec (plugins.py:1688): ``tool_name=``,
    ``args=``, ``task_id=``, ``session_id=``, ``tool_call_id=``.
    Earlier versions of this hook used ``params=`` which silently
    captured nothing because Python rejected the mismatch via kwargs
    and the hook crashed → Hermes swallowed the exception → no
    audit entry was ever written for tool calls. Only the inference
    gate (``/v1/responses``, audited by the router proxy itself)
    showed up in the AGT audit log.

    On deny we return a JSON-string result that Hermes interprets as
    short-circuiting the call (returns ``{"action": "block",
    "message": "..."}`` semantics expected by ``get_pre_tool_call_block_message``).
    """
    params = args or {}
    decision = evaluate(tool_name, params)
    if decision.allowed:
        return None  # proceed to tool dispatch

    reason = decision.reason or "denied by AGT policy"
    rule = decision.matched_rule or "unspecified"
    msg = f'⛔ Blocked by AGT policy: rule "{rule}" — {reason}'
    logger.warning("AGT DENY %s: %s", tool_name, reason)

    # Hermes ≥0.15 wants {"action": "block", "message": "..."} from
    # pre_tool_call hooks to short-circuit the tool. Earlier code
    # returned a JSON string which Hermes' check
    # (plugins.py::get_pre_tool_call_block_message) silently ignored
    # — the deny decision was logged but the tool still ran. Returning
    # the proper dict shape makes the block actually block.
    return {"action": "block", "message": msg}


def register(ctx: Any) -> None:  # noqa: ANN401
    """Register the AGT pre_tool_call hook with Hermes."""
    ctx.register_hook("pre_tool_call", _on_pre_tool_call)
    logger.info(
        "AGT governance pre_tool_call hook registered (fail-closed grace: %d)",
        FAIL_CLOSED_THRESHOLD,
    )


def _reset_for_testing() -> None:
    """Test helper — reset module state for unit tests."""
    global _consecutive_failures
    _consecutive_failures = 0

