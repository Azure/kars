#!/usr/bin/env python3
"""AGT Governance Sidecar — thin HTTP wrapper around the official
Agent Governance Toolkit (microsoft/agent-governance-toolkit v3.0.0).

Every governance function delegates to real AGT components:
  - agentmesh TrustPolicy + PolicyEvaluator → native condition evaluation
    with operators: eq, in, matches (regex), lt, gt, gte, lte, ne, not_in
  - agentmesh.services.RateLimiter → HTTP-level token-bucket rate limiting
  - agentmesh.storage.FileTrustStore → persistent JSON-backed trust scores
  - agentmesh.AuditLog            → hash-chain tamper-evident audit
  - agentmesh.AgentDID            → agent identity format validation
  - agentmesh.services.AgentBehaviorMonitor → anomaly detection

When the AGT Rust SDK ships, this sidecar is removed entirely — the
router calls the crate inline with the same policy YAML.
"""

import json
import logging
import os
import re
import time

from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse

# ── AGT SDK imports (all from agentmesh v3.0.0) ─────────────────────
from agentmesh import AgentDID, AuditLog
from agentmesh.governance.trust_policy import (
    load_policies,
)
from agentmesh.governance.policy_evaluator import PolicyEvaluator
from agentmesh.services.rate_limiter import RateLimiter
from agentmesh.storage.file_trust_store import FileTrustStore
from agentmesh.services.behavior_monitor import AgentBehaviorMonitor

logging.basicConfig(
    level=getattr(logging, os.environ.get("LOG_LEVEL", "INFO")),
    format="%(asctime)s [agt-governance] %(levelname)s %(message)s",
)
log = logging.getLogger("agt-governance")

# ── Configuration ────────────────────────────────────────────────────
POLICY_DIR = os.environ.get(
    "AGT_POLICY_DIR", os.environ.get("POLICY_DIR", "/sandbox/.openclaw/policies")
)
PORT = int(os.environ.get("AGT_PORT", "8081"))
SANDBOX = os.environ.get("SANDBOX_NAME", "unknown")
TRUST_THRESHOLD = int(os.environ.get("AGT_TRUST_THRESHOLD", "500"))
TRUST_DB = os.environ.get("AGT_TRUST_DB", "/tmp/agt/trust_scores.json")

# ── Initialize AGT components ───────────────────────────────────────

# 1. TrustPolicy + PolicyEvaluator — native condition-based evaluation
#    Uses TrustCondition with operators: eq, in, matches (regex), lt/gt, etc.
#    No custom pre-processing — AGT evaluates conditions directly.
policies = []
policy_count = 0
if os.path.isdir(POLICY_DIR):
    try:
        policies = load_policies(POLICY_DIR)
        for p in policies:
            policy_count += len(p.rules)
            log.info("Loaded policy '%s': %d rules", p.name, len(p.rules))
    except Exception as e:
        log.warning("Native load_policies failed (%s), policies empty", e)

evaluator = PolicyEvaluator(policies)

# 2. RateLimiter — token-bucket per agent
rate_limiter = RateLimiter(
    global_rate=100, global_capacity=200,
    per_agent_rate=10, per_agent_capacity=20,
)

# 3. FileTrustStore — persistent JSON trust scores
os.makedirs(os.path.dirname(TRUST_DB), exist_ok=True)
trust_store = FileTrustStore(path=TRUST_DB, auto_save=True)

# 4. AuditLog — hash-chain tamper-evident audit trail
audit_log = AuditLog()

# 5. AgentBehaviorMonitor — anomaly detection
behavior_monitor = AgentBehaviorMonitor(
    burst_threshold=100,
    consecutive_failure_threshold=20,
    capability_denial_threshold=10,
)


def _score_to_tier(score: int) -> str:
    if score >= 800:
        return "Sovereign"
    if score >= 600:
        return "Verified"
    if score >= 400:
        return "Known"
    if score >= 200:
        return "Observed"
    return "Anonymous"


def _build_context(action_str: str, extra: dict = None) -> dict:
    """Parse action string into context dict for PolicyEvaluator.

    Splits 'shell:ls -la' into structured fields that TrustCondition
    operators (eq, in, matches, lt/gt) evaluate directly — no custom
    classification logic.
    """
    parts = action_str.split(":", 1)
    category = parts[0] if parts else "unknown"
    detail = parts[1] if len(parts) > 1 else ""
    cmd = detail.split()[0] if detail else ""

    ctx = {
        "action": {
            "full": action_str,
            "category": category,
            "detail": detail,
            "command": cmd,
        },
    }
    if extra:
        ctx.update(extra)
    return ctx


