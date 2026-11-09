# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pure harness checks. These are not the hosted Kubernetes acceptance result."""

import copy
import json
from pathlib import Path
import re
import runpy
import subprocess
import tempfile
import time
import types
import unittest
from unittest.mock import patch

from sre_authority.common import (
    CLAIM_VERSION, NAMESPACE_UID, RUNTIME, SOURCE_NAME, SOURCE_NS, SOURCE_UID, SYSTEM,
    Harness, assert_claim, assert_denial, authority_failure_site, control_consumer_facts,
    enrollment_json, printed_object, review_args,
)
from sre_authority.fixtures import seed_control_consumer
from sre_authority.admission import reserved_source_probe
from sre_authority.proxy import MARKER, assert_filtered, log_reader_facts, pending_proposal, secret_list_wire_facts
from sre_authority.credential_paths import assert_token_type_immutable, assert_watch_result


class Response:
    def __init__(self, code, value):
        self.status_code = code
        self.value = value

    def json(self):
        return self.value


class HarnessTests(unittest.TestCase):
    def test_pending_proposal_uses_the_name_from_created_and_reused_hermes_results(self):
        for newly_created in (True, False):
            calls = []
            persisted = {"metadata": {"name": "sre-action-fixture"},
                         "spec": {"approval": {"state": "Pending"}}}
            def get(kind, name, namespace):
                self.assertEqual((kind, name, namespace), ("karssreaction", "sre-action-fixture", RUNTIME))
                return persisted
            def propose(**kwargs):
                calls.append(kwargs)
                return "sre-action-fixture", newly_created
            with self.subTest(created=newly_created):
                h = types.SimpleNamespace(get=get)
                sre = types.SimpleNamespace(_create_karssreaction_cr=propose)
                self.assertEqual(pending_proposal(h, sre), ("sre-action-fixture", persisted))
                self.assertEqual(len(calls), 1)
                self.assertEqual(calls[0]["action"]["type"], "RolloutRestart")
                persisted["spec"]["approval"]["state"] = "Approved"
                with self.assertRaisesRegex(AssertionError, "did not persist Pending"):
                    pending_proposal(h, sre)

    def test_real_failed_command_relays_sanitized_preparation_facts_without_its_output(self):
        facts = {"step": "data-returned-identity", "source": "cli/src/lib/sre-migration-data.ts",
                 "kind": "KarsTask", "category": "local-check"}
        stderr = "PRIVATE-RESPONSE\nSRE-SCHEMA-PREPARATION " + json.dumps(facts) + "\n"
        with tempfile.TemporaryDirectory() as temporary:
            h = Harness.__new__(Harness)
            h.work = h.root = Path(temporary)
            h.deadline, h.phase = time.monotonic() + 20, "prepare"
            with patch("builtins.print") as printed, self.assertRaisesRegex(AssertionError, "Command failed"):
                h.run(["python3", "-c", f"import sys; sys.stderr.write({stderr!r}); sys.exit(1)"])
            output = " ".join(str(call) for call in printed.call_args_list)
            self.assertIn("SRE-SCHEMA-PREPARATION-FACTS", output)
            self.assertIn("data-returned-identity", output)
            self.assertNotIn("PRIVATE-RESPONSE", output)

    def test_schema_preparation_diagnostics_preserve_only_fixed_child_source_and_shape(self):
        from sre_authority.schema_preparation_diagnostics import schema_preparation_failure
        facts = {"step": "schema-preview-identity", "source": "cli/src/lib/schema-stage.ts",
                 "kind": "KarsBudgetAccount", "category": "local-check",
                 "shape": {"uid": "string", "resourceVersion": "missing", "kind": "string", "apiVersion": "string"}}
        prefix = "SRE-SCHEMA-PREPARATION "
        self.assertEqual(schema_preparation_failure("PRIVATE\n" + prefix + json.dumps(facts) + "\nPRIVATE"), facts)
        for key, value in (("step", "PRIVATE"), ("source", "PRIVATE"), ("kind", "PRIVATE"),
                           ("raw", "PRIVATE"), ("field", "spec/PRIVATE"), ("reason", "PRIVATE"),
                           ("shape", {"uid": "PRIVATE"})):
            with self.subTest(key=key):
                self.assertIsNone(schema_preparation_failure(prefix + json.dumps({**facts, key: value})))
        self.assertIsNone(schema_preparation_failure(prefix + "{invalid PRIVATE"))
        self.assertEqual(schema_preparation_failure((prefix + json.dumps(facts) + "\n") * 2),
                         {"category": "ambiguous"})

    def test_schema_preparation_diagnostic_vocabulary_matches_the_cli_source(self):
        from sre_authority.schema_preparation_diagnostics import FIELDS, KINDS, SOURCES
        root = Path(__file__).resolve().parents[3]
        source = (root / "cli/src/lib/sre-schema-diagnostics.ts").read_text()
        steps = re.search(r"const sources = \{(.*?)\} as const;", source, re.S).group(1)
        self.assertEqual(dict(re.findall(r'"([^"]+)": "([^"]+)"', steps)), SOURCES)
        for name, expected in (("kinds", KINDS), ("fields", FIELDS - {"unrecognized"})):
            literal = re.search(rf"const {name} = new Set\(\[(.*?)\]\);", source, re.S).group(1)
            self.assertEqual(set(re.findall(r'"([^"]+)"', literal)), expected)

    def test_sre_stage_diagnostics_keep_only_fixed_known_phase_names(self):
        from sre_authority.common import command_error_category
        private = "DO-NOT-EMIT-PRIVATE-DATA"
        for stage in ("action-schema-review", "helm-server-dry-run", "core-schema-preparation"):
            value = command_error_category(f"{private}\nSRE-STAGE-FAILURE {stage}\n{private}")
            self.assertEqual(value, "sre-stage:" + stage)
            self.assertNotIn(private, value)
        for value in (f"SRE-STAGE-FAILURE {private}",
                      f"SRE-STAGE-FAILURE action-schema-review {private}"):
            self.assertEqual(command_error_category(value), "unclassified")
        self.assertEqual(command_error_category(
            "SRE-STAGE-FAILURE action-schema-review\nSRE-STAGE-FAILURE helm-server-dry-run"),
            "sre-stage:ambiguous")

    def test_discovery_failure_retains_only_known_api_types_not_resource_names(self):
        from sre_authority.common import command_error_category
        private = "DO-NOT-EMIT-PRIVATE-DATA"
        error = f'resource mapping not found for name: "{private}": no matches for kind "ServiceMonitor" in version "monitoring.coreos.com/v1"'
        self.assertEqual(command_error_category(error), "api-discovery:ServiceMonitor@monitoring.coreos.com/v1")
        self.assertEqual(command_error_category(error + "\n" + error), command_error_category(error))
        self.assertNotIn(private, command_error_category(error))
        for kind, version in ((private, "monitoring.coreos.com/v1"), ("ServiceMonitor", private),
                              ("Deployment", "kars.azure.com/v1alpha1")):
            self.assertEqual(command_error_category(f'no matches for kind "{kind}" in version "{version}"'),
                             "api-discovery:unclassified-type")
        self.assertEqual(command_error_category('no matches for kind "KarsBudgetAccount" in version "kars.azure.com/v1alpha1"'),
                         "api-discovery:KarsBudgetAccount@kars.azure.com/v1alpha1")

    def test_hermes_runtime_assertion_verifies_exact_pin_and_runtime_without_blanket_standin_acceptance(self):
        helper = Path(__file__).resolve().parents[1] / "sre-authority.sh"
        cases = [
            ("kars-sandbox-e2e:dev", "kars-sandbox-e2e:dev", "Hermes", 0),
            ("wrong:latest", "kars-sandbox-e2e:dev", "Hermes", 1),
            ("kars-sandbox-e2e:dev", "", "Hermes", 1),
            ("custom/runtime:latest", "custom/runtime:latest", "Hermes", 0),
            ("kars-runtime-hermes:latest", "", "Hermes", 0),
            ("kars-runtime-hermes:latest", "", "BYO", 1),
            ("", "", "Hermes", 1),
        ]
        for image, configured, runtime, expected in cases:
            result = subprocess.run(
                ["bash", "-c", 'source "$1"; sre_hermes_image_matches "$2" "$3" "$4"',
                 "hermes-image-test", str(helper), image, configured, runtime],
                capture_output=True, text=True, timeout=5, check=False)
            with self.subTest(image=image, configured=configured, runtime=runtime):
                self.assertEqual(result.returncode, expected)

    def test_log_reader_diagnostics_never_echo_log_text_or_error_body(self):
        root = Path(__file__).resolve().parents[3]
        facts = log_reader_facts(root, {"error": "406 Not Acceptable",
            "body": json.dumps({"kind": "Status", "reason": "SREProxyDenied",
                               "message": "Kubernetes rejected the diagnostic/proposal request", "data": MARKER})})
        self.assertEqual(facts["httpStatus"], 406)
        self.assertTrue(facts["hasError"])
        self.assertIn("responseSite", facts)
        self.assertNotIn(MARKER, json.dumps(facts))
        facts = log_reader_facts(root, {"logs": "sre-standin-alive " + MARKER})
        self.assertTrue(facts["standinMarker"])
        self.assertNotIn(MARKER, json.dumps(facts))

    def test_native_secret_list_wire_facts_publish_no_synthetic_secret_fields(self):
        value = {"kind": "SecretList", "items": [{"metadata": {"uid": "fixture", "annotations": {"copy": MARKER}},
                                                "data": {"token": MARKER}}]}
        facts = secret_list_wire_facts(value, "fixture")
        self.assertEqual(facts, {"envelopeKind": "SecretList", "itemHasKind": False,
                                 "itemHasApiVersion": False, "itemMetadataObject": True})
        self.assertNotIn(MARKER, json.dumps(facts))
        with self.assertRaises(AssertionError):
            secret_list_wire_facts(value, "other")
        with self.assertRaises(AssertionError):
            secret_list_wire_facts({"kind": "List", "items": []}, "fixture")

    def test_firewall_diagnostic_requires_owned_node_before_readonly_namespace_entry(self):
        h = Harness.__new__(Harness)
        calls = []
        h.run = lambda args, **_kwargs: calls.append(args) or "foreign-cluster"
        pod = {"metadata": {"uid": "pod-uid"}, "spec": {"nodeName": "kars-e2e-worker"}}
        with self.assertRaises(AssertionError):
            h.firewall_diagnostics(pod)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][:3], ["docker", "inspect", "kars-e2e-worker"])
        pod["spec"]["hostNetwork"] = True
        with self.assertRaises(AssertionError):
            h.firewall_diagnostics(pod)
        self.assertEqual(len(calls), 1)

    def test_connectivity_diagnostic_is_uid_fenced_nonprivileged_and_has_no_credentials(self):
        h = Harness.__new__(Harness)
        pod = {"metadata": {"name": "sre-probe", "uid": "pod-uid", "resourceVersion": "17"},
               "spec": {"automountServiceAccountToken": False},
               "status": {"ephemeralContainerStatuses": [{"name": "sre-e2e-network-diagnostic",
                                                          "state": {"terminated": {"exitCode": 0}}}]}}
        def get(kind, _name, _namespace):
            if kind == "service":
                return {"spec": {"clusterIP": "10.96.0.1", "ports": [{"name": "https", "port": 443}]}}
            if kind == "endpoints":
                return {"subsets": [{"addresses": [{"ip": "172.18.0.2"}],
                                     "ports": [{"name": "https", "port": 6443}]}]}
            return pod
        writes = []
        h.get = get
        h.api = lambda method, path, **kwargs: writes.append((method, path, kwargs))
        h.poll = lambda _label, predicate, **_kwargs: self.assertTrue(predicate())
        h.k = lambda *_args: '{"available":true,"uidMatches1001":true,"serviceExit":0,"endpointExit":0}'
        h.control_connectivity_diagnostics = lambda *_args: {"available": False}
        h.policy_connectivity_diagnostics = lambda *_args, **_kwargs: {"available": False}
        self.assertEqual(h.connectivity_diagnostics(pod), {
            "available": True, "uidMatches1001": True, "serviceExit": 0, "endpointExit": 0,
            "sameNodeControl": {"available": False},
            "guardControls": {variant: {"available": False} for variant in ("full", "filter-only", "legacy-full")},
            "policyControl": {"available": False}, "denyAllPolicyControl": {"available": False},
            "apiEndpointPolicyControl": {"available": False}})
        self.assertEqual(len(writes), 1)
        method, path, args = writes[0]
        self.assertEqual(method, "PATCH")
        self.assertTrue(path.endswith("/ephemeralcontainers"))
        body = args["body"]
        self.assertEqual(body["metadata"], {"uid": "pod-uid", "resourceVersion": "17"})
        probe = body["spec"]["ephemeralContainers"][0]
        self.assertEqual(probe["securityContext"]["runAsUser"], 1001)
        self.assertFalse(probe["securityContext"]["allowPrivilegeEscalation"])
        self.assertEqual(probe["securityContext"]["capabilities"], {"drop": ["ALL"]})
        for field in ("targetContainerName", "volumeMounts", "env", "envFrom"):
            self.assertNotIn(field, probe)
        for key, value in (("automountServiceAccountToken", True), ("shareProcessNamespace", True)):
            old = copy.deepcopy(pod["spec"])
            pod["spec"][key] = value
            with self.assertRaises(AssertionError):
                h.connectivity_diagnostics(pod)
            pod["spec"] = old
        self.assertEqual(len(writes), 1)

    def test_connectivity_result_rejects_arbitrary_output_or_untyped_fields(self):
        for value in ({"available": True}, {"available": 1},
                      {"available": False, "data": MARKER},
                      {"available": True, "uidMatches1001": True, "serviceExit": "0", "endpointExit": 0}):
            with self.assertRaises(AssertionError):
                Harness.parse_connectivity_result(json.dumps(value))
        self.assertEqual(Harness.parse_connectivity_result('{"available":false}'), {"available": False})

    def test_same_node_control_creates_only_unprivileged_owned_pod_and_uid_fenced_cleanup(self):
        h = Harness.__new__(Harness)
        pod = {"spec": {"nodeName": "sandbox-worker", "securityContext": {"runAsNonRoot": True}}}
        container = {"name": "network", "image": "kars-sandbox-e2e:dev",
                     "securityContext": {"runAsUser": 1001, "capabilities": {"drop": ["ALL"]}}}
        seen, deleted = [], []
        current = {"metadata": {"uid": "control-uid", "resourceVersion": "7"}, "status": {"phase": "Succeeded"}}
        h.create = lambda obj: seen.append(obj) or current
        h.get = lambda *_args: current
        h.poll = lambda _label, predicate, **_kwargs: self.assertTrue(predicate())
        h.k = lambda *_args: '{"available":true,"uidMatches1001":true,"serviceExit":0,"endpointExit":0}'
        h.api = lambda method, path, **kwargs: deleted.append((method, path, kwargs))
        h.control_connectivity_diagnostics(pod, container)
        self.assertEqual(len(seen), 1)
        self.assertEqual(seen[0]["spec"]["nodeName"], "sandbox-worker")
        self.assertFalse(seen[0]["spec"]["automountServiceAccountToken"])
        self.assertNotIn("shareProcessNamespace", seen[0]["spec"])
        self.assertNotIn("volumes", seen[0]["spec"])
        self.assertEqual(deleted[0][0], "DELETE")
        self.assertEqual(deleted[0][2]["body"]["preconditions"], {"uid": "control-uid", "resourceVersion": "7"})

    def test_endpoint_candidate_keeps_full_guard_and_tests_separate_agent_uid(self):
        h = Harness.__new__(Harness)
        h.root = Path(__file__).resolve().parents[3]
        pod = {"spec": {"nodeName": "sandbox-worker"}}
        container = {"name": "network", "image": "kars-sandbox-e2e:dev",
                     "command": ["sh", "-c", "uid test = 1001 uidMatches1001"],
                     "securityContext": {"runAsUser": 1001, "capabilities": {"drop": ["ALL"]}}}
        seen = []
        current = {"metadata": {"uid": "control", "resourceVersion": "1"}, "status": {"phase": "Succeeded"}}
        h.create = lambda obj: seen.append(obj) or current
        h.get = lambda *_args: current
        h.poll = lambda _label, predicate, **_kwargs: self.assertTrue(predicate())
        h.k = lambda *args: (
            '{"available":true,"uidMatches1000":true,"serviceExit":1,"endpointExit":124}'
            if "agent-network" in args else '{"available":true,"uidMatches1001":true,"serviceExit":0,"endpointExit":0}')
        h.api = lambda *_args, **_kwargs: None
        with patch("sre_authority.network_diagnostics.guard_variant", return_value={"name": "guard"}) as guard:
            result = h.control_connectivity_diagnostics(pod, container, policy=True, api_endpoint=True)
        guard.assert_called_once_with(h.root, pod, "full")
        actual = seen[0]["spec"]["containers"]
        self.assertEqual([obj["securityContext"]["runAsUser"] for obj in actual], [1001, 1000])
        self.assertIn("= 1000", actual[1]["command"][2])
        self.assertIn("uidMatches1000", actual[1]["command"][2])
        self.assertTrue(actual[0]["command"][2].startswith("sleep 10\n"))
        self.assertTrue(result["agentUid1000"]["uidMatches1000"])
        self.assertNotEqual(result["agentUid1000"]["endpointExit"], 0)
        for obj in actual:
            self.assertNotIn("volumeMounts", obj)
            self.assertNotIn("envFrom", obj)

    def test_policy_comparison_targets_only_owned_control_and_never_changes_source_policy(self):
        h = Harness.__new__(Harness)
        source = {"spec": {"podSelector": {}, "policyTypes": ["Egress"],
                          "egress": [{"to": [{"ipBlock": {"cidr": "10.96.0.1/32"}}],
                                      "ports": [{"port": 443, "protocol": "TCP"}]}]}}
        before = copy.deepcopy(source)
        seen, deleted = [], []
        own = {"metadata": {"uid": "policy-uid", "resourceVersion": "3"}}
        h.get = lambda kind, name, namespace: source if name == "sandbox-policy" else own
        h.create = lambda obj: seen.append(obj) or own
        h.control_connectivity_diagnostics = lambda *_args, **kwargs: {"policy": kwargs["policy"]}
        h.api = lambda method, path, **kwargs: deleted.append((method, path, kwargs))
        self.assertEqual(h.policy_connectivity_diagnostics({}, {}), {"policy": True})
        self.assertEqual(source, before)
        h.get = lambda kind, name, namespace: (
            {"subsets": [{"addresses": [{"ip": "172.18.0.2"}], "ports": [{"name": "https", "port": 6443}]}]}
            if kind == "endpoints" else source if name == "sandbox-policy" else own)
        h.policy_connectivity_diagnostics({}, {}, api_endpoint=True)
        self.assertEqual(seen[1]["spec"]["egress"], source["spec"]["egress"] + [
            {"to": [{"ipBlock": {"cidr": "172.18.0.2/32"}}], "ports": [{"protocol": "TCP", "port": 6443}]}])
        self.assertEqual(source, before)
        self.assertEqual(seen[0]["spec"]["egress"], source["spec"]["egress"])
        self.assertEqual(seen[0]["spec"]["podSelector"],
                         {"matchLabels": {"kars.azure.com/e2e-policy-control": "true"}})
        self.assertEqual(deleted[0][2]["body"]["preconditions"], {"uid": "policy-uid", "resourceVersion": "3"})
        h.policy_connectivity_diagnostics({}, {}, deny_all=True)
        self.assertEqual(seen[2]["spec"], {"policyTypes": ["Egress"], "egress": [],
            "podSelector": {"matchLabels": {"kars.azure.com/e2e-policy-control": "deny-all"}}})
        self.assertEqual(source, before)

    def test_blocked_authority_diagnostics_report_source_coordinates_not_status_detail(self):
        root = Path(__file__).resolve().parents[3]
        cases = [
            ("Reviewed SRE consumer was changed or replaced", "migration.rs", "controller-rejection", None),
            ("Private SRE role definition has excessive or unsupported authority", "bindings.rs", "controller-rejection", None),
            ("Retire reviewed SRE ClusterRoleBinding: Kubernetes status 403", "bindings.rs", "kubernetes-status", 403),
            ("Inspect reserved SRE token identity: Kubernetes transport/serialization failure",
             "credential_guard.rs", "kubernetes-transport", None),
        ]
        for detail, source, category, code in cases:
            with self.subTest(source=source):
                result = authority_failure_site(root, detail)
                self.assertTrue(result["source"].endswith(source))
                self.assertGreater(result["line"], 0)
                self.assertEqual(result["category"], category)
                self.assertEqual(result.get("httpStatus"), code)
                self.assertNotIn(detail, json.dumps(result))
                for unsafe in (detail + MARKER, MARKER + detail, detail + "\n" + MARKER):
                    self.assertEqual(authority_failure_site(root, unsafe), {"category": "unclassified"})
        for unsafe in (None, {}, "", MARKER, "private body: Kubernetes status 403",
                       "Retire reviewed SRE ClusterRoleBinding: Kubernetes status 403 " + MARKER):
            self.assertEqual(authority_failure_site(root, unsafe), {"category": "unclassified"})

    def test_blocked_authority_diagnostics_classify_first_install_and_public_policy_failures(self):
        root = Path(__file__).resolve().parents[3]
        for detail in (
            "First-install funding configuration is invalid",
            "First-install funding requires the governed broker",
            "First-install funding catalog is unavailable",
            "First-install SRE intent, identity, catalog or funding authorization failed verification",
        ):
            with self.subTest(detail=detail):
                result = authority_failure_site(root, detail)
                self.assertEqual(result["source"], "controller/src/inference_budget/first_install.rs")
                self.assertNotIn(detail, json.dumps(result))
        from sre_authority.common import POLICIES
        for name in POLICIES:
            detail = f"SRE admission policy kars-sre-{name} is not observed, type-checked, and enforced; no privilege may be issued"
            result = authority_failure_site(root, detail)
            self.assertEqual(result["source"], "controller/src/sre_authority/admission.rs")
            self.assertNotIn(detail, json.dumps(result))
        for name in (MARKER, "kars-sre-unknown", "kars-sre-source-authority " + MARKER):
            detail = f"SRE admission policy {name} is not observed, type-checked, and enforced; no privilege may be issued"
            self.assertEqual(authority_failure_site(root, detail), {"category": "unclassified"})

    def test_blocked_authority_diagnostics_cover_shared_issuance_and_retirement_errors(self):
        root = Path(__file__).resolve().parents[3]
        cases = [
            ("Private capability is unqualified; regenerate and apply the reviewed grant activation",
             "controller/src/private_activation.rs"),
            ("Private target namespace requires reviewed grant activation before issuance or reuse",
             "controller/src/private_activation/runtime.rs"),
            ("SRE CA issuance failed", "controller/src/providers/sre_tls.rs"),
            ("Discover Cilium policy API: Kubernetes status 403",
             "controller/src/reconciler/sre_egress.rs"),
            ("Installed Cilium policy API has an unsupported resource contract",
             "controller/src/reconciler/sre_egress.rs"),
        ]
        for detail, source in cases:
            with self.subTest(source=source):
                result = authority_failure_site(root, detail)
                self.assertEqual(result.get("source"), source)
                self.assertGreater(result["line"], 0)
                self.assertNotIn(detail, json.dumps(result))
                for unsafe in (MARKER + detail, detail + MARKER, detail + "\n" + MARKER):
                    self.assertEqual(authority_failure_site(root, unsafe), {"category": "unclassified"})
        primary = "Reviewed SRE consumer was changed or replaced"
        cleanup = "Discover Cilium policy API: Kubernetes status 403"
        result = authority_failure_site(root, primary + "; owned API policy retirement failed: " + cleanup)
        self.assertEqual(result["category"], "controller-rejection-chain")
        self.assertEqual(result["failures"], [
            {"operation": "authority", **authority_failure_site(root, primary)},
            {"operation": "API policy retirement", **authority_failure_site(root, cleanup)},
        ])

    def test_blocked_authority_diagnostics_preserve_all_checked_cleanup_failure_sites(self):
        root = Path(__file__).resolve().parents[3]
        primary = "Reviewed SRE consumer was changed or replaced"
        cleanups = [
            ("private authority revocation", "Retire private SRE binding: Kubernetes status 403"),
            ("API policy retirement", "Read retiring SRE API policy: Kubernetes transport/serialization failure"),
            ("private credential retirement", "Retire private SRE identity: Kubernetes status 409"),
        ]
        for mask in range(1, 8):
            selected = [entry for index, entry in enumerate(cleanups) if mask & (1 << index)]
            detail = primary + "".join(f"; owned {operation} failed: {message}" for operation, message in selected)
            with self.subTest(mask=mask):
                result = authority_failure_site(root, detail)
                self.assertEqual(result["category"], "controller-rejection-chain")
                self.assertEqual(result["failures"], [
                    {"operation": "authority", **authority_failure_site(root, primary)},
                    *[{"operation": operation, **authority_failure_site(root, message)} for operation, message in selected],
                ])
                for _, message in selected:
                    self.assertNotIn(message, json.dumps(result))
                for unsafe in (MARKER + detail, detail + MARKER, detail + "\n" + MARKER):
                    self.assertEqual(authority_failure_site(root, unsafe), {"category": "unclassified"})

    def test_blocked_authority_diagnostics_reject_unknown_or_malformed_cleanup_chains(self):
        root = Path(__file__).resolve().parents[3]
        primary = "Reviewed SRE consumer was changed or replaced"
        rejection = "Retire private SRE binding: Kubernetes status 403"
        first = f"; owned private authority revocation failed: {rejection}"
        second = f"; owned API policy retirement failed: {rejection}"
        for detail in (
            primary + first + first, primary + second + first,
            primary + first * 4, primary + "; owned unknown failed: " + rejection,
            primary + "; owned private authority revocation failed: " + MARKER,
            primary + "; owned private authority revocation failed: ",
            primary + first + "; owned API policy retirement failed: " + MARKER,
            MARKER + first, "x" * 8193,
        ):
            with self.subTest(detail=detail[:100]):
                self.assertEqual(authority_failure_site(root, detail), {"category": "unclassified"})

    def test_exit_trap_collects_safe_failure_evidence_before_cleanup_and_preserves_exit(self):
        root = Path(__file__).resolve().parents[3]
        source = (root / "tests/e2e/run.sh").read_text()
        teardown = "teardown() {" + source.split("teardown() {", 1)[1].split("\n}", 1)[0] + "\n}"
        for status, diagnostic_status, cleanup_status in ((0, 0, 0), (7, 0, 0), (7, 3, 0), (7, 3, 4)):
            with self.subTest(status=status, diagnostics=diagnostic_status, cleanup=cleanup_status), \
                    tempfile.TemporaryDirectory() as directory:
                config = Path(directory) / "kubeconfig"
                config.write_text("disposable-test-config")
                script = '''set -euo pipefail
ROOT_DIR=/fixture
CLUSTER_NAME=kars-e2e
E2E_KUBECONFIG="$1"
python3() { printf 'capture:%s\\n' "$*"; return CAPTURE_STATUS; }
sre_authority_cleanup() { echo authority-cleanup; return CLEANUP_STATUS; }
kind() { printf 'kind:%s\\n' "$*"; }
info() { :; }
warn() { echo bounded-capture-unavailable; }
'''.replace("CAPTURE_STATUS", str(diagnostic_status)).replace("CLEANUP_STATUS", str(cleanup_status))
                result = subprocess.run(["bash", "-c", script + teardown + "\ntrap teardown EXIT\nexit " + str(status),
                                         "test", str(config)], capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertFalse(config.exists())
                events = result.stdout.splitlines()
                cleanup_index = events.index("authority-cleanup")
                self.assertLess(cleanup_index, events.index("kind:delete cluster --name kars-e2e"))
                captures = [index for index, event in enumerate(events) if event.startswith("capture:")]
                if status:
                    self.assertEqual(len(captures), 1)
                    self.assertLess(captures[0], cleanup_index)
                    self.assertEqual(events[captures[0]], "capture:-m sre_authority.bootstrap_probe --diagnostics-only")
                    self.assertEqual("bounded-capture-unavailable" in events, diagnostic_status != 0)
                else:
                    self.assertEqual(captures, [])
        workflow = (root / ".github/workflows/ci.yml").read_text()
        self.assertFalse("Collect cluster diagnostics on failure" in workflow,
                         "Post-teardown collection cannot observe the deleted cluster")
        self.assertIn("e2e-sre-schema-diag/bootstrap-*.json", workflow)

    def test_control_consumer_writes_keep_manager_and_identity_fences(self):
        root = Path(__file__).resolve().parents[3]
        migration = (root / "controller/src/sre_authority/migration.rs").read_text()
        credentials = (root / "controller/src/reconciler/governed_services/credentials.rs").read_text()
        rotation = migration.split("async fn rotate_owned_control_credentials(", 1)[1]
        quarantine = credentials.split("async fn quarantine(", 1)[1].split("async fn checked_epoch(", 1)[0]
        stop = migration.split("async fn stop_legacy_consumer(", 1)[1].split("pub(crate) fn controller_managed(", 1)[0]
        canonical = "field_manager:Some(crate::field_managers::CLAWSANDBOX.into())"
        for source, writer, count in ((rotation, "deployments.patch(name,&params,", 3),
                                      (quarantine, ".patch(name,&params,", 1),
                                      (stop, 'api.patch("sre",&params,', 1)):
            compact = re.sub(r"\s+", "", source)
            writes = compact.split(writer)[1:]
            self.assertEqual(len(writes), count)
            for write in writes:
                payload = write.split(".await", 1)[0]
                self.assertIn("&Patch::Merge(json!", payload)
                self.assertIn('"uid":deployment.metadata.uid', payload)
                self.assertIn('"resourceVersion":deployment.metadata.resource_version', payload)
            if source != stop:
                self.assertIn(canonical, compact)
        self.assertLess(rotation.index("if !controller_managed(&deployment, name)"),
                        rotation.index("let params = PatchParams"))
        self.assertLess(quarantine.index("review_consumer(client, namespace, name).await?"),
                        quarantine.index("let params = PatchParams"))
        self.assertIn('field_manager:controller_managed(&deployment,"sre").then(||crate::field_managers::CLAWSANDBOX.into())',
                      re.sub(r"\s+", "", stop))
        self.assertIn("validate_consumer(client, reg).await?", stop)

    def test_control_consumer_diagnostics_project_ownership_without_workload_payloads(self):
        epoch = "a" * 64
        obj = {"metadata": {"name": "consumer", "namespace": "kars-consumer", "uid": "consumer-uid",
            "resourceVersion": "42", "generation": 3, "deletionTimestamp": None,
            "labels": {"kars.azure.com/sandbox": "consumer", "private": MARKER},
            "annotations": {"kars.azure.com/sre-control-drain": epoch + ":3", "private": MARKER},
            "ownerReferences": [{"apiVersion": "apps/v1", "kind": "Deployment", "name": "owner",
                                 "uid": "owner-uid", "controller": True, "private": MARKER}],
            "managedFields": [{"manager": "kars-controller/karssandbox", "operation": "Apply",
                "apiVersion": "apps/v1", "fieldsV1": {"f:spec": {"f:replicas": {}, "f:template": {
                    "f:spec": {"f:containers": {'k:{"name":"' + MARKER + '"}': {}}}}}}},
                {"manager": "unknown", "operation": "Update", "fieldsV1": {"f:spec": {"f:replicas": {}}}}]},
            "spec": {"replicas": 0, "template": {"spec": {"containers": [{"env": [{"value": MARKER}]}]}}},
            "status": {"observedGeneration": 3, "replicas": 0, "conditions": [{"message": MARKER}]},
            "data": {"control-token": MARKER}}
        before = copy.deepcopy(obj)
        facts = control_consumer_facts(obj)
        self.assertEqual(facts["uid"], "consumer-uid")
        self.assertEqual(facts["resourceVersion"], "42")
        self.assertEqual(facts["generation"], 3)
        self.assertEqual(facts["replicas"], 0)
        self.assertEqual(facts["status"]["observedGeneration"], 3)
        self.assertEqual(facts["drain"], {"present": True, "valid": True, "epoch": epoch, "restoreReplicas": 3})
        self.assertEqual(facts["ownerReferences"][0]["uid"], "owner-uid")
        self.assertEqual(facts["managedFields"][0]["fieldPaths"],
                         ["f:spec", "f:spec/f:replicas", "f:spec/f:template"])
        self.assertEqual(facts["managedFields"][1]["fieldPaths"], ["f:spec", "f:spec/f:replicas"])
        self.assertNotIn(MARKER, json.dumps(facts))
        self.assertEqual(obj, before)
        obj["metadata"]["managedFields"][0]["fieldsV1"] = {"f:metadata": {"f:annotations": {}}}
        self.assertEqual(control_consumer_facts(obj)["managedFields"][0]["fieldPaths"], [])

    def test_control_consumer_diagnostics_never_echo_malformed_drain_markers(self):
        self.assertEqual(control_consumer_facts({})["drain"], {"present": False, "valid": False})
        for marker in (None, {}, MARKER, "a" * 64 + ":0", "a" * 64 + ":-1",
                       "a" * 64 + ":2147483648", "a" * 64 + ":1:" + MARKER):
            with self.subTest(marker=marker):
                facts = control_consumer_facts({"metadata": {"annotations": {
                    "kars.azure.com/sre-control-drain": marker}}})
                self.assertEqual(facts["drain"], {"present": True, "valid": False})
                self.assertNotIn(MARKER, json.dumps(facts))

    def test_control_consumer_capture_requests_managed_fields_and_survives_missing_or_failed_reads(self):
        from sre_authority.fixtures import CONTROL, CONTROL_NS
        for first in ("", RuntimeError(MARKER), "{malformed " + MARKER):
            h = Harness.__new__(Harness)
            responses = iter([first, json.dumps({"metadata": {"name": "sre", "namespace": RUNTIME,
                "uid": "live-sre", "resourceVersion": "2", "managedFields": [{"manager": "kube-client",
                    "operation": "Update", "fieldsV1": {"f:spec": {"f:replicas": {}}}}]},
                "spec": {"replicas": 0, "template": {"private": MARKER}}})])
            calls = []
            def read(*args, **kwargs):
                calls.append((args, kwargs))
                value = next(responses)
                if isinstance(value, Exception):
                    raise value
                return value
            h.k = read
            with patch("builtins.print") as printed:
                h.control_consumer_diagnostics()
            self.assertEqual(len(calls), 2)
            for (name, namespace), (args, kwargs) in zip(((CONTROL, CONTROL_NS), ("sre", RUNTIME)), calls):
                self.assertEqual(args, ("get", "deployment", name, "-n", namespace, "--ignore-not-found",
                                        "--show-managed-fields=true", "-o", "json"))
                self.assertEqual(kwargs, {"timeout": 10})
            facts = [json.loads(call.args[1]) for call in printed.call_args_list]
            self.assertEqual(facts[0]["availability"], "absent" if first == "" else "unavailable")
            self.assertEqual(facts[1]["uid"], "live-sre")
            self.assertEqual(facts[1]["replicas"], 0)
            self.assertEqual(facts[1]["managedFields"][0]["manager"], "kube-client")
            self.assertEqual(facts[1]["managedFields"][0]["fieldPaths"], ["f:spec", "f:spec/f:replicas"])
            self.assertNotIn(MARKER, json.dumps(facts))

    def test_failure_diagnostics_retain_blocked_registration_and_capture_control_consumers(self):
        h = Harness.__new__(Harness)
        h.root = Path(__file__).resolve().parents[3]
        h.deadline = time.monotonic() + 30
        h.get = lambda kind, *_args: ({"metadata": {"uid": "registration", "generation": 3},
            "status": {"phase": "Blocked", "observedGeneration": 3,
                       "detail": "Control credential consumer is not controller-owned"}}
            if kind == "karssreregistrations.kars.azure.com" else None)
        h.k = lambda *_args, **_kwargs: '{"items": []}'
        with patch("sre_authority.common.collect_stage_workloads"), \
                patch.object(h, "control_consumer_diagnostics") as capture, patch("builtins.print") as printed:
            h.diagnostics()
        capture.assert_called_once_with()
        facts = json.loads(printed.call_args_list[0].args[1])
        self.assertEqual(facts["phase"], "Blocked")
        self.assertEqual(facts["generation"], 3)
        self.assertEqual(facts["authorityFailure"]["source"], "controller/src/sre_authority/migration.rs")

    def test_control_consumer_fixture_satisfies_required_sandbox_fields(self):
        captured = []
        class Captured(Exception):
            pass
        def capture(obj, **_kwargs):
            captured.append(obj)
            raise Captured
        with self.assertRaises(Captured):
            seed_control_consumer(types.SimpleNamespace(create=capture))
        root = Path(__file__).resolve().parents[3]
        schema = (root / "deploy/helm/kars/templates/crd.yaml").read_text()
        spec = schema.split("            spec:\n", 1)[1]
        required = json.loads(re.search(r"required: (\[[^\n]+\])", spec).group(1))
        for source in (captured[0], reserved_source_probe()):
            with self.subTest(name=source["metadata"]["name"]):
                self.assertTrue(set(required).issubset(source["spec"]))
                self.assertEqual(source["spec"]["inferenceRef"], {"name": "sre-inference"})
                self.assertEqual(source["spec"]["runtime"]["kind"], "BYO")
                self.assertNotIn("agent", source["spec"])

    def test_failed_command_reports_only_source_exit_and_allowlisted_category(self):
        root = Path(__file__).resolve().parent
        with tempfile.TemporaryDirectory(prefix=".harness-unit-", dir=root) as folder:
            harness = Harness.__new__(Harness)
            harness.work = harness.root = Path(folder)
            harness.deadline = time.monotonic() + 30
            harness.phase = "prepare"
            process = types.SimpleNamespace(returncode=1, communicate=lambda **_kwargs: (
                MARKER, f"Error from server (Invalid): {MARKER} secret/header/kubeconfig body"))
            with patch("sre_authority.common.subprocess.Popen", return_value=process):
                with self.assertRaises(AssertionError) as failure:
                    harness.run(["kubectl", "--token", MARKER], data=MARKER)
            message = str(failure.exception)
            self.assertIn("harness_test.py:test_failed_command_reports_only_source_exit_and_allowlisted_category:", message)
            self.assertIn("exit=1; category=Invalid", message)
            for forbidden in (MARKER, "--token", "secret/header/kubeconfig body", str(harness.root)):
                self.assertNotIn(forbidden, message)

    def test_secret_type_rejection_is_specific_and_not_counted_as_vap_denial(self):
        cause = {"field": "type", "reason": "FieldValueInvalid",
                 "message": 'Invalid value: "Opaque": field is immutable'}
        body = {"kind": "Status", "reason": "Invalid", "details": {"causes": [cause]}}
        assert_token_type_immutable(Response(422, body), "type update")
        with self.assertRaises(AssertionError):
            assert_denial(Response(422, body), "type update", "kars-sre-no-legacy-tokens")
        for code in (200, 400, 403, 404, 409, 500):
            with self.subTest(code=code), self.assertRaises(AssertionError):
                assert_token_type_immutable(Response(code, body), "type update")
        for invalid in (
            {**body, "reason": "Forbidden"},
            {**body, "details": {"causes": []}},
            {**body, "details": {"causes": [{**cause, "field": "metadata.annotations"}]}},
            {**body, "details": {"causes": [{**cause, "message": "different validation failure"}]}},
        ):
            with self.subTest(body=invalid), self.assertRaises(AssertionError):
                assert_token_type_immutable(Response(422, invalid), "type update")

    def test_denials_require_real_forbidden_and_the_intended_policy(self):
        valid = {"kind": "Status", "reason": "Forbidden", "message": "kars-sre-private-mounts denied"}
        assert_denial(Response(403, valid), "probe", "kars-sre-private-mounts")
        for code in (200, 201, 400, 404, 409, 422, 500):
            with self.assertRaises(AssertionError):
                assert_denial(Response(code, valid), "probe", "kars-sre-private-mounts")
        with self.assertRaises(AssertionError):
            assert_denial(Response(403, {**valid, "message": "ordinary RBAC denied"}), "probe", "kars-sre-private-mounts")

    def test_secret_watch_never_confuses_empty_success_with_authorization_denial(self):
        forbidden = Response(403, {"kind": "Status", "reason": "Forbidden"})
        assert_watch_result(forbidden, False, False)
        assert_watch_result(Response(200, {}), True, True)
        for observed in (True, False):
            with self.assertRaises(AssertionError):
                assert_watch_result(Response(200, {}), observed, False)
        with self.assertRaises(AssertionError):
            assert_watch_result(Response(200, {}), False, True)
        with self.assertRaises(AssertionError):
            assert_watch_result(Response(404, {"kind": "Status", "reason": "NotFound"}), False, False)

    def test_review_arguments_pin_every_uid_resource_version_and_consumer(self):
        spec = {
            "controller": {}, "sandbox": {"uid": "source-uid"}, "runtimeNamespace": {"uid": "namespace-uid"},
            "legacyBindings": [
                {"kind": "ClusterRoleBinding", "name": "reader", "uid": "binding-a", "resourceVersion": "10"},
                {"kind": "RoleBinding", "namespace": "work", "name": "writer", "uid": "binding-b", "resourceVersion": "20"},
            ],
            "legacyConsumer": {"uid": "consumer-uid", "resourceVersion": "30"},
        }
        args = review_args(spec, {"metadata": {"uid": "registration-uid", "resourceVersion": "40"}})
        self.assertIn("ClusterRoleBinding//reader=binding-a@10", args)
        self.assertIn("RoleBinding/work/writer=binding-b@20", args)
        self.assertIn("consumer-uid@30", args)
        self.assertEqual(args[-4:], ["--registration-uid", "registration-uid", "--resource-version", "40"])
        self.assertEqual(enrollment_json(json.dumps(spec) + "\n--binding 'review'\n"), spec)
        with self.assertRaises(AssertionError):
            enrollment_json('{"kind":"Status","reason":"Forbidden"}')
        self.assertEqual(printed_object("Preview:\n" + json.dumps({"spec": spec}) + "\n"),
                         {"spec": spec})

    def test_namespace_proof_independently_checks_full_claim_and_backlink(self):
        source = {"metadata": {"name": "sre", "namespace": SYSTEM, "uid": "source-uid", "resourceVersion": "1",
                               "annotations": {NAMESPACE_UID: "namespace-uid"}}}
        namespace = {"metadata": {"name": RUNTIME, "uid": "namespace-uid", "resourceVersion": "2",
            "annotations": {CLAIM_VERSION: "v1", SOURCE_NS: SYSTEM, SOURCE_NAME: "sre", SOURCE_UID: "source-uid"}}}
        core = {"metadata": {"uid": "core-uid"}}
        assert_claim(source, namespace, core, "core-uid")
        for key in (CLAIM_VERSION, SOURCE_NS, SOURCE_NAME, SOURCE_UID):
            bad = copy.deepcopy(namespace)
            bad["metadata"]["annotations"][key] = "wrong"
            with self.assertRaises(AssertionError):
                assert_claim(source, bad, core, "core-uid")
        bad = copy.deepcopy(source)
        bad["metadata"]["annotations"][NAMESPACE_UID] = "old-namespace-uid"
        with self.assertRaises(AssertionError):
            assert_claim(bad, namespace, core, "core-uid")
        with self.assertRaises(AssertionError):
            assert_claim(source, namespace, core, "old-core-uid")

    def test_real_api_status_handler_rejects_errors_without_echoing_secret_bodies(self):
        harness = Harness.__new__(Harness)
        seen = []
        def request(method, path, **kwargs):
            seen.append((method, path, kwargs))
            return Response(403, {"message": MARKER})
        harness.client = lambda _user: types.SimpleNamespace(request=request)
        with self.assertRaises(AssertionError) as failure:
            harness.api("PATCH", "/api/v1/namespaces/example/secrets/example",
                        body={"stringData": {"token": MARKER}}, status=200)
        self.assertIn("HTTP 403", str(failure.exception))
        self.assertNotIn(MARKER, str(failure.exception))
        self.assertEqual(seen[0][2]["headers"]["Content-Type"], "application/merge-patch+json")
        self.assertEqual(harness.api("GET", "/missing").status_code, 403)

    def test_secret_projection_assertions_detect_all_copy_channels(self):
        safe = {"kind": "Secret", "metadata": {"name": "example"},
                "data": {"operator-token": "", "password": ""}}
        assert_filtered(safe)
        for key in ("labels", "annotations", "managedFields"):
            with self.assertRaises(AssertionError):
                assert_filtered({**safe, "metadata": {"name": "example", key: {"copy": MARKER}}})
        with self.assertRaises(AssertionError):
            assert_filtered({**safe, "stringData": {"copy": MARKER}})
        with self.assertRaises(AssertionError):
            assert_filtered({**safe, "data": {"operator-token": MARKER, "password": ""}})

    def test_token_probe_cannot_retain_admin_client_certificate(self):
        root = Path(__file__).resolve().parent
        with tempfile.TemporaryDirectory(prefix=".harness-unit-", dir=root) as folder:
            harness = Harness.__new__(Harness)
            harness.work = Path(folder)
            harness.config = {
                "clusters": [{"name": "kind-kars-e2e", "cluster": {"server": "https://127.0.0.1", "certificate-authority-data": "Y2E="}}],
                "users": [{"name": "admin", "user": {"client-certificate-data": "PRIVATE", "client-key-data": "PRIVATE"}}],
            }
            harness.clients = {}
            harness.server = "https://127.0.0.1"
            harness.token_config("opaque", "not-a-real-token")
            config = json.loads((harness.work / "opaque.json").read_text())
            self.assertEqual(config["users"][0]["user"], {"token": "not-a-real-token"})

            class Context:
                def __init__(self):
                    self.loaded = False

                def load_cert_chain(self, *_args):
                    self.loaded = True

            contexts = []
            def context_factory(**_kwargs):
                context = Context()
                contexts.append(context)
                return context
            clients = []
            def client_factory(**kwargs):
                client = types.SimpleNamespace(**kwargs)
                clients.append(client)
                return client
            harness.httpx = types.SimpleNamespace(Client=client_factory)
            with patch("sre_authority.common.ssl.create_default_context", side_effect=context_factory):
                harness.client("opaque")
                harness.client("admin")
            self.assertFalse(contexts[0].loaded)
            self.assertTrue(contexts[1].loaded)
            self.assertIsNot(contexts[0], contexts[1])
            self.assertNotIn("cert", clients[0].__dict__)
            self.assertEqual(clients[0].headers["Authorization"], "Bearer not-a-real-token")
            self.assertNotIn("Authorization", clients[1].headers)

    def test_legacy_seed_precedes_new_install_and_other_acceptance(self):
        root = Path(__file__).resolve().parents[3]
        main = (root / "tests/e2e/run.sh").read_text().split("main() {", 1)[1]
        self.assertLess(main.index("prepare_sre_authority_legacy"), main.index("install_crds"))
        self.assertLess(main.index("test_sre_authority_migration"), main.index("test_create_sandbox"))
        fixture = (root / "tests/e2e/sre_authority/fixtures.py").read_text()
        self.assertIn("8b206065608593667a40665b3f48225ef9ce278d", fixture)
        self.assertIn("https://github.com/Azure/kars.git", fixture)
        self.assertNotIn("--force-conflicts", fixture)
        self.assertNotIn("--validate=false", fixture)
        self.assertLess(fixture.index('h.state["legacy_review_before_guards"]'),
                        fixture.index("seed_privacy_gaps(h)"))
        self.assertLess(fixture.index("seed_privacy_gaps(h)"),
                        fixture.index('h.cli("authority", "stage"'))
        from sre_authority.common import POLICIES
        self.assertIn("no-legacy-tokens", POLICIES)

    def test_final_upgrade_preserves_saved_values_only_for_prepared_legacy_release(self):
        root = Path(__file__).resolve().parents[3]
        source = (root / "tests/e2e/run.sh").read_text()
        install = "install_crds() {" + source.split("install_crds() {", 1)[1].split("\nteardown()", 1)[0]
        script = '''
set -eu
ROOT_DIR="$1"
SRE_LEGACY_PREPARED="$2"
source "$ROOT_DIR/tests/e2e/sre-authority.sh"
info() { :; }
helm() {
    if [ "$1" = version ]; then printf '%s\\n' "$HELM_VERSION";
    else printf '%s\\0' "$@"; fi
}
''' + install + '\nHELM_VERSION="$3"; install_crds\n'
        for prepared in ("0", "1"):
            for version, wait in (("v3.16.4", "--wait"), ("v4.2.4", "--wait=legacy")):
                with self.subTest(prepared=prepared, version=version):
                    result = subprocess.run(
                        ["bash", "-c", script, "saved-helm-values-test", str(root), prepared, version],
                        capture_output=True, check=True, timeout=5)
                    args = result.stdout.decode().rstrip("\0").split("\0")
                    self.assertEqual(args[:3], ["upgrade", "--install", "kars"])
                    self.assertEqual(args.count("--reset-then-reuse-values"), int(prepared))
                    self.assertNotIn("--reset-values", args)
                    self.assertNotIn("--reuse-values", args)
                    self.assertNotIn("sre.enabled=false", args)
                    self.assertEqual("sre.authorityStage=true" in args, prepared == "1")
                    self.assertIn(wait if prepared == "1" else "--wait", args)
                    self.assertEqual(args[args.index("--timeout") + 1], "5m")
                    self.assertEqual(args[args.index("--values") + 1], str(root / ".e2e-addon-values.json"))
                    self.assertIn("controller.replicas=1", args)
                    self.assertIn("inferenceRouter.image.pullPolicy=Never", args)

    def test_staged_setup_waits_for_builtins_without_skipping_migration_readiness(self):
        root = Path(__file__).resolve().parents[3]
        helper = root / "tests/e2e/sre-authority.sh"
        for version, expected in (("v3.16.4", "--wait"), ("v4.2.4", "--wait=legacy")):
            with self.subTest(version=version):
                result = subprocess.run(
                    ["bash", "-c", 'source "$1"; sre_migration_helm_wait_arg "$2"',
                     "sre-wait-test", str(helper), version],
                    capture_output=True, text=True, check=True, timeout=5,
                )
                self.assertEqual(result.stdout.strip(), expected)
        for version in ("v5.0.0", "unexpected"):
            result = subprocess.run(
                ["bash", "-c", 'source "$1"; sre_migration_helm_wait_arg "$2"',
                 "sre-wait-test", str(helper), version],
                capture_output=True, text=True, check=False, timeout=5,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")
        script = (root / "tests/e2e/run.sh").read_text()
        install = script.split("install_crds() {", 1)[1].split("\nteardown()", 1)[0]
        self.assertIn("local helm_wait_arg=--wait", install)
        staged = install.split('if [ "$SRE_LEGACY_PREPARED" = "1" ]; then', 1)[1].split("\n    fi", 1)[0]
        self.assertIn('sre_migration_helm_wait_arg "$helm_version"', staged)
        self.assertIn('"$helm_wait_arg" --timeout 5m', install)
        main = script.split("main() {", 1)[1]
        self.assertIn("    install_crds\n", main)
        self.assertIn("    test_sre_authority_migration\n", main)
        self.assertLess(main.index("install_crds"), main.index("test_sre_authority_migration"))
        self.assertLess(main.index("test_sre_authority_migration"), main.index("test_create_sandbox"))

    def test_kind_prerequisites_do_not_remove_the_rust_httpx_test_step(self):
        root = Path(__file__).resolve().parents[3]
        workflow = (root / ".github/workflows/ci.yml").read_text()
        self.assertIn("Legacy Hermes HTTPS client test dependency", workflow)
        kind = workflow.split("  e2e-kind:", 1)[1].split("  bench-regression:", 1)[0]
        self.assertIn("actions/setup-node@820762786026740c76f36085b0efc47a31fe5020", kind)
        self.assertIn("npm ci && npm run build", kind)
        self.assertIn("'httpx==0.28.1'", kind)
        self.assertGreaterEqual(workflow.count("'httpx==0.28.1'"), 2)

    def test_kind_runtime_filter_includes_all_shared_security_modules(self):
        root = Path(__file__).resolve().parents[3]
        workflow = (root / ".github/workflows/ci.yml").read_text()
        kind = workflow.split("  e2e-kind:", 1)[1].split("  bench-regression:", 1)[0]
        self.assertIn("ci/bridge_contracts.py --output run --core-only", kind)
        required = runpy.run_path(str(root / "ci/bridge_contracts.py"))["native_required"]
        for path in ("shared/sre_privacy.rs", "shared/another_security_module.rs",
                     "controller/src/sre_authority.rs", "cli/src/lib/sre-authority.ts",
                     "cli/src/lib/private-activation.ts", "runtimes/openclaw/skills/SKILL.md",
                     "unrelated/shared/sre_privacy.rs"):
            with self.subTest(path=path):
                self.assertTrue(required([path], core_only=True))
        for path in ("docs/how-to/sre-authority.md", "bridge/web/src/page.tsx"):
            with self.subTest(path=path):
                self.assertFalse(required([path], core_only=True))


if __name__ == "__main__":
    unittest.main()
