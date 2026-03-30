#!/usr/bin/env python3
"""AGT Governance Sidecar — thin HTTP wrapper around the official
Agent Governance Toolkit (microsoft/agent-governance-toolkit v3.0.0).

Exposes the AGT PolicyEngine, TrustManager, and AuditLogger over REST
so the inference-router can proxy /agt/* routes to localhost:8081.

When the AGT Rust SDK ships, this sidecar is removed entirely — the
router calls the crate inline with the same policy YAML.
"""

import asyncio
import json
import logging
import os
import sys

from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse

# AGT SDK imports (from pip install agent-governance-toolkit[full])
from agent_os import StatelessKernel, ExecutionContext, GovernancePolicy
from agentmesh import TrustManager, AuditLogger

logging.basicConfig(
    level=getattr(logging, os.environ.get("LOG_LEVEL", "INFO")),
    format="%(asctime)s [agt-governance] %(levelname)s %(message)s",
)
log = logging.getLogger("agt-governance")

# ── Configuration ────────────────────────────────────────────────────
POLICY_DIR = os.environ.get("AGT_POLICY_DIR", os.environ.get("POLICY_DIR", "/etc/agt/policies"))
PORT = int(os.environ.get("AGT_PORT", "8081"))
METRICS_PORT = int(os.environ.get("AGT_METRICS_PORT", "9091"))
SANDBOX = os.environ.get("SANDBOX_NAME", "unknown")
TRUST_THRESHOLD = float(os.environ.get("AGT_TRUST_THRESHOLD", "500")) / 1000.0
EXECUTION_RING = int(os.environ.get("EXECUTION_RING", "3"))

# ── Initialize AGT components ───────────────────────────────────────
kernel = StatelessKernel()

# Load policies from mounted ConfigMap directory
policies = []
if os.path.isdir(POLICY_DIR):
    for fname in sorted(os.listdir(POLICY_DIR)):
        if fname.endswith((".yaml", ".yml")):
            fpath = os.path.join(POLICY_DIR, fname)
            try:
                policy = GovernancePolicy.from_yaml(fpath)
                policies.append(policy)
                log.info("Loaded policy: %s (%s)", fname, policy.name)
            except Exception as e:
                log.error("Failed to load %s: %s", fname, e)

ctx = ExecutionContext(
    agent_id=SANDBOX,
    policies=[p.name for p in policies],
    ring=EXECUTION_RING,
)

trust = TrustManager(initial_score=TRUST_THRESHOLD)
audit = AuditLogger(agent_id=SANDBOX)

log.info(
    "AGT governance ready: sandbox=%s policies=%d ring=%d trust_threshold=%.2f",
    SANDBOX, len(policies), EXECUTION_RING, TRUST_THRESHOLD,
)


# ── HTTP Handler ─────────────────────────────────────────────────────

class GovernanceHandler(BaseHTTPRequestHandler):
    """Minimal HTTP handler — no framework dependency."""

    def log_message(self, format, *args):
        # Suppress default access logs; we log via Python logging
        pass

    def _json_response(self, status, data):
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(data).encode())

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
            # Normalize trust_states to array format expected by operator
            raw_scores = trust.all_scores()
            if isinstance(raw_scores, dict):
                trust_states = [
                    {"agent_id": aid, "score": int(s * 1000) if isinstance(s, float) and s <= 1.0 else int(s),
                     "tier": "Sovereign" if (int(s * 1000) if isinstance(s, float) and s <= 1.0 else int(s)) >= 800
                            else "Verified" if (int(s * 1000) if isinstance(s, float) and s <= 1.0 else int(s)) >= 600
                            else "Known" if (int(s * 1000) if isinstance(s, float) and s <= 1.0 else int(s)) >= 400
                            else "Observed" if (int(s * 1000) if isinstance(s, float) and s <= 1.0 else int(s)) >= 200
                            else "Anonymous",
                     "interactions": 0, "last_interaction": ""}
                    for aid, s in raw_scores.items()
                ]
            elif isinstance(raw_scores, list):
                trust_states = raw_scores
            else:
                trust_states = []

            return self._json_response(200, {
                "enabled": True,
                "sandbox": SANDBOX,
                "policy_loaded": len(policies) > 0,
                "policy_rules": len(policies),
                "audit_entries": audit.entry_count,
                "audit_integrity": audit.verify_integrity(),
                "known_agents": len(trust.agents) if hasattr(trust, 'agents') else len(trust_states),
                "trust_states": trust_states,
                "trust_threshold": int(TRUST_THRESHOLD * 1000),
            })

        if path == "/trust":
            raw_scores = trust.all_scores()
            if isinstance(raw_scores, dict):
                agents = [
                    {"agent_id": aid, "score": int(s * 1000) if isinstance(s, float) and s <= 1.0 else int(s)}
                    for aid, s in raw_scores.items()
                ]
            elif isinstance(raw_scores, list):
                agents = raw_scores
            else:
                agents = []
            return self._json_response(200, {"agents": agents})

        if path.startswith("/trust/"):
            agent_id = path.split("/trust/", 1)[1]
            return self._json_response(200, trust.get_score(agent_id))

        if path == "/audit":
            return self._json_response(200, {
                "entries": audit.get_entries(),
                "count": audit.entry_count,
                "sandbox": SANDBOX,
            })

        if path == "/audit/verify":
            valid = audit.verify_integrity()
            return self._json_response(200, {
                "integrity": "valid" if valid else "COMPROMISED",
                "entries": audit.entry_count,
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

            result = asyncio.get_event_loop().run_until_complete(
                kernel.evaluate(action=action, context=ctx)
            )

            decision = result.decision  # "allow", "deny", "requires_approval"
            audit.log(action=action, decision=decision, detail=str(result.reason or ""))

            status_map = {"allow": 200, "deny": 403, "requires_approval": 202, "rate_limited": 429}
            return self._json_response(
                status_map.get(decision, 200),
                {"decision": decision, "action": action, "reason": result.reason},
            )

        if path == "/trust":
            body = self._read_body()
            agent_id = body.get("agent_id", "")
            if not agent_id:
                return self._json_response(400, {"error": "agent_id required"})

            score = body.get("score", 500)
            trust.update(agent_id, score / 1000.0)
            audit.log(
                action=f"trust_update:{agent_id}",
                decision="applied",
                detail=f"score={score}",
            )
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
        server.server_close()