def _evaluate(agent_did: str, action_str: str, extra: dict = None) -> dict:
    """Run action through AGT PolicyEvaluator + RateLimiter + BehaviorMonitor."""
    # Rate limit check (token bucket)
    if not rate_limiter.allow(agent_did):
        audit_log.log("rate_limit", agent_did, action_str, outcome="denied",
                       policy_decision="deny")
        behavior_monitor.record_tool_call(agent_did, action_str, success=False)
        return {"allowed": False, "action": "deny",
                "reason": "Rate limited (token bucket)",
                "rate_limited": True}

    # Build context and evaluate via AGT PolicyEvaluator
    ctx = _build_context(action_str, extra)
    decision = evaluator.evaluate(ctx)

    outcome = "success" if decision.allowed else "denied"
    audit_log.log("policy_check", agent_did, action_str,
                   outcome=outcome, policy_decision=decision.action,
                   data={"rule": decision.rule_name})

    behavior_monitor.record_tool_call(agent_did, action_str,
                                       success=decision.allowed)

    return {
        "allowed": decision.allowed,
        "action": decision.action,
        "matched_rule": decision.rule_name,
        "reason": decision.reason,
        "rate_limited": False,
    }


log.info(
    "AGT governance ready: sandbox=%s policies=%d rules trust_threshold=%d "
    "trust_db=%s",
    SANDBOX, policy_count, TRUST_THRESHOLD, TRUST_DB,
)


# ── HTTP Handler ─────────────────────────────────────────────────────

