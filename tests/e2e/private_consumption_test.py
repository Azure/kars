# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json
from pathlib import Path
import unittest

from private_consumption import KINDS, PREFIX, PRIVATE, denied, namespace_surface_cases, pod_spec, root_token_retirement_case, variants, workload


class Response:
    def __init__(self, status, message):
        self.status_code, self.message = status, message

    def json(self):
        return {"message": self.message}


class PrivateConsumptionFixtures(unittest.TestCase):
    def test_namespace_contract_covers_all_metadata_mutating_surfaces(self):
        bundle = json.loads((Path(__file__).resolve().parents[2]
                             / "deploy/helm/kars/files/private-consumption.json").read_text())
        policy = next(value for value in bundle["objects"]
                      if value["kind"] == "ValidatingAdmissionPolicy"
                      and value["metadata"]["name"] == "kars-private-consumption-namespace")
        rule = policy["spec"]["matchConstraints"]["resourceRules"][0]
        self.assertEqual(rule["resources"], ["namespaces", "namespaces/status", "namespaces/finalize"])
        self.assertEqual(rule["operations"], ["CREATE", "UPDATE"])
        self.assertEqual(rule["scope"], "Cluster")
        self.assertIn("oldObject", policy["spec"]["matchConditions"][0]["expression"])
        self.assertIn("variables.manager || variables.projector", policy["spec"]["validations"][0]["expression"])

    def test_only_operator_can_change_retirement_attempt_and_review_requires_original_replica_intent(self):
        bundle = json.loads((Path(__file__).resolve().parents[2]
                             / "deploy/helm/kars/files/private-consumption.json").read_text())
        policy = next(value for value in bundle["objects"]
                      if value["kind"] == "ValidatingAdmissionPolicy"
                      and value["metadata"]["name"] == "kars-private-consumption-namespace")
        expression = next(value["expression"] for value in policy["spec"]["validations"]
                          if "root-retirement" in value["expression"])
        self.assertTrue(expression.startswith("variables.manager || "))
        self.assertNotIn("variables.projector", expression)
        self.assertIn("oldObject.metadata", expression)
        self.assertEqual(expression.count(PREFIX + "root-retirement"), 2)
        root = bundle["activationSchema"]["properties"]["root"]
        self.assertIn("replicaIntent", root["required"])
        self.assertEqual(root["properties"]["replicaIntent"],
                         {"type": "integer", "minimum": 0, "maximum": 2147483647})

    def test_all_native_kinds_have_nonexecuting_bases_and_all_material_forms(self):
        for kind, *_ in KINDS:
            with self.subTest(kind=kind):
                value = workload(kind, "fixture", "workspace")
                pod = pod_spec(value)
                self.assertFalse(pod["automountServiceAccountToken"])
                self.assertEqual(pod["containers"][0]["command"], ["/bin/true"])
                self.assertEqual(pod["containers"][0]["imagePullPolicy"], "Never")
                self.assertEqual(pod["schedulerName"], "private-consumption-never-schedule")
                self.assertNotIn("volumes", pod)
                self.assertEqual(len(variants(value)), len(PRIVATE) + 7)
                for invalid in variants(value):
                    self.assertEqual(pod_spec(invalid)["containers"][0]["command"], ["/bin/true"])
                    self.assertNotEqual(invalid, value)
                if kind in ("Deployment", "ReplicaSet", "StatefulSet", "ReplicationController"):
                    self.assertEqual(value["spec"]["replicas"], 0)
                elif kind == "Job":
                    self.assertEqual(value["spec"]["parallelism"], 0)
                    self.assertTrue(value["spec"]["suspend"])
                elif kind == "CronJob":
                    self.assertTrue(value["spec"]["suspend"])
                else:
                    self.assertTrue(pod["nodeSelector"])

    def test_each_variant_is_independent_and_does_not_mutate_its_zero_replica_base(self):
        value = workload("Deployment", "fixture", "workspace")
        original = copy.deepcopy(value)
        values = variants(value)
        pod_spec(values[0])["containers"][0]["image"] = "changed"
        self.assertEqual(value, original)
        self.assertNotEqual(pod_spec(values[1])["containers"][0]["image"], "changed")

    def test_only_the_exact_native_policy_denial_counts(self):
        denied(Response(403, 'ValidatingAdmissionPolicy "kars-private-consumption" denied request'))
        for status, message in [
            (201, 'kars-private-consumption'),
            (403, 'RBAC forbids update'),
            (422, 'kars-private-consumption invalid shape'),
            (403, 'kars-private-consumption-namespace denied'),
        ]:
            with self.subTest(status=status, message=message):
                with self.assertRaises(AssertionError):
                    denied(Response(status, message))

    def test_named_namespace_subresource_attempts_never_mutate_and_are_followed_by_template_denials(self):
        harness = NamespaceHarness()
        before = copy.deepcopy(harness.namespace)
        created = []
        namespace_surface_cases(harness, "work", "actor", {"username": "actor", "uid": "actor-uid"},
                                "/apis/apps/v1/namespaces/work/deployments/fixture", "workload", created)
        self.assertEqual(harness.namespace, before)
        probes = [call for call in harness.calls if call[0] in ("PATCH", "PUT")]
        self.assertTrue(all("?dryRun=All" in call[1] for call in probes))
        self.assertEqual(sum("/namespaces/work/status?" in call[1] for call in probes), 12)
        self.assertEqual(sum("/namespaces/work/finalize?" in call[1] for call in probes), 12)
        self.assertEqual(sum("/deployments/fixture?" in call[1] for call in probes), 20)
        roles = [body for method, path, body, _ in harness.calls if method == "POST" and path.endswith("/clusterroles")]
        self.assertEqual(roles[0]["rules"][0]["resourceNames"], ["work"])
        self.assertEqual(roles[0]["rules"][0]["resources"], ["namespaces/status", "namespaces/finalize"])

    def test_namespace_bypass_or_an_unrelated_denial_cannot_count_as_success(self):
        for options in ({"allow_fence": True}, {"wrong_denial": True}):
            with self.subTest(options=options):
                harness = NamespaceHarness(**options)
                with self.assertRaises(AssertionError):
                    namespace_surface_cases(harness, "work", "actor", {"username": "actor", "uid": "actor-uid"},
                                            "/apis/apps/v1/namespaces/work/deployments/fixture", "workload", [])

    def test_native_root_authority_probe_checks_real_bound_token_before_and_after_without_mount_or_log_access(self):
        harness = TokenHarness()
        root_token_retirement_case(harness, "core", "root", "old-root", harness.activate)
        self.assertEqual(harness.results, ["private activation retired the old root API authority"])
        self.assertFalse(any("/log" in path or "/exec" in path or "/secrets" in path
                             for _, path, _ in harness.calls))
        request = next(body for _, path, body in harness.calls if path.endswith("/token"))
        self.assertEqual(request["spec"]["boundObjectRef"]["uid"], "old-root")

    def test_still_present_or_still_authenticated_old_root_never_passes(self):
        for mode in ("terminating", "still-valid"):
            harness = TokenHarness(mode)
            with self.assertRaises(AssertionError):
                root_token_retirement_case(harness, "core", "root", "old-root", harness.activate)
            self.assertEqual(harness.results, [])


