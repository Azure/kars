# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import hashlib
import json
from pathlib import Path
import unittest
from unittest.mock import patch

from private_consumption import KINDS, PREFIX, PRIVATE, denied, namespace_surface_cases, pod_spec, root_token_retirement_case, variants, workload
from sre_authority import private_consumption_phase as phase


class Response:
    def __init__(self, status, message):
        self.status_code, self.message = status, message

    def json(self):
        return {"message": self.message}


class PrivateConsumptionFixtures(unittest.TestCase):
    def test_namespace_gate_is_in_validation_and_preserves_reviewed_authority_exactly(self):
        bundle = json.loads((Path(__file__).resolve().parents[2]
                             / "deploy/helm/kars/files/private-consumption.json").read_text())
        hashes = {"kars-private-consumption": "1d78da746103834f9afcabcd8890e99ea535deb49ac8c8714fe55b5ab8f8cd90",
                  "kars-private-consumption-connect": "44e04003472a49ac1e8be821c8aac5a9613b21f302da44c7c4c6c11999cdbb73"}
        gate = "variables.a[?'kars.azure.com/private-enabled'].orValue('') == 'true' ? ("
        for name, expected in hashes.items():
            policy = next(o for o in bundle["objects"] if o["kind"] == "ValidatingAdmissionPolicy"
                          and o["metadata"]["name"] == name)
            self.assertNotIn("matchConditions", policy["spec"])
            self.assertEqual(policy["spec"]["variables"][0],
                             {"name": "a", "expression": "namespaceObject.metadata.?annotations.orValue({})"})
            expression = policy["spec"]["validations"][0]["expression"]
            self.assertTrue(expression.startswith(gate))
            self.assertTrue(expression.endswith(") : true"))
            # The 63ef authority body is byte-identical inside the phase gate.
            self.assertEqual(hashlib.sha256(expression[len(gate):-len(") : true")].encode()).hexdigest(), expected)
            self.assertEqual(policy["spec"]["failurePolicy"], "Fail")
            self.assertEqual(policy["spec"]["validations"][0]["reason"], "Forbidden")
            binding = next(o for o in bundle["objects"] if o["kind"] == "ValidatingAdmissionPolicyBinding"
                           and o["metadata"]["name"] == name)
            self.assertEqual(binding["spec"]["validationActions"], ["Deny", "Audit"])
        for policy in bundle["objects"]:
            for condition in policy.get("spec", {}).get("matchConditions", []):
                self.assertNotIn("namespaceObject", condition["expression"])

    def test_native_phase_shapes_cover_all_matched_workload_kinds_without_execution(self):
        for kind in ("Pod", *(item[0] for item in KINDS)):
            for private in (False, True):
                value = phase.shape(kind, "namespace", private, "a" * 64 if private else None)
                spec = phase.template(value)["spec"]
                self.assertFalse(spec["automountServiceAccountToken"])
                self.assertEqual(spec["schedulerName"], "private-consumption-never-schedule")
                self.assertEqual(spec["containers"][0]["imagePullPolicy"], "Never")
                self.assertNotIn("nodeName", spec)
                self.assertEqual(bool(spec.get("volumes")), private)
                self.assertTrue(phase.collection(value).endswith(
                    "/pods" if kind == "Pod" else "/" + next(item[3] for item in KINDS if item[0] == kind)))
        self.assertEqual({stage[0] for stage in phase.STAGES}, {
            "deployment-controller", "cronjob-controller", "replicaset-controller",
            "replication-controller", "statefulset-controller", "daemon-set-controller", "job-controller"})
        self.assertEqual(phase.CONNECTIONS, ("exec", "attach", "portforward", "proxy"))

    def test_native_phase_source_selection_refuses_missing_duplicate_and_changed_policies(self):
        bundle = json.loads((Path(__file__).resolve().parents[2]
                             / "deploy/helm/kars/files/private-consumption.json").read_text())
        objects = bundle["objects"]
        self.assertEqual(len(phase.source_policy(objects)), 2)
        for values in ([], objects + [objects[0]], copy.deepcopy(objects)):
            if len(values) == len(objects):
                values[0]["spec"]["failurePolicy"] = "Ignore"
            with self.assertRaises(RuntimeError):
                phase.source_policy(values)

    def test_native_phase_transport_exercises_both_namespaces_controller_uids_and_fenced_cleanup(self):
        api = PhaseAPI()
        reports = []
        with patch.object(phase, "request", side_effect=api.request), \
                patch.object(phase.shared, "request", side_effect=api.request), \
                patch.object(phase, "as_tenant", side_effect=api.actor), \
                patch.object(phase.shared, "wait_for", side_effect=api.wait):
            cases = phase.cases(1, api.bundle["objects"], reports.append)
        self.assertEqual(len(cases), 108)
        self.assertTrue(all(case["matched"] for case in cases))
        self.assertEqual(len([c for c in cases if "-missing-metadata" in c["case"]]), 40)
        self.assertEqual(len([c for c in cases if c["expectedStatus"] == 404]), 8)
        self.assertEqual(api.objects, {})
        self.assertTrue(all(preconditions.get("uid") for preconditions in api.deleted))
        self.assertFalse(any("/secrets" in call[2] or "/status" in call[2] for call in api.calls))
        self.assertTrue(all(call[3] in (None, {}) for call in api.calls
                            if call[1] == "GET" and call[2].split("/")[-1] in phase.CONNECTIONS))
        self.assertEqual(len(api.namespace_patches), 2)
        self.assertTrue(all({"uid", "resourceVersion"} <= set(p["metadata"]) for p in api.namespace_patches))
        self.assertTrue(all(not obj["spec"].get("matchConditions")
                            for obj in api.fault_policies))
        self.assertNotIn("do-not-publish", json.dumps(reports))

    def test_native_phase_rejects_wrong_denials_false_fault_acceptance_and_wrong_lookup_errors(self):
        for fault in ("allow-private", "allow-missing-metadata", "unrelated-not-found"):
            api = PhaseAPI(fault)
            reports = []
            with patch.object(phase, "request", side_effect=api.request), \
                    patch.object(phase.shared, "request", side_effect=api.request), \
                    patch.object(phase, "as_tenant", side_effect=api.actor), \
                    patch.object(phase.shared, "wait_for", side_effect=api.wait), self.assertRaises(RuntimeError):
                phase.cases(1, api.bundle["objects"], reports.append)
            self.assertEqual(api.objects, {})
            if fault != "allow-missing-metadata":
                self.assertFalse(reports[-1]["cases"][-1]["matched"])

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