class GovernanceHandler(BaseHTTPRequestHandler):

    def log_message(self, fmt, *args):
        pass  # suppress default HTTP logging

    def _json(self, status, data):
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(data, default=str).encode())

    def _body(self):
        length = int(self.headers.get("Content-Length", 0))
        return json.loads(self.rfile.read(length)) if length else {}

    # ── GET ───────────────────────────────────────────────────────────

    def do_GET(self):
        path = urlparse(self.path).path

        if path == "/healthz":
            return self._json(200, {"status": "ok", "sandbox": SANDBOX})

        if path == "/status":
            all_scores = trust_store.get_all_scores()
            trust_states = [
                {"agent_id": aid, "score": ts.get("score", 0),
                 "tier": _score_to_tier(ts.get("score", 0)),
                 "interactions": ts.get("interactions", 0),
                 "last_interaction": ts.get("last_interaction", "")}
                for aid, ts in all_scores.items()
            ]
            integrity_ok, _ = audit_log.verify_integrity()
            audit_count = len(audit_log.query(limit=10000))
            return self._json(200, {
                "enabled": True,
                "sandbox": SANDBOX,
                "policy_loaded": policy_count > 0,
                "policy_rules": policy_count,
                "audit_entries": audit_count,
                "audit_integrity": integrity_ok,
                "known_agents": len(trust_states),
                "trust_states": trust_states,
                "trust_threshold": TRUST_THRESHOLD,
                "trust_updates": len(all_scores),
            })

        if path == "/trust":
            all_scores = trust_store.get_all_scores()
            agents = [
                {"agent_id": aid, "score": ts.get("score", 0),
                 "tier": _score_to_tier(ts.get("score", 0)),
                 "interactions": ts.get("interactions", 0),
                 "last_interaction": ts.get("last_interaction", "")}
                for aid, ts in all_scores.items()
            ]
            return self._json(200, {"agents": agents})

        if path.startswith("/trust/"):
            agent_id = path.split("/trust/", 1)[1]
            try:
                AgentDID.from_string(agent_id)
            except ValueError:
                if not re.match(r"^[a-zA-Z0-9][a-zA-Z0-9._-]{1,62}$", agent_id):
                    return self._json(400, {
                        "error": "Invalid agent_id format"})
            ts = trust_store.get_trust_score(agent_id) or {
                "score": 0, "interactions": 0, "last_interaction": ""}
            return self._json(200, {
                "agent_id": agent_id, "score": ts.get("score", 0),
                "tier": _score_to_tier(ts.get("score", 0)),
                "interactions": ts.get("interactions", 0),
                "last_interaction": ts.get("last_interaction", ""),
            })

        if path == "/audit":
            entries = audit_log.query(limit=100)
            return self._json(200, {
                "entries": [
                    {"action": e.action, "agent_id": e.agent_did,
                     "decision": e.policy_decision or e.outcome,
                     "result": e.outcome,
                     "timestamp": str(e.timestamp)}
                    for e in entries
                ],
                "count": len(entries),
                "sandbox": SANDBOX,
            })

        if path == "/audit/verify":
            valid, msg = audit_log.verify_integrity()
            count = len(audit_log.query(limit=10000))
            return self._json(200, {
                "integrity": "valid" if valid else "COMPROMISED",
                "entries": count,
                "sandbox": SANDBOX,
                "message": msg,
            })

        self._json(404, {"error": "not found"})

    # ── POST ──────────────────────────────────────────────────────────

    def do_POST(self):
        path = urlparse(self.path).path

        if path == "/evaluate":
            body = self._body()
            action = body.get("action", "")
            if not action:
                return self._json(400, {"error": "Missing 'action' field"})
            agent_did = body.get("agent_id", SANDBOX)
            extra = body.get("context", {})

            result = _evaluate(agent_did, action, extra)
            status = 200 if result["allowed"] else 403
            result["decision"] = result["action"]
            return self._json(status, result)

        if path == "/trust":
            body = self._body()
            agent_id = body.get("agent_id", "")
            if not agent_id:
                return self._json(400, {"error": "agent_id required"})

            # Validate agent ID format via AGT AgentDID
            try:
                AgentDID.from_string(agent_id)
            except ValueError:
                # Accept lowercase agent names OR Base58-encoded AMIDs (mixed case)
                if not re.match(r"^[a-zA-Z0-9][a-zA-Z0-9._-]{1,62}$", agent_id):
                    return self._json(400, {
                        "error": "Invalid agent_id: 3-63 chars, "
                                 "alphanumeric, dots, hyphens"})

            # Reject self-trust updates (sandbox can't boost its own score)
            if agent_id == SANDBOX:
                return self._json(403, {
                    "error": "Cannot update own trust score"})

            score = int(body.get("score", 500))
            interactions = int(body.get("interactions", 0))
            now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())

            existing = trust_store.get_trust_score(agent_id) or {
                "score": 0, "interactions": 0, "last_interaction": ""}
            old_score = existing.get("score", 0)

            # Bound trust delta to ±200 per update (prevents score forging).
            # Exception: first interaction (no prior record) may set up to 500
            # to support KNOCK trust bootstrap (X3DH handshake = crypto identity).
            MAX_DELTA = 200
            is_new = existing.get("interactions", 0) == 0
            max_initial = 500 if is_new else MAX_DELTA
            clamped = max(old_score - MAX_DELTA, min(old_score + max_initial, score))
            clamped = max(0, min(1000, clamped))

            trust_store.store_trust_score(agent_id, {
                "score": clamped,
                "interactions": existing.get("interactions", 0) + interactions,
                "last_interaction": now,
            })

            audit_log.log("trust_update", agent_id,
                           f"trust_update:{agent_id}",
                           data={"score": clamped, "requested": score,
                                 "previous": old_score},
                           outcome="success")

            return self._json(200, {
                "ok": True, "agent_id": agent_id, "score": clamped})

        if path == "/report_content_flag":
            body = self._body()
            agent_id = body.get("agent_id", SANDBOX)
            flags = body.get("flags", {})
            filtered = body.get("filtered_categories", [])
            detected = body.get("detected_categories", [])
            penalty = int(body.get("trust_penalty", 0))

            # Audit log the content flag event
            audit_log.log("content_flag", agent_id,
                           f"content_flag:{','.join(filtered + detected)}",
                           data={"flags": flags, "filtered": filtered,
                                 "detected": detected, "penalty": penalty},
                           outcome="flagged")

            # Record as suspicious behavior
            flag_summary = ",".join(filtered + detected) or "unknown"
            behavior_monitor.record_tool_call(
                agent_id, f"content_flag:{flag_summary}", success=False)

            # Apply trust penalty (negative score adjustment)
            if penalty < 0:
                existing = trust_store.get_trust_score(agent_id) or {
                    "score": 500, "interactions": 0, "last_interaction": ""}
                old_score = existing.get("score", 500)
                new_score = max(0, old_score + penalty)
                now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
                trust_store.store_trust_score(agent_id, {
                    "score": new_score,
                    "interactions": existing.get("interactions", 0) + 1,
                    "last_interaction": now,
                })
                log.warning(
                    "Content flag: agent=%s categories=%s penalty=%d "
                    "trust=%d→%d",
                    agent_id, flag_summary, penalty, old_score, new_score)
                return self._json(200, {
                    "ok": True, "penalty_applied": penalty,
                    "trust_score": new_score, "previous_score": old_score})

            return self._json(200, {
                "ok": True, "penalty_applied": 0,
                "trust_score": None})

        self._json(404, {"error": "not found"})


# ── Start ────────────────────────────────────────────────────────────

if __name__ == "__main__":
    bind = os.environ.get("AGT_BIND", "0.0.0.0")
    server = HTTPServer((bind, PORT), GovernanceHandler)
    log.info("Listening on %s:%d", bind, PORT)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        log.info("Shutting down")
        server.server_close()
