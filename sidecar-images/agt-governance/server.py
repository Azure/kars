#!/usr/bin/env python3
"""AGT Governance Sidecar — thin HTTP wrapper around the official
Agent Governance Toolkit (microsoft/agent-governance-toolkit v3.0.0).

Exposes PolicyEngine, FlightRecorder (audit), and trust scoring over REST
so the inference-router can proxy /agt/* routes to localhost:8081.

When the AGT Rust SDK ships, this sidecar is removed entirely — the
router calls the crate inline with the same policy YAML.
"""

import hashlib
import json
import logging
import os
import time
import threading

from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse

# AGT SDK imports — verified against agent-governance-toolkit 3.0.0
from agent_os import PolicyEngine, FlightRecorder, PolicyDecision

logging.basicConfig(
    level=getattr(logging, os.environ.get("LOG_LEVEL", "INFO")),
    format="%(asctime)s [agt-governance] %(levelname)s %(message)s",
)
log = logging.getLogger("agt-governance")

# ── Configuration ────────────────────────────────────────────────────
POLICY_DIR = os.environ.get("AGT_POLICY_DIR",
    os.environ.get("POLICY_DIR", "/sandbox/.openclaw/policies"))
PORT = int(os.environ.get("AGT_PORT", "8081"))
SANDBOX = os.environ.get("SANDBOX_NAME", "unknown")
TRUST_THRESHOLD = int(os.environ.get("AGT_TRUST_THRESHOLD", "500"))

# ── Initialize AGT components ───────────────────────────────────────
policy = PolicyEngine()

# Load YAML policies into a simple lookup structure
import yaml
_allowed_actions = set()
_denied_actions = set()
_policy_names = []

if os.path.isdir(POLICY_DIR):
    for fname in sorted(os.listdir(POLICY_DIR)):
        if not fname.endswith((".yaml", ".yml")):
            continue
        fpath = os.path.join(POLICY_DIR, fname)
        try:
            with open(fpath) as fh:
                data = yaml.safe_load(fh)
            for p in data.get("policies", []):
                name = p.get("name", fname)
                ptype = p.get("type", "")
                _policy_names.append(name)

                if ptype == "capability":
                    for a in p.get("allowed_actions", []):
                        _allowed_actions.add(a)
                    for d in p.get("denied_actions", []):
                        _denied_actions.add(d)

                log.info("Loaded policy: %s (%s)", name, ptype)
        except Exception as e:
            log.error("Failed to load %s: %s", fname, e)

policy_count = len(_policy_names)

def evaluate_action(action: str) -> tuple:
    """Returns (decision, reason). Decision: 'allow' or 'deny'."""
    # Check explicit denials first (exact + prefix match)
    for d in _denied_actions:
        if d.endswith("*") and action.startswith(d.rstrip("*")):
            return ("deny", f"Matches denied pattern: {d}")
        if action == d:
            return ("deny", f"Action explicitly denied: {d}")
    # Check explicit allows
    for a in _allowed_actions:
        if a.endswith("*") and action.startswith(a.rstrip("*")):
            return ("allow", "")
        if action == a:
            return ("allow", "")
    # Default: allow (fail open for actions not covered by policy)
    return ("allow", "")

# FlightRecorder for tamper-evident audit
recorder = FlightRecorder(db_path="/tmp/agt-audit.db")

# Simple trust store (agent_id → {score, interactions, last_interaction})
_trust_lock = threading.Lock()
_trust_store: dict = {}

def _score_to_tier(score: int) -> str:
    if score >= 800: return "Sovereign"
    if score >= 600: return "Verified"
    if score >= 400: return "Known"
    if score >= 200: return "Observed"
    return "Anonymous"

log.info(
    "AGT governance ready: sandbox=%s policies=%d trust_threshold=%d",
    SANDBOX, policy_count, TRUST_THRESHOLD,
)


# ── HTTP Handler ─────────────────────────────────────────────────────

