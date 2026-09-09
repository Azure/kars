# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pure harness checks. These are not the hosted Kubernetes acceptance result."""

import copy
import json
from pathlib import Path
import re
import subprocess
import tempfile
import time
import types
import unittest
from unittest.mock import patch

from sre_authority.common import (
    CLAIM_VERSION, NAMESPACE_UID, RUNTIME, SOURCE_NAME, SOURCE_NS, SOURCE_UID, SYSTEM,
    Harness, assert_claim, assert_denial, authority_failure_site, enrollment_json, printed_object, review_args,
)
from sre_authority.fixtures import seed_control_consumer
from sre_authority.admission import reserved_source_probe
from sre_authority.proxy import MARKER, assert_filtered
from sre_authority.credential_paths import assert_token_type_immutable, assert_watch_result


class Response:
    def __init__(self, code, value):
        self.status_code = code
        self.value = value

    def json(self):
        return self.value


class HarnessTests(unittest.TestCase):
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
        self.assertEqual(h.connectivity_diagnostics(pod), {
            "available": True, "uidMatches1001": True, "serviceExit": 0, "endpointExit": 0,
            "sameNodeControl": {"available": False}})
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
        expression = re.search(r"\| grep -E '([^']+)'", kind)
        self.assertIsNotNone(expression)
        pattern = expression.group(1)
        for path in ("shared/sre_privacy.rs", "shared/another_security_module.rs",
                     "controller/src/sre_authority.rs", "cli/src/lib/sre-authority.ts"):
            with self.subTest(path=path):
                self.assertRegex(path, pattern)
        for path in ("docs/how-to/sre-authority.md", "unrelated/shared/sre_privacy.rs"):
            with self.subTest(path=path):
                self.assertNotRegex(path, pattern)


if __name__ == "__main__":
    unittest.main()
