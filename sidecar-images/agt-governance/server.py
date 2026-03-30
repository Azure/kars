#!/usr/bin/env python3
"""AGT Governance Sidecar — thin HTTP wrapper around the official
Agent Governance Toolkit (microsoft/agent-governance-toolkit v3.0.0).

Every governance function delegates to real AGT components:
  - agentmesh.PolicyEngine        → policy evaluation + built-in rate limiting
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
import yaml

from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse

# ── AGT SDK imports (all from agentmesh v3.0.0) ─────────────────────
from agentmesh import (
    AgentDID,
    AuditLog,
    PolicyEngine,
)
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
TRUST_DB = os.environ.get("AGT_TRUST_DB", "/sandbox/.agt/trust_scores.json")
AUDIT_SINK = os.environ.get("AGT_AUDIT_SINK", None)

# ── Safe shell commands & destructive patterns ───────────────────────
SAFE_SHELL = frozenset([
    "ls", "cat", "grep", "find", "echo", "head", "tail", "wc", "sort",
    "uniq", "diff", "python", "python3", "pip", "node", "npm", "git",
    "curl", "jq", "sed", "awk",
])
DESTRUCTIVE_PATTERNS = re.compile(
    r"rm\s+-rf\s+/|mkfs|shutdown|reboot|chmod\s+777|dd\s+if=", re.IGNORECASE
)
INJECTION_PATTERNS = re.compile(
    r"ignore previous instructions|ignore all prior|you are now|"
    r"new system prompt|DROP TABLE|UNION SELECT|rm -rf /|; curl |<script>",
    re.IGNORECASE,
)

# ── Initialize AGT components ───────────────────────────────────────

# 1. PolicyEngine — loads YAML policies, evaluates with condition expressions
policy_engine = PolicyEngine()
policy_count = 0

if os.path.isdir(POLICY_DIR):
    for fname in sorted(os.listdir(POLICY_DIR)):
        if not fname.endswith((".yaml", ".yml")):
            continue
        fpath = os.path.join(POLICY_DIR, fname)
        try:
            with open(fpath) as fh:
                content = fh.read()
            data = yaml.safe_load(content)
            if "name" in data and "rules" in data:
                policy = policy_engine.load_yaml(content)
                policy_count = len(policy.rules)
                log.info("Loaded policy '%s': %d rules", policy.name, policy_count)
            else:
                log.warning("Skipping %s: missing 'name' or 'rules'", fname)
        except Exception as e:
            log.error("Failed to load %s: %s", fname, e)

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
    if score >= 800: return "Sovereign"
    if score >= 600: return "Verified"
    if score >= 400: return "Known"
    if score >= 200: return "Observed"
    return "Anonymous"


def _build_context(action_str: str, extra: dict = None) -> dict:
    """Pre-process an action string into structured context for PolicyEngine.

    PolicyEngine conditions use field == 'value' and boolean field checks.
    We parse 'shell:ls', 'inference:chat', 'output:validate', etc. into
    nested dicts with pre-computed boolean flags.
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
            "safe": cmd in SAFE_SHELL if category == "shell" else False,
            "destructive": bool(DESTRUCTIVE_PATTERNS.search(action_str)),
            "injection_detected": bool(INJECTION_PATTERNS.search(detail))
                if category == "output" else False,
            "bulk": detail.startswith("bulk") or detail.startswith("promote"),
            "untrusted": False,
        },
    }
    if extra:
        # Merge trust info for mesh messages
        if "trust_score" in extra:
            ctx["action"]["untrusted"] = extra["trust_score"] < TRUST_THRESHOLD
        ctx.update({k: v for k, v in extra.items() if k != "action"})
    return ctx


def _evaluate(agent_did: str, action_str: str, extra: dict = None) -> dict:
    """Run action through AGT PolicyEngine + RateLimiter + BehaviorMonitor."""
    # Rate limit check (token bucket)
    if not rate_limiter.allow(agent_did):
        audit_log.log("rate_limit", agent_did, action_str, outcome="denied",
                       policy_decision="deny")
        behavior_monitor.record_tool_call(agent_did, action_str, success=False)
        return {"allowed": False, "action": "deny",
                "reason": "Rate limited (token bucket)",
                "rate_limited": True}

    # Build structured context and evaluate policy
    ctx = _build_context(action_str, extra)
    decision = policy_engine.evaluate(agent_did, ctx)

    outcome = "success" if decision.allowed else "denied"
    audit_log.log("policy_check", agent_did, action_str,
                   outcome=outcome, policy_decision=decision.action,
                   data={"rule": decision.matched_rule,
                         "policy": decision.policy_name})

    behavior_monitor.record_tool_call(agent_did, action_str,
                                       success=decision.allowed)

    return {
        "allowed": decision.allowed,
        "action": decision.action,
        "matched_rule": decision.matched_rule,
        "policy_name": decision.policy_name,
        "reason": decision.reason,
        "rate_limited": decision.rate_limited,
        "evaluation_ms": decision.evaluation_ms,
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
            integrity_ok, integrity_msg = audit_log.verify_integrity()
            entries = audit_log.query(limit=1)  # just get count efficiently
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
                if not re.match(r"^[a-z0-9][a-z0-9._-]{1,62}$", agent_id):
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
            # Map to legacy field name for router compatibility
            result["decision"] = result["action"]
            return self._json(status, result)

        if path == "/trust":
            body = self._body()
            agent_id = body.get("agent_id", "")
            if not agent_id:
                return self._json(400, {"error": "agent_id required"})

            # Validate agent ID format
            try:
                AgentDID.from_string(agent_id)
            except ValueError:
                if not re.match(r"^[a-z0-9][a-z0-9._-]{1,62}$", agent_id):
                    return self._json(400, {
                        "error": "Invalid agent_id: 3-63 chars, "
                                 "lowercase alphanumeric, dots, hyphens"})

            score = int(body.get("score", 500))
            interactions = int(body.get("interactions", 0))
            now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())

            existing = trust_store.get_trust_score(agent_id) or {
                "score": 0, "interactions": 0, "last_interaction": ""}
            trust_store.store_trust_score(agent_id, {
                "score": score,
                "interactions": existing.get("interactions", 0) + interactions,
                "last_interaction": now,
            })

            audit_log.log("trust_update", agent_id,
                           f"trust_update:{agent_id}",
                           data={"score": score}, outcome="success")

            return self._json(200, {
                "ok": True, "agent_id": agent_id, "score": score})

        self._json(404, {"error": "not found"})


# ── Start ────────────────────────────────────────────────────────────

if __name__ == "__main__":
    server = HTTPServer(("127.0.0.1", PORT), GovernanceHandler)
    log.info("Listening on 127.0.0.1:%d", PORT)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        log.info("Shutting down")
        server.server_close()
