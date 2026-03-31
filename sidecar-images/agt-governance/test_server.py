#!/usr/bin/env python3
"""Unit tests for the AGT governance sidecar (server.py).

Mocks the agentmesh SDK (not installed locally) then starts the HTTP
server in-process on a random port and exercises every endpoint.
"""

import json
import os
import re
import sys
import threading
import time
import types
import unittest
from http.client import HTTPConnection
from http.server import HTTPServer

# =====================================================================
# Mock agentmesh SDK — must be registered before importing server.py
# =====================================================================


class _Decision:
    """Mirrors agentmesh PolicyEvaluator decision object."""

    def __init__(self, allowed, action, rule_name, reason):
        self.allowed = allowed
        self.action = action
        self.rule_name = rule_name
        self.reason = reason


class MockPolicyEvaluator:
    def __init__(self, policies=None):
        self.policies = policies or []

    def evaluate(self, ctx):
        full = ctx.get("action", {}).get("full", "")
        if "rm -rf /" in full or "mkfs" in full:
            return _Decision(False, "deny", "shell-destructive-deny",
                             "Destructive command blocked")
        return _Decision(True, "allow", "default-allow",
                         "No deny rule matched")


class MockRateLimiter:
    def __init__(self, **kwargs):
        self._blocked = set()

    def allow(self, agent_id):
        return agent_id not in self._blocked

    def block(self, agent_id):
        self._blocked.add(agent_id)

    def unblock(self, agent_id):
        self._blocked.discard(agent_id)


class _AuditEntry:
    def __init__(self, action, agent_did, detail, outcome="",
                 policy_decision="", data=None):
        self.action = action
        self.agent_did = agent_did
        self.detail = detail
        self.outcome = outcome
        self.policy_decision = policy_decision
        self.data = data or {}
        self.timestamp = time.time()


class MockAuditLog:
    def __init__(self):
        self._entries = []

    def log(self, action, agent_did, detail, outcome="",
            policy_decision="", data=None):
        self._entries.append(_AuditEntry(
            action, agent_did, detail,
            outcome=outcome, policy_decision=policy_decision, data=data))

    def query(self, limit=100):
        return self._entries[:limit]

    def verify_integrity(self):
        return (True, "OK")


class MockFileTrustStore:
    def __init__(self, path=None, auto_save=False):
        self._scores = {}

    def get_trust_score(self, agent_id):
        return self._scores.get(agent_id)

    def get_all_scores(self):
        return dict(self._scores)

    def store_trust_score(self, agent_id, data):
        self._scores[agent_id] = data


class MockAgentDID:
    @staticmethod
    def from_string(s):
        # Real SDK expects a DID format; simple names fall through to
        # the regex fallback in server.py — which is the intended path.
        if not s.startswith("did:"):
            raise ValueError(f"Invalid agent DID format: {s}")
        return MockAgentDID()


class MockAgentBehaviorMonitor:
    def __init__(self, **kwargs):
        pass

    def record_tool_call(self, agent_id, action, success=True):
        pass


class _MockPolicy:
    def __init__(self, name, rules=None):
        self.name = name
        self.rules = rules or []


def _mock_load_policies(directory):
    return [_MockPolicy("test-policy", rules=["r1", "r2"])]


def _register_mock_modules():
    """Inject fake agentmesh modules into sys.modules."""
    def _mod(name, **attrs):
        m = types.ModuleType(name)
        for k, v in attrs.items():
            setattr(m, k, v)
        sys.modules[name] = m
        return m

    _mod("agentmesh", AgentDID=MockAgentDID, AuditLog=MockAuditLog)
    _mod("agentmesh.governance")
    _mod("agentmesh.governance.trust_policy",
         TrustPolicy=_MockPolicy,
         TrustRule=type("TrustRule", (), {}),
         TrustCondition=type("TrustCondition", (), {}),
         TrustDefaults=type("TrustDefaults", (), {}),
         load_policies=_mock_load_policies)
    _mod("agentmesh.governance.policy_evaluator",
         PolicyEvaluator=MockPolicyEvaluator)
    _mod("agentmesh.services")
    _mod("agentmesh.services.rate_limiter",
         RateLimiter=MockRateLimiter)
    _mod("agentmesh.storage")
    _mod("agentmesh.storage.file_trust_store",
         FileTrustStore=MockFileTrustStore)
    _mod("agentmesh.services.behavior_monitor",
         AgentBehaviorMonitor=MockAgentBehaviorMonitor)