class GovernanceHandler(BaseHTTPRequestHandler):

    def log_message(self, format, *args):
        pass

    def _json_response(self, status, data):
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(data, default=str).encode())

    def _read_body(self):
        length = int(self.headers.get("Content-Length", 0))
        if length == 0:
            return {}
        return json.loads(self.rfile.read(length))

    def do_GET(self):
        path = urlparse(self.path).path

        if path == "/healthz":
            return self._json_response(200, {"status": "ok", "sandbox": SANDBOX})

        if path == "/status":
            with _trust_lock:
                trust_states = [
                    {"agent_id": aid, "score": ts["score"],
                     "tier": _score_to_tier(ts["score"]),
                     "interactions": ts["interactions"],
                     "last_interaction": ts["last_interaction"]}
                    for aid, ts in _trust_store.items()
                ]
            stats = recorder.get_statistics()
            audit_count = stats.get("total_events", 0) if isinstance(stats, dict) else 0
            return self._json_response(200, {
                "enabled": True,
                "sandbox": SANDBOX,
                "policy_loaded": policy_count > 0,
                "policy_rules": policy_count,
                "audit_entries": audit_count,
                "audit_integrity": recorder.verify_integrity(),
                "known_agents": len(trust_states),
                "trust_states": trust_states,
                "trust_threshold": TRUST_THRESHOLD,
            })

        if path == "/trust":
            with _trust_lock:
                agents = [
                    {"agent_id": aid, "score": ts["score"],
                     "tier": _score_to_tier(ts["score"]),
                     "interactions": ts["interactions"],
                     "last_interaction": ts["last_interaction"]}
                    for aid, ts in _trust_store.items()
                ]
            return self._json_response(200, {"agents": agents})

        if path.startswith("/trust/"):
            agent_id = path.split("/trust/", 1)[1]
            with _trust_lock:
                ts = _trust_store.get(agent_id, {"score": 0, "interactions": 0, "last_interaction": ""})
            return self._json_response(200, {
                "agent_id": agent_id, "score": ts["score"],
                "tier": _score_to_tier(ts["score"]),
                "interactions": ts["interactions"],
                "last_interaction": ts["last_interaction"],
            })

        if path == "/audit":
            logs = recorder.get_log(limit=100)
            entries = []
            if isinstance(logs, list):
                for e in logs:
                    entries.append({
                        "action": getattr(e, "tool_name", None) or getattr(e, "action", str(e)),
                        "decision": getattr(e, "status", "unknown"),
                        "timestamp": getattr(e, "timestamp", ""),
                        "agent_id": SANDBOX,
                    })
            return self._json_response(200, {
                "entries": entries,
                "count": len(entries),
                "sandbox": SANDBOX,
            })

        if path == "/audit/verify":
            valid = recorder.verify_integrity()
            stats = recorder.get_statistics()
            count = stats.get("total_events", 0) if isinstance(stats, dict) else 0
            return self._json_response(200, {
                "integrity": "valid" if valid else "COMPROMISED",
                "entries": count,
                "sandbox": SANDBOX,
            })

        self._json_response(404, {"error": "not found"})

    def do_POST(self):
        path = urlparse(self.path).path

        if path == "/evaluate":
            body = self._read_body()
            action = body.get("action", "")
            if not action:
                return self._json_response(400, {"error": "Missing 'action' field"})

            decision, reason = evaluate_action(action)
            status = 403 if decision == "deny" else 200

            # Log to FlightRecorder
            try:
                if decision == "allow":
                    recorder.log_success(tool_name=action, input_data=json.dumps(body.get("context", {})))
                else:
                    recorder.log_violation(tool_name=action, violation_type="policy_deny", details=reason)
            except Exception as e:
                log.warning("Audit log failed: %s", e)

            return self._json_response(status, {
                "decision": decision, "action": action, "reason": reason,
            })

        if path == "/trust":
            body = self._read_body()
            agent_id = body.get("agent_id", "")
            if not agent_id:
                return self._json_response(400, {"error": "agent_id required"})

            score = int(body.get("score", 500))
            interactions = int(body.get("interactions", 0))
            now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())

            with _trust_lock:
                existing = _trust_store.get(agent_id, {"score": 0, "interactions": 0, "last_interaction": ""})
                _trust_store[agent_id] = {
                    "score": score,
                    "interactions": existing["interactions"] + interactions,
                    "last_interaction": now,
                }

            try:
                recorder.log_success(tool_name=f"trust_update:{agent_id}", input_data=f"score={score}")
            except Exception:
                pass

            return self._json_response(200, {"ok": True, "agent_id": agent_id, "score": score})

        self._json_response(404, {"error": "not found"})


# ── Start ────────────────────────────────────────────────────────────

if __name__ == "__main__":
    server = HTTPServer(("127.0.0.1", PORT), GovernanceHandler)
    log.info("Listening on 127.0.0.1:%d", PORT)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        log.info("Shutting down")
        recorder.close()
        server.server_close()