class PhaseAPI:
    """Transport/orchestration fixture only; does not compile or evaluate CEL."""

    def __init__(self, fault=None):
        self.bundle = json.loads((Path(__file__).resolve().parents[2]
                                  / "deploy/helm/kars/files/private-consumption.json").read_text())
        self.fault = fault
        self.objects, self.calls, self.deleted, self.namespace_patches, self.fault_policies = {}, [], [], [], []
        self.serial = 0

    def request(self, _port, method, path, body=None):
        return self.respond(None, None, method, path, body)

    def actor(self, _port, path, obj, *, user, uid=None, method="POST"):
        return self.respond(user, uid, method, path, obj)

    def wait(self, probe, predicate, *_args, **_kwargs):
        code, value = probe()
        if not predicate(code, value):
            raise RuntimeError("Fixture expected proof did not match")
        return code, value

    def decision(self, user, uid, path, body, connection=False):
        parts = path.split("?")[0].strip("/").split("/")
        namespace = parts[parts.index("namespaces") + 1]
        ns = self.objects[f"/api/v1/namespaces/{namespace}"]
        bindings = {obj["spec"]["policyName"] for obj in self.objects.values()
                    if obj["kind"] == "ValidatingAdmissionPolicyBinding"}
        for policy in self.fault_policies:
            connects = policy["spec"]["matchConstraints"]["resourceRules"][0]["operations"] == ["CONNECT"]
            if (self.fault != "allow-missing-metadata" and connection == connects
                    and policy["metadata"]["name"] in bindings):
                return policy["metadata"]["name"], 422
        fields = ns["metadata"].get("annotations", {})
        if user is None or fields.get(PREFIX + "enabled") != "true":
            return None, 201
        root = self.objects[f"/api/v1/namespaces/{namespace}/serviceaccounts/root"]
        if user.endswith(":root") and uid == root["metadata"]["uid"]:
            return None, 201
        if connection:
            return "kars-private-consumption-connect", 403
        def consumes(value):
            return bool(phase.template(value)["spec"].get("volumes")
                        or phase.template(value)["metadata"].get("annotations", {}).get(PREFIX + "epoch"))
        previous = self.objects.get(path.split("?")[0])
        if not consumes(body) and (previous is None or not consumes(previous)):
            return None, 201
        for controller, kind, owner in phase.STAGES:
            refs = body["metadata"].get("ownerReferences", [])
            if (user == "system:serviceaccount:kube-system:" + controller and uid == "uid-" + controller
                    and body["kind"] == kind and len(refs) == 1 and refs[0]["kind"] == owner):
                return None, 201
        if self.fault == "allow-private":
            return None, 201
        return "kars-private-consumption", 403

    def denied(self, name, code, obj_name):
        policy = next((p for p in self.bundle["objects"]
                       if p["kind"] == "ValidatingAdmissionPolicy" and p["metadata"]["name"] == name), None)
        message = ("no such key: metadata" if code == 422 else policy["spec"]["validations"][0]["message"])
        text = f"ValidatingAdmissionPolicy '{name}' with binding '{name}' denied request: {message}"
        return code, {"kind": "Status", "status": "Failure", "reason": "Invalid" if code == 422 else "Forbidden",
                      "message": text, "details": {"name": obj_name, "causes": [{"message": text}]},
                      "unrelated": "do-not-publish"}

    def respond(self, user, uid, method, path, body):
        self.calls.append((user, method, path, copy.deepcopy(body)))
        target = path.split("?")[0]
        if method == "GET" and target.split("/")[-1] in phase.CONNECTIONS:
            policy, code = self.decision(user, uid, path, body, True)
            if policy:
                return self.denied(policy, code, phase.ABSENT_POD)
            return 404, {"kind": "Status", "reason": "NotFound", "details": {
                "name": phase.ABSENT_POD, "kind": "secrets" if self.fault == "unrelated-not-found" else "pods"}}
        if method == "GET" and "/kube-system/serviceaccounts/" in path:
            return 200, {"metadata": {"uid": "uid-" + path.rsplit("/", 1)[1]}}
        if method == "GET" and "/nodes?" in path:
            return 200, {"items": []}
        if "?dryRun=All" in path:
            policy, code = self.decision(user, uid, path, body)
            if policy:
                return self.denied(policy, code, body["metadata"]["name"])
            return (200 if method == "PUT" else 201), copy.deepcopy(body)
        if method == "POST":
            value = copy.deepcopy(body)
            self.serial += 1
            value["metadata"].update(uid=f"uid-{self.serial}", resourceVersion="1", generation=1)
            if value["kind"] == "ValidatingAdmissionPolicy":
                value["status"] = {"observedGeneration": 1, "typeChecking": {}}
                self.fault_policies.append(value)
            self.objects[target + "/" + value["metadata"]["name"]] = value
            return 201, copy.deepcopy(value)
        if method == "GET":
            return (200, copy.deepcopy(self.objects[target])) if target in self.objects else (404, {})
        if method == "PATCH":
            value = self.objects[target]
            assert body["metadata"]["uid"] == value["metadata"]["uid"]
            assert body["metadata"]["resourceVersion"] == value["metadata"]["resourceVersion"]
            self.namespace_patches.append(copy.deepcopy(body))
            value["metadata"].setdefault("annotations", {}).update(body["metadata"]["annotations"])
            value["metadata"]["resourceVersion"] = str(int(value["metadata"]["resourceVersion"]) + 1)
            return 200, copy.deepcopy(value)
        if method == "DELETE":
            assert body["preconditions"]["uid"] == self.objects[target]["metadata"]["uid"]
            self.deleted.append(body["preconditions"])
            del self.objects[target]
            return 200, {}
        raise AssertionError("Unexpected fixture API operation")


if __name__ == "__main__":
    unittest.main()
