# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pure redaction/orchestration checks, not native readiness or CNI evidence."""

import contextlib
import io
import json
from pathlib import Path
import time
import unittest
from unittest.mock import patch

from sre_authority import readiness_diagnostics as diag
from sre_authority.common import CLAIM_VERSION, NAMESPACE_UID, SOURCE_NAME, SOURCE_NS, SOURCE_UID

PRIVATE = "DO-NOT-LOG-PRIVATE-SECRET-ENV-ARGV-OR-API-BODY"
ROOT = Path(__file__).resolve().parents[3]


def event(message, target=diag.BACKEND, **fields):
    return json.dumps({"timestamp": "2026-09-10T00:00:00Z", "level": "WARN", "target": target,
                       "fields": {"message": message, **fields}, "span": {"private": PRIVATE}})


def annotations():
    return {diag.OWNER: "registration-uid", diag.EPOCH: "registration-uid:1",
            diag.PRIVATE_UID: "credential-uid", "private": PRIVATE}


def owner(kind, name, uid):
    return {"apiVersion": "apps/v1", "kind": kind, "name": name, "uid": uid, "controller": True}


class FixtureHarness:
    def __init__(self):
        self.root, self.phase = ROOT, "legacy"
        self.server = "https://127.0.0.1:12345"
        self.config = {"current-context": diag.CONTEXT,
            "contexts": [{"name": diag.CONTEXT, "context": {"cluster": "fixture"}}],
            "clusters": [{"name": "fixture", "cluster": {"server": self.server}}]}
        self.state = {"system_uid": "system-uid"}
        self.deadline = time.monotonic() + 120
        self.calls, self.log_reads = [], 0
        self.after_log = None
        self.log = event("SRE authority transport failure", stage="registration",
                         timed_out=True, connect_error=False) + "\n" + PRIVATE
        pod = {"metadata": {"name": "sre-abc-def", "namespace": diag.RUNTIME, "uid": "pod-uid",
                           "annotations": annotations(), "labels": {"kars.azure.com/sandbox": "sre"},
                           "ownerReferences": [owner("ReplicaSet", "sre-abc", "replica-uid")]},
               "spec": {"containers": [{"name": "inference-router", "image": "kars-inference-router:e2e",
                        "env": [{"name": "PRIVATE", "value": PRIVATE}], "args": [PRIVATE]}]},
               "status": {"containerStatuses": [{"name": "inference-router", "ready": False,
                          "state": {"running": {}}, "restartCount": 0}]}}
        self.objects = {
            ("karssreregistrations.kars.azure.com", "canonical", None): {
                "metadata": {"uid": "registration-uid", "generation": 1},
                "spec": {"sandbox": {"name": "sre", "namespace": diag.SYSTEM, "uid": "source-uid"},
                         "runtimeNamespace": {"name": diag.RUNTIME, "uid": "namespace-uid"}},
                "status": {"phase": "Ready", "observedGeneration": 1, "privacyEpoch": "registration-uid:1"}},
            ("karssandbox", "sre", diag.SYSTEM): {"metadata": {
                "name": "sre", "namespace": diag.SYSTEM, "uid": "source-uid", "resourceVersion": "1",
                "annotations": {NAMESPACE_UID: "namespace-uid"}}},
            ("namespace", diag.RUNTIME, None): {"metadata": {
                "name": diag.RUNTIME, "uid": "namespace-uid", "resourceVersion": "2",
                "annotations": {CLAIM_VERSION: "v1", SOURCE_NS: diag.SYSTEM,
                                SOURCE_NAME: "sre", SOURCE_UID: "source-uid"}}},
            ("namespace", diag.SYSTEM, None): {"metadata": {"uid": "system-uid"}},
            ("deployment", "sre", diag.RUNTIME): {"metadata": {"uid": "deployment-uid"},
                "spec": {"template": {"metadata": {"annotations": annotations()}}}},
            ("pods", None, diag.RUNTIME): {"items": [pod]},
            ("pod", "sre-abc-def", diag.RUNTIME): pod,
            ("replicaset", "sre-abc", diag.RUNTIME): {"metadata": {"uid": "replica-uid",
                "ownerReferences": [owner("Deployment", "sre", "deployment-uid")]}},
            ("networkpolicy", "sandbox-policy", diag.RUNTIME): {"spec": {"egress": [{
                "to": [{"ipBlock": {"cidr": "10.96.0.1/32"}}], "ports": [{"port": 443, "protocol": "TCP"}]}]}},
            ("service", "kubernetes", "default"): {"metadata": {
                "name": "kubernetes", "namespace": "default", "uid": "service-uid"},
                "spec": {"clusterIP": "10.96.0.1", "ports": [{"name": "https", "port": 443}]}},
            ("endpoints", "kubernetes", "default"): {"metadata": {
                "name": "kubernetes", "namespace": "default", "uid": "endpoints-uid"},
                "subsets": [{"addresses": [{"ip": "172.18.0.2"}], "ports": [{"name": "https", "port": 6443}]}]},
        }

    def k(self, *args, **kwargs):
        self.calls.append((args, kwargs))
        if args[0] == "logs":
            self.log_reads += 1
            if self.after_log:
                self.after_log()
            return self.log
        if args[0] != "get":
            raise AssertionError("Diagnostic attempted a non-read-only command")
        kind = args[1]
        name = args[2] if len(args) > 2 and not args[2].startswith("-") else None
        namespace = args[args.index("-n") + 1] if "-n" in args else None
        obj = self.objects.get((kind, name, namespace))
        return json.dumps(obj) if obj else ""