_register_mock_modules()

# Configure env vars before the module-level init code runs.
os.environ["AGT_POLICY_DIR"] = "/nonexistent"
os.environ["SANDBOX_NAME"] = "test-sandbox"
os.environ["AGT_PORT"] = "0"
os.environ["AGT_TRUST_DB"] = "/tmp/agt-test/trust_scores.json"

import server  # noqa: E402  (must come after mock registration)

# =====================================================================
# Test suite
# =====================================================================


class TestGovernanceSidecar(unittest.TestCase):
    """Integration tests for every governance sidecar HTTP endpoint."""

    @classmethod
    def setUpClass(cls):
        cls.httpd = HTTPServer(("127.0.0.1", 0), server.GovernanceHandler)
        cls.port = cls.httpd.server_address[1]
        cls.thread = threading.Thread(target=cls.httpd.serve_forever,
                                      daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.httpd.shutdown()
        cls.thread.join(timeout=5)

    def setUp(self):
        """Reset mutable server state between tests."""
        server.rate_limiter = MockRateLimiter()
        server.trust_store = MockFileTrustStore()
        server.audit_log = MockAuditLog()
        server.evaluator = MockPolicyEvaluator()

    # -- helpers -------------------------------------------------------

    def _get(self, path):
        conn = HTTPConnection("127.0.0.1", self.port, timeout=5)
        conn.request("GET", path)
        resp = conn.getresponse()
        return resp.status, json.loads(resp.read())

    def _post(self, path, data):
        payload = json.dumps(data).encode()
        conn = HTTPConnection("127.0.0.1", self.port, timeout=5)
        conn.request("POST", path, body=payload,
                     headers={"Content-Type": "application/json"})
        resp = conn.getresponse()
        return resp.status, json.loads(resp.read())

    # == Health ========================================================

    def test_healthz_returns_200(self):
        status, body = self._get("/healthz")
        self.assertEqual(status, 200)
        self.assertEqual(body["status"], "ok")
        self.assertIn("sandbox", body)

    # == Policy evaluation (POST /evaluate) ============================

    def test_evaluate_allowed_action(self):
        status, body = self._post("/evaluate", {
            "action": "inference:chat_completions:gpt-4.1",
            "agent_id": "agent-alpha",
        })
        self.assertEqual(status, 200)
        self.assertTrue(body["allowed"])
        self.assertEqual(body["action"], "allow")
        self.assertEqual(body["decision"], "allow")
        self.assertFalse(body["rate_limited"])

    def test_evaluate_denied_action(self):
        status, body = self._post("/evaluate", {
            "action": "shell:rm -rf /",
            "agent_id": "agent-alpha",
        })
        self.assertEqual(status, 403)
        self.assertFalse(body["allowed"])
        self.assertEqual(body["action"], "deny")
        self.assertEqual(body["decision"], "deny")

    def test_evaluate_missing_action_returns_400(self):
        status, body = self._post("/evaluate", {"agent_id": "agent-alpha"})
        self.assertEqual(status, 400)
        self.assertIn("error", body)
        self.assertIn("action", body["error"].lower())

    def test_evaluate_empty_action_returns_400(self):
        status, body = self._post("/evaluate", {
            "action": "",
            "agent_id": "agent-alpha",
        })
        self.assertEqual(status, 400)

    def test_evaluate_rate_limited(self):
        server.rate_limiter.block("agent-flood")
        status, body = self._post("/evaluate", {
            "action": "inference:chat_completions:gpt-4.1",
            "agent_id": "agent-flood",
        })
        self.assertEqual(status, 403)
        self.assertFalse(body["allowed"])
        self.assertTrue(body["rate_limited"])
        self.assertIn("rate", body["reason"].lower())

    def test_evaluate_defaults_agent_id_to_sandbox(self):
        """When agent_id is omitted the sandbox name is used."""
        status, body = self._post("/evaluate", {
            "action": "inference:chat_completions:gpt-4.1",
        })
        self.assertEqual(status, 200)
        self.assertTrue(body["allowed"])

    # == Trust CRUD ====================================================

    def test_get_trust_unknown_agent_returns_defaults(self):
        status, body = self._get("/trust/agent-unknown")
        self.assertEqual(status, 200)
        self.assertEqual(body["agent_id"], "agent-unknown")
        self.assertEqual(body["score"], 0)
        self.assertEqual(body["interactions"], 0)
        self.assertEqual(body["tier"], "Anonymous")

    def test_update_trust_first_interaction_allows_up_to_500(self):
        status, body = self._post("/trust", {
            "agent_id": "agent-new",
            "score": 800,
            "interactions": 1,
        })
        self.assertEqual(status, 200)
        self.assertTrue(body["ok"])
        # First interaction: max 500 from score 0
        self.assertLessEqual(body["score"], 500)

    def test_update_trust_first_interaction_exact_500(self):
        status, body = self._post("/trust", {
            "agent_id": "agent-first",
            "score": 500,
            "interactions": 1,
        })
        self.assertEqual(status, 200)
        self.assertEqual(body["score"], 500)

    def test_update_and_get_trust(self):
        self._post("/trust", {
            "agent_id": "agent-beta",
            "score": 400,
            "interactions": 1,
        })
        status, body = self._get("/trust/agent-beta")
        self.assertEqual(status, 200)
        self.assertEqual(body["score"], 400)
        self.assertEqual(body["tier"], "Known")
        self.assertEqual(body["interactions"], 1)

    def test_trust_clamping_subsequent_update(self):
        """After first interaction, delta is clamped to ±200."""
        # First interaction: set to 400
        self._post("/trust", {
            "agent_id": "agent-gamma",
            "score": 400,
            "interactions": 1,
        })
        # Try to jump to 900 — should clamp to 400 + 200 = 600
        status, body = self._post("/trust", {
            "agent_id": "agent-gamma",
            "score": 900,
        })
        self.assertEqual(status, 200)
        self.assertEqual(body["score"], 600)

    def test_trust_clamping_prevents_decrease_beyond_200(self):
        """Score cannot drop more than 200 in a single update."""
        self._post("/trust", {
            "agent_id": "agent-delta",
            "score": 500,
            "interactions": 1,
        })
        # Try to crash to 0 — should clamp to 500 - 200 = 300
        status, body = self._post("/trust", {
            "agent_id": "agent-delta",
            "score": 0,
        })
        self.assertEqual(status, 200)
        self.assertEqual(body["score"], 300)

    def test_trust_score_floor_is_zero(self):
        """Score never goes below 0."""
        # New agent, score starts at 0, request negative is nonsensical
        # but the clamp formula floors at 0.
        status, body = self._post("/trust", {
            "agent_id": "agent-floor",
            "score": -999,
            "interactions": 1,
        })
        self.assertEqual(status, 200)
        self.assertGreaterEqual(body["score"], 0)

    def test_self_trust_update_blocked(self):
        status, body = self._post("/trust", {
            "agent_id": server.SANDBOX,
            "score": 999,
        })
        self.assertEqual(status, 403)
        self.assertIn("error", body)
        self.assertIn("own", body["error"].lower())

    def test_trust_invalid_agent_id_post(self):
        status, body = self._post("/trust", {
            "agent_id": "a",
            "score": 500,
        })
        self.assertEqual(status, 400)
        self.assertIn("error", body)

    def test_trust_missing_agent_id_post(self):
        status, body = self._post("/trust", {"score": 500})
        self.assertEqual(status, 400)
        self.assertIn("error", body)

    def test_get_trust_invalid_agent_id(self):
        status, body = self._get("/trust/a")
        self.assertEqual(status, 400)
        self.assertIn("error", body)

    def test_get_trust_list(self):
        self._post("/trust", {
            "agent_id": "agent-list-a",
            "score": 300,
            "interactions": 1,
        })
        self._post("/trust", {
            "agent_id": "agent-list-b",
            "score": 400,
            "interactions": 2,
        })
        status, body = self._get("/trust")
        self.assertEqual(status, 200)
        self.assertIn("agents", body)
        self.assertEqual(len(body["agents"]), 2)

    def test_trust_tier_mapping(self):
        """Verify tier labels match score thresholds."""
        cases = [
            (0, "Anonymous"),
            (199, "Anonymous"),
            (200, "Observed"),
            (399, "Observed"),
            (400, "Known"),
            (500, "Known"),     # first interaction cap
        ]
        for score, expected_tier in cases:
            self.assertEqual(server._score_to_tier(score), expected_tier,
                             f"score={score}")

    # == Audit =========================================================

    def test_audit_entries_recorded_after_evaluate(self):
        self._post("/evaluate", {
            "action": "inference:chat_completions:gpt-4.1",
            "agent_id": "agent-audit",
        })
        status, body = self._get("/audit")
        self.assertEqual(status, 200)
        self.assertGreater(body["count"], 0)
        entry = body["entries"][0]
        for field in ("action", "agent_id", "decision", "timestamp"):
            self.assertIn(field, entry, f"missing field: {field}")

    def test_audit_records_trust_updates(self):
        self._post("/trust", {
            "agent_id": "agent-audit-trust",
            "score": 400,
            "interactions": 1,
        })
        status, body = self._get("/audit")
        self.assertEqual(status, 200)
        actions = [e["action"] for e in body["entries"]]
        self.assertIn("trust_update", actions)

    def test_audit_verify_returns_valid(self):
        status, body = self._get("/audit/verify")
        self.assertEqual(status, 200)
        self.assertEqual(body["integrity"], "valid")
        self.assertIn("entries", body)
        self.assertIn("sandbox", body)

    def test_audit_verify_includes_entry_count(self):
        # Create a couple of audit entries via evaluate
        self._post("/evaluate", {"action": "shell:ls", "agent_id": "a1"})
        self._post("/evaluate", {"action": "shell:cat", "agent_id": "a2"})
        status, body = self._get("/audit/verify")
        self.assertEqual(status, 200)
        self.assertEqual(body["entries"], 2)

    # == Status ========================================================

    def test_status_returns_expected_fields(self):
        status, body = self._get("/status")
        self.assertEqual(status, 200)
        for field in ("enabled", "sandbox", "policy_loaded",
                      "policy_rules", "audit_entries",
                      "audit_integrity", "known_agents",
                      "trust_threshold", "trust_updates"):
            self.assertIn(field, body, f"missing field: {field}")

    def test_status_reflects_trust_state(self):
        self._post("/trust", {
            "agent_id": "agent-status",
            "score": 300,
            "interactions": 1,
        })
        status, body = self._get("/status")
        self.assertEqual(status, 200)
        self.assertEqual(body["known_agents"], 1)
        self.assertIsInstance(body["trust_states"], list)
        self.assertEqual(body["trust_states"][0]["agent_id"], "agent-status")

    # == 404 ===========================================================

    def test_get_unknown_path_returns_404(self):
        status, body = self._get("/nonexistent")
        self.assertEqual(status, 404)
        self.assertIn("error", body)

    def test_post_unknown_path_returns_404(self):
        # Send without body — server doesn't call _body() for unknown
        # paths, so sending a payload causes a connection reset.
        conn = HTTPConnection("127.0.0.1", self.port, timeout=5)
        conn.request("POST", "/nonexistent",
                     headers={"Content-Length": "0"})
        resp = conn.getresponse()
        self.assertEqual(resp.status, 404)
        body = json.loads(resp.read())
        self.assertIn("error", body)

    # == Edge cases ====================================================

    def test_evaluate_with_extra_context(self):
        """Extra context dict is forwarded without error."""
        status, body = self._post("/evaluate", {
            "action": "inference:chat_completions:gpt-4.1",
            "agent_id": "agent-ctx",
            "context": {"trust_score": 800},
        })
        self.assertEqual(status, 200)
        self.assertTrue(body["allowed"])

    def test_evaluate_returns_matched_rule(self):
        status, body = self._post("/evaluate", {
            "action": "shell:rm -rf /home",
            "agent_id": "agent-rule",
        })
        self.assertEqual(status, 403)
        self.assertIn("matched_rule", body)
        self.assertEqual(body["matched_rule"], "shell-destructive-deny")

    def test_build_context_parsing(self):
        """_build_context splits action strings correctly."""
        ctx = server._build_context("shell:ls -la")
        self.assertEqual(ctx["action"]["category"], "shell")
        self.assertEqual(ctx["action"]["detail"], "ls -la")
        self.assertEqual(ctx["action"]["command"], "ls")
        self.assertEqual(ctx["action"]["full"], "shell:ls -la")

    def test_build_context_no_colon(self):
        ctx = server._build_context("simple")
        self.assertEqual(ctx["action"]["category"], "simple")
        self.assertEqual(ctx["action"]["detail"], "")
        self.assertEqual(ctx["action"]["command"], "")


if __name__ == "__main__":
    unittest.main()