class ApiResponse:
    def __init__(self, code, body):
        self.status_code, self.body = code, body

    def json(self):
        return copy.deepcopy(self.body)


class NamespaceHarness:
    def __init__(self, *, allow_fence=False, wrong_denial=False):
        self.calls = []
        self.allow_fence, self.wrong_denial = allow_fence, wrong_denial
        self.namespace = {"apiVersion": "v1", "kind": "Namespace",
                          "metadata": {"name": "work", "uid": "namespace", "resourceVersion": "1",
                                       "annotations": {PREFIX + "enabled": "true", PREFIX + "epoch": "a" * 64,
                                                       PREFIX + "namespace-uid": "namespace", PREFIX + "root-uid": "root"}},
                          "spec": {"finalizers": ["kubernetes"]}, "status": {"phase": "Active"}}
        self.workload = workload("Deployment", "fixture", "work")
        self.workload["metadata"].update(uid="workload", resourceVersion="1")

    def api(self, method, path, *, body=None, user="admin", status=None):
        self.calls.append((method, path, copy.deepcopy(body), user))
        if path.endswith("selfsubjectaccessreviews"):
            response = ApiResponse(201, {"status": {"allowed": body["spec"]["resourceAttributes"].get("name") == "work"}})
        elif method == "POST":
            response = ApiResponse(201, {"metadata": {"uid": "fixture-resource", "resourceVersion": "1"}})
        elif method == "GET":
            response = ApiResponse(200, self.namespace if path == "/api/v1/namespaces/work" else self.workload)
        elif "/deployments/" in path:
            response = ApiResponse(403, {"message": 'ValidatingAdmissionPolicy "kars-private-consumption" denied request'})
        else:
            before = self.namespace["metadata"]["annotations"]
            if method == "PATCH":
                after = copy.deepcopy(before)
                for key, value in body["metadata"].get("annotations", {}).items():
                    if value is None:
                        after.pop(key, None)
                    else:
                        after[key] = value
            else:
                after = body["metadata"]["annotations"]
            changed = {k: v for k, v in before.items() if k.startswith(PREFIX)} != {
                k: v for k, v in after.items() if k.startswith(PREFIX)}
            policy = "unrelated-policy" if self.wrong_denial else "kars-private-consumption-namespace"
            response = ApiResponse(403, {"message": f'ValidatingAdmissionPolicy "{policy}" denied request'}) \
                if changed and not self.allow_fence else ApiResponse(200, self.namespace)
        if status is not None:
            assert response.status_code in (status if isinstance(status, tuple) else (status,))
        return response

class TokenHarness:
    def __init__(self, mode="retired"):
        self.mode, self.active, self.calls, self.results = mode, True, [], []

    def activate(self):
        self.active = False

    def passed(self, value):
        self.results.append(value)

    def poll(self, _description, operation, **_options):
        if not operation():
            raise AssertionError("Old authority remains valid")

    def api(self, method, path, *, body=None, **_options):
        self.calls.append((method, path, copy.deepcopy(body)))
        if path.endswith("/token"):
            return ApiResponse(201, {"status": {"token": "synthetic-private-token"}})
        if path.endswith("/tokenreviews"):
            return ApiResponse(201, {"status": {"authenticated": self.active or self.mode == "still-valid",
                                               "user": {"uid": "root-account"}}})
        if "/serviceaccounts/" in path:
            return ApiResponse(200, {"metadata": {"uid": "root-account"}})
        if path.endswith("/pods"):
            items = [{"metadata": {"uid": "old-root", "deletionTimestamp": "terminating"}}] \
                if self.mode == "terminating" else []
            return ApiResponse(200, {"metadata": {}, "items": items})
        return ApiResponse(200, {"metadata": {"uid": "old-root"}, "spec": {"serviceAccountName": "kars-controller"}})


if __name__ == "__main__":
    unittest.main()