def collect(h):
    output = io.StringIO()
    with patch.object(Path, "mkdir"), patch.object(Path, "write_text") as write, \
         patch.object(Path, "chmod"), contextlib.redirect_stdout(output):
        result = diag.collect(h)
    return result, output.getvalue(), write.call_args.args[0]


class ReadinessDiagnosticsTests(unittest.TestCase):
    def test_only_exact_trusted_json_categories_status_and_duration_cross_boundary(self):
        text = "\n".join([
            PRIVATE,
            event("SRE authority transport failure", stage="registration", timed_out=True, connect_error=True),
            event("SRE authority request denied", stage="privacy-review", http_status=403),
            event("SRE readiness authority rejected", target=diag.PROXY, category="registration-stale"),
            event("SRE readiness authority slow", target=diag.PROXY, elapsed_seconds=20, authorized=False),
        ])
        self.assertEqual(diag.router_facts(text), [
            diag.fact("registration", "transport-timeout"),
            diag.fact("privacy-review", "http-denied", httpStatus=403),
            diag.fact("readiness", "registration-stale"),
            diag.fact("readiness", "authority-slow-rejected", durationSeconds=20),
        ])
        self.assertNotIn(PRIVATE, json.dumps(diag.router_facts(text)))

    def test_untrusted_messages_fields_targets_or_types_do_not_become_facts(self):
        values = [
            event("SRE authority request denied", target="agent", stage="registration", http_status=403),
            event("prefix SRE authority request denied", stage="registration", http_status=403),
            event("SRE authority request denied", stage=PRIVATE, http_status=403),
            event("SRE authority request denied", stage=[], http_status=403),
            event("SRE authority request denied", stage="registration", http_status=PRIVATE),
            event("SRE authority request denied", stage="registration", http_status=True),
            event("SRE authority request denied", stage="registration", http_status=700),
            event("SRE authority request denied", stage="registration", http_status=403, body=PRIVATE),
            event("SRE readiness authority rejected", target=diag.PROXY, category=PRIVATE),
            event("SRE readiness authority rejected", target=diag.PROXY, category=[]),
            event("SRE readiness authority slow", target=diag.PROXY, elapsed_seconds=1.5, authorized=False),
            event("SRE readiness authority slow", target=diag.PROXY, elapsed_seconds=1000, authorized=False),
            json.dumps({"fields": {"message": "SRE authority request denied", "stage": "registration", "http_status": 403}}),
            f'WARN {diag.BACKEND}: SRE authority request denied stage="registration" http_status=403',
        ]
        for text in values:
            with self.subTest(text=text):
                self.assertEqual(diag.router_facts(text), [])
        for stage, category, fields in [(PRIVATE, "current", {}), ("collection", PRIVATE, {}),
                                        ("collection", "current", {"body": PRIVATE})]:
            with self.assertRaises(diag.Unavailable):
                diag.fact(stage, category, **fields)

    def test_parser_bounds_and_deduplicates_events(self):
        single = event("SRE authority request denied", stage="source", http_status=403)
        self.assertEqual(diag.router_facts("\n".join([single] * 40)), [
            diag.fact("source", "http-denied", httpStatus=403)])
        with self.assertRaises(diag.Unavailable):
            diag.router_facts("x" * (diag.MAX_LOG_BYTES + 1))
        many = "\n".join(event("SRE authority request denied", stage="source", http_status=code)
                         for code in range(400, 450))
        self.assertEqual(len(diag.router_facts(many)), 32)

    def test_service_only_and_endpoint_rules_are_observations_not_cni_claims(self):
        h = FixtureHarness()
        policy = h.objects[("networkpolicy", "sandbox-policy", diag.RUNTIME)]
        service = h.objects[("service", "kubernetes", "default")]
        endpoints = h.objects[("endpoints", "kubernetes", "default")]
        self.assertEqual(diag.exact_api_rules(policy, service, endpoints), [
            diag.fact("policy-api-service", "exact-rule-present"),
            diag.fact("policy-api-endpoint", "exact-rules-absent")])
        policy["spec"]["egress"].append({"to": [{"ipBlock": {"cidr": "172.18.0.2/32"}}],
                                         "ports": [{"port": 6443, "protocol": "TCP"}]})
        self.assertEqual(diag.exact_api_rules(policy, service, endpoints)[1]["category"], "exact-rules-present")
        endpoints["subsets"][0]["addresses"].append({"ip": "172.18.0.3"})
        self.assertEqual(diag.exact_api_rules(policy, service, endpoints)[1]["category"], "exact-rules-partial")
        endpoints["subsets"][0]["addresses"] = [{"ip": "127.0.0.1"}]
        with self.assertRaises(diag.Unavailable):
            diag.exact_api_rules(policy, service, endpoints)

    def test_collector_is_read_only_bounded_and_never_persists_private_material(self):
        h = FixtureHarness()
        report, output, saved = collect(h)
        self.assertIn(diag.fact("registration", "transport-timeout"), report["facts"])
        self.assertIn(diag.fact("pod-router", "running-not-ready"), report["facts"])
        self.assertIn(diag.fact("policy-api-endpoint", "exact-rules-absent"), report["facts"])
        self.assertEqual(report["facts"][-1], diag.fact("collection", "complete"))
        self.assertNotIn(PRIVATE, output + saved)
        self.assertNotIn("pod-uid", output + saved)
        self.assertLess(len(saved), 16384)
        self.assertTrue(all(set(row) <= {"stage", "category", "httpStatus", "durationSeconds"}
                            for row in report["facts"]))
        for args, kwargs in h.calls:
            self.assertIn(args[0], ("get", "logs"))
            self.assertLessEqual(kwargs["timeout"], 8)
            self.assertNotIn("secret", args)
            self.assertNotIn("exec", args)
        logs = [args for args, _ in h.calls if args[0] == "logs"]
        self.assertEqual(len(logs), 1)
        self.assertIn("--tail=150", logs[0])
        self.assertIn("--limit-bytes=32768", logs[0])
        self.assertIn("--since=120s", logs[0])
        self.assertEqual(h.calls[h.calls.index(next(c for c in h.calls if c[0][0] == "logs")) + 1][0][1], "pod")

    def test_context_guard_rejects_cached_non_kind_or_non_loopback_configuration(self):
        for mutate in (
            lambda h: h.config.update({"current-context": "h100"}),
            lambda h: h.config["contexts"][0].update(name="h100"),
            lambda h: h.config["clusters"][0]["cluster"].update(server="https://h100.example"),
            lambda h: setattr(h, "server", "https://other.example"),
        ):
            h = FixtureHarness()
            mutate(h)
            report, output, _ = collect(h)
            self.assertEqual(h.calls, [])
            self.assertEqual(report["facts"], [diag.fact("fixture-identity", "refused")])
            self.assertNotIn("h100", output)

    def test_recreated_pod_discards_logs_before_parsing_or_output(self):
        h = FixtureHarness()
        h.after_log = lambda: h.objects[("pod", "sre-abc-def", diag.RUNTIME)]["metadata"].update(uid="replacement")
        report, output, saved = collect(h)
        self.assertEqual(report["facts"][-1], diag.fact("pod-fence", "identity-changed"))
        self.assertNotIn(diag.fact("registration", "transport-timeout"), report["facts"])
        self.assertNotIn(PRIVATE, output + saved)

    def test_foreign_owner_image_or_namespace_cannot_supply_trusted_router_logs(self):
        for key, mutate in (
            (("replicaset", "sre-abc", diag.RUNTIME), lambda obj: obj["metadata"].update(uid="foreign")),
            (("deployment", "sre", diag.RUNTIME), lambda obj: obj["metadata"].update(uid="foreign")),
            (("pod", "sre-abc-def", diag.RUNTIME), lambda obj: obj["spec"]["containers"][0].update(image=PRIVATE)),
            (("namespace", diag.RUNTIME, None), lambda obj: obj["metadata"].update(uid="foreign")),
        ):
            h = FixtureHarness()
            mutate(h.objects[key])
            report, output, saved = collect(h)
            self.assertEqual(h.log_reads, 0)
            self.assertNotEqual(report["facts"][-1], diag.fact("collection", "complete"))
            self.assertNotIn(PRIVATE, output + saved)

    def test_deadline_and_command_failures_are_not_readiness_success(self):
        h = FixtureHarness()
        with patch.object(diag.time, "monotonic", side_effect=[0, 41]):
            report, _, _ = collect(h)
        self.assertEqual(h.calls, [])
        self.assertEqual(report["facts"][-1], diag.fact("collection", "deadline"))
        h = FixtureHarness()
        h.k = lambda *_a, **_k: (_ for _ in ()).throw(AssertionError(PRIVATE))
        report, output, saved = collect(h)
        self.assertEqual(report["facts"][-1], diag.fact("collection", "unavailable"))
        self.assertNotIn(PRIVATE, output + saved)

    def test_same_uid_with_changed_image_is_rejected_after_log_read(self):
        h = FixtureHarness()
        h.after_log = lambda: h.objects[("pod", "sre-abc-def", diag.RUNTIME)]["spec"]["containers"][0].update(image=PRIVATE)
        report, output, saved = collect(h)
        self.assertEqual(report["facts"][-1], diag.fact("pod-fence", "refused"))
        self.assertNotIn(diag.fact("registration", "transport-timeout"), report["facts"])
        self.assertNotIn(PRIVATE, output + saved)

    def test_runtime_failures_still_propagate_and_collection_precedes_teardown(self):
        source = (ROOT / "tests/e2e/sre-authority.py").read_text()
        self.assertLess(source.index("harness.diagnostics()"), source.index("harness.close()"))
        self.assertIn("return 1", source.split("except Exception as error:", 1)[1])
        common = (ROOT / "tests/e2e/sre_authority/common.py").read_text().split("    def diagnostics(self):", 1)[1]
        self.assertLess(common.index("collect(self)"), common.index('self.get(kind, name, namespace)'))
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("make test-e2e", workflow)
        self.assertIn("composed-sre-readiness-${{ github.run_id }}", workflow)
        section = workflow.split("- name: Upload bounded composed SRE readiness evidence", 1)[1].split("- name:", 1)[0]
        self.assertIn("if: always() && steps.paths.outputs.run == 'true'", section)
        self.assertIn("path: e2e-sre-schema-diag/sre-readiness-*.json", section)
        self.assertNotIn("continue-on-error", section)
        self.assertNotIn("router.log", section)

    def test_rust_category_projection_and_shared_tls_probe_contract_remain_explicit(self):
        source = (ROOT / "inference-router/src/sre_proxy/mod.rs").read_text()
        for category in diag.REJECTIONS:
            self.assertIn(f'"{category}"', source)
        self.assertIn("pub(crate) use crate::private_tls::{Listener, tls_from_pem};", source)
        probe = source.split("pub async fn readiness_probe()", 1)[1]
        self.assertIn('join("agent-ca.crt")', probe)
        self.assertIn(".no_proxy()", probe)
        self.assertIn(".timeout(std::time::Duration::from_secs(3))", probe)
        self.assertIn('https://127.0.0.1:{PORT}/readyz', probe)


if __name__ == "__main__":
    unittest.main()
