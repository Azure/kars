"""Controlled formats and failure checkpoints, not live Cilium qualification."""

import json
from pathlib import Path
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from native_api import Failure
import observer_cilium_diagnostics as cilium
import observer_network_diagnostics as network
import test_observer_network_diagnostics as fixtures


class CiliumBaselineWitnessTests(unittest.TestCase):
    def setUp(self):
        self.api = fixtures.NetworkFixture()
        self.setup = SimpleNamespace(admin=self.api, cluster={"server": self.api.server})

    def collect(self):
        return fixtures.ObserverNetworkTests.collect(self)

    def assert_stopped(self, result, stage):
        self.assertFalse(result["available"])
        self.assertFalse(result["policyCreated"])
        self.assertEqual(result["baselineStoppingStage"], stage)
        witness = result["baselineSnapshot"]
        self.assertFalse(witness["complete"])
        self.assertTrue(witness["networkValidated"])
        self.assertEqual(witness["lastStage"], stage)
        self.assertEqual(witness["validatedNetworkFacts"]["pods"][0]["identity"]["uid"], "agent-pod-uid")
        self.assertNotIn("cilium", witness["validatedNetworkFacts"])
        self.assertNotIn("canary", json.dumps(result))
        self.assertEqual(self.api.created, [])
        self.assertEqual(self.api.deleted, [])
        return witness

    def test_nil_empty_and_unknown_modes_expose_rendering_without_becoming_defaults(self):
        original = cilium.read_projection
        for value, syntax in (("", "empty"), ("<nil>", "go-nil"), ("<no value>", "go-no-value"),
                              ("private-mode-canary", "invalid-json")):
            with self.subTest(syntax=syntax):
                self.setUp()
                self.api.read_projection = original
                raw = f"{value}\ttrue\ttrue\n\n".encode()
                with patch.object(cilium.subprocess, "run", return_value=SimpleNamespace(returncode=0, stdout=raw)):
                    result = self.collect()
                witness = self.assert_stopped(result, "config_mode")
                self.assertEqual(witness["failureKind"], "format")
                self.assertEqual(witness["checks"]["config_mode"]["modeSyntax"], syntax)
                self.assertEqual(witness["checks"]["config_fields"]["fieldCount"], 3)
                self.assertEqual(witness["checks"]["config_exec"]["exitStatus"], 0)
                self.assertEqual(witness["checks"]["config_exec"]["byteCount"], len(raw))
                self.assertEqual(witness["checks"]["config_framing"]["lineCount"], 1)
                self.assertIn("observedConfigMap", witness)
                self.assertNotIn("observedEffectiveConfig", witness)

    def test_framing_exit_and_timeout_failures_have_distinct_fixed_facts(self):
        original = cilium.read_projection
        for code, raw, stage in ((0, b"", "config_framing"), (0, b"one\ntwo", "config_framing"),
                                 (1, b"", "config_exec")):
            self.setUp()
            self.api.read_projection = original
            with patch.object(cilium.subprocess, "run", return_value=SimpleNamespace(returncode=code, stdout=raw)):
                result = self.collect()
            witness = self.assert_stopped(result, stage)
            self.assertEqual(witness["checks"]["config_exec"]["exitStatus"], code)
            self.assertFalse(witness["checks"]["config_exec"]["timedOut"])
        self.setUp()
        self.api.read_projection = original
        with patch.object(cilium.subprocess, "run", side_effect=subprocess.TimeoutExpired("private-canary", 12)):
            result = self.collect()
        witness = self.assert_stopped(result, "config_exec")
        self.assertTrue(witness["checks"]["config_exec"]["timedOut"])
        self.assertEqual(witness["failureKind"], "command-timeout")

    def test_actual_api_status_is_retained_without_error_objects_or_headers(self):
        for path, stage in ((cilium.CONFIG, "configmap_read"), (cilium.DAEMONSET, "daemonset_read"),
                             (fixtures.CEP, "endpoint_read")):
            for code in (403, 404, 503):
                self.setUp()
                original = self.api.request
                def request(method, selected, body=None, expected=(200,)):
                    if method == "GET" and selected == path:
                        return code, {"message": "private-body-canary", "headers": {"token": "private-header-canary"}}
                    return original(method, selected, body, expected)
                with patch.object(self.api, "request", side_effect=request):
                    result = self.collect()
                witness = self.assert_stopped(result, stage)
                self.assertEqual(witness["checks"][stage]["httpStatus"], code)

    def test_identity_image_and_owner_assumptions_report_the_actual_false_check(self):
        cases = [
            (lambda: self.api.objects[cilium.CONFIG]["metadata"].pop("resourceVersion"),
             "configmap_identity", "resourceVersionPresent"),
            (lambda: self.api.objects[fixtures.CILIUM_POD]["spec"].update(serviceAccountName="foreign"),
             "agent_constraints", "serviceAccountMatches"),
            (lambda: self.api.objects[fixtures.CILIUM_POD]["spec"]["containers"][0].update(image="private-image-canary"),
             "agent_constraints", "imageExpectedVersion"),
            (lambda: self.api.objects[fixtures.CEP]["metadata"]["ownerReferences"][0].update(uid="foreign"),
             "endpoint_owner", "podUidMatches"),
            (lambda: self.api.objects[fixtures.CEP]["status"]["identity"].update(id=0),
             "endpoint_fields", "securityIdValid"),
            (lambda: self.api.objects[fixtures.CEP]["status"]["networking"].update(node="172.18.0.99"),
             "endpoint_pins", "nodeMatchesPod"),
        ]
        for mutate, stage, check in cases:
            with self.subTest(stage=stage, check=check):
                self.setUp()
                mutate()
                witness = self.assert_stopped(self.collect(), stage)
                self.assertFalse(witness["checks"][stage][check])

    def test_missing_shapes_and_endpoint_projection_mismatches_are_not_generic(self):
        self.api.objects[fixtures.CEP]["status"]["identity"] = None
        witness = self.assert_stopped(self.collect(), "endpoint_fields")
        self.assertEqual(witness["failureKind"], "shape")
        self.assertEqual(witness["checks"]["endpoint_fields"]["identityShape"], "null")
        self.setUp()
        original = self.api.read_projection
        def read(agent, endpoint_id=None, witness=None):
            fields = original(agent, endpoint_id)
            if endpoint_id is not None:
                fields[6] = "private-pod-name-canary"
            return fields
        self.api.read_projection = read
        witness = self.assert_stopped(self.collect(), "endpoint_projection_pins")
        self.assertFalse(witness["checks"]["endpoint_projection_pins"]["podFieldMatches"])
        self.assertIn("observedEffectiveConfig", witness)

    def test_late_failure_keeps_only_previously_validated_network_facts(self):
        original = network.snapshot
        reads = 0
        def snapshot(*args, **kwargs):
            nonlocal reads
            reads += 1
            if reads == 2:
                raise Failure("private-late-canary")
            return original(*args, **kwargs)
        with patch.object(network, "snapshot", side_effect=snapshot):
            witness = self.assert_stopped(self.collect(), "network_recheck")
        self.assertNotIn("cilium", witness["validatedNetworkFacts"])

    def test_checkpoint_surface_rejects_arbitrary_keys_values_and_stages(self):
        for stage, facts in (("private-stage-canary", {}), ("config_fields", {"private-key-canary": True}),
                             ("config_fields", {"modeSyntax": "private-value-canary"}),
                             ("config_fields", {"objectShape": {"body": "private-canary"}})):
            witness = {}
            with self.assertRaises(Failure):
                cilium.checkpoint(witness, stage, **facts)
            self.assertEqual(witness, {})

    def test_every_required_config_token_must_survive_projection(self):
        for index in range(3):
            self.setUp()
            self.api.effective_config[index] = ""
            result = self.collect()
            stage = "config_mode" if index == 0 else "config_fields"
            witness = self.assert_stopped(result, stage)
            self.assertFalse(witness["checks"]["config_fields"]["requiredTokensPresent"])
        for token in ("null", "[]"):
            witness = {}
            self.assertEqual(cilium.effective_configuration([token, "true", "true"], witness)
                             ["policyCIDRMatchMode"], [])
            self.assertTrue(witness["checks"]["config_fields"]["requiredTokensPresent"])

    @unittest.skipUnless(shutil.which("kubectl"), "Existing kubectl JSONPath engine is unavailable")
    def test_existing_jsonpath_engine_with_credential_free_offline_format_control(self):
        projection = cilium.CONFIG_OUTPUT.replace("{.", "{.extensions[0].extension.")
        fields_by_name = ("PolicyCIDRMatchMode", "EnableCiliumNetworkPolicy", "EnableK8sNetworkPolicy")
        cases = [(mode, None) for mode in (None, [], ["nodes"])] + [([], name) for name in fields_by_name]
        with tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parent) as directory:
            path = Path(directory) / "offline-context.json"
            for mode, omitted in cases:
                value = {"apiVersion": "v1", "kind": "Config",
                         "clusters": [{"name": "offline", "cluster": {"server": "https://127.0.0.1:9"}}],
                         "users": [{"name": "offline", "user": {}}],
                         "contexts": [{"name": "offline", "context": {"cluster": "offline", "user": "offline"}}],
                         "current-context": "offline", "extensions": [{"name": "format-control", "extension": {
                             "PolicyCIDRMatchMode": mode, "EnableCiliumNetworkPolicy": True,
                             "EnableK8sNetworkPolicy": True}}]}
                if omitted:
                    del value["extensions"][0]["extension"][omitted]
                path.write_text(json.dumps(value))
                output = subprocess.run(
                    ["kubectl", "--kubeconfig", str(path), "config", "view", "--minify",
                     "--allow-missing-template-keys=true", "-o", projection],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20, check=False)
                self.assertEqual(output.returncode, 0, "Offline JSONPath control failed")
                fields = output.stdout.decode().strip("\r\n").split("\t")
                self.assertEqual(len(fields), 3)
                witness = {}
                if omitted:
                    self.assertEqual(fields[fields_by_name.index(omitted)], "")
                    with self.assertRaises((Failure, ValueError)):
                        cilium.effective_configuration(fields, witness)
                    self.assertFalse(witness["checks"]["config_fields"]["requiredTokensPresent"])
                elif mode is None and fields[0] not in ("null", "[]"):
                    self.assertEqual(fields[1:], ["true", "true"])
                    self.assertIn(cilium.rendering(fields[0]), ("empty", "go-nil", "go-no-value"))
                    with self.assertRaises((Failure, ValueError)):
                        cilium.effective_configuration(fields, witness)
                    self.assertEqual(witness["lastStage"], "config_mode")
                else:
                    self.assertEqual(fields[1:], ["true", "true"])
                    parsed = cilium.effective_configuration(fields, witness)
                    self.assertEqual(parsed["policyCIDRMatchMode"], mode or [])


if __name__ == "__main__":
    unittest.main()
