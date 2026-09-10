# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json
from pathlib import Path
import unittest
from unittest.mock import patch

from sre_authority.bootstrap_diagnostics import api_result, collect, control_plane_status, exception_summary, failure_facts, firewall_summary, object_status, policy_status, probe_command_result, public_stack_facts, router_log_summary, router_readiness_facts
from sre_authority.bootstrap_probe import builtin_documents, converted_objects, exercise, preserved_json_candidate, safe_controller

POLICIES = {"kars-sre-private-mounts": {"spec": {"validations": [
    {"message": "Private SRE material requires authority"}]}}}


class BootstrapProofTests(unittest.TestCase):
    def test_policy_compile_diagnostics_keep_only_known_fields_and_token_categories(self):
        body = {"kind": "Status", "reason": "Invalid", "details": {
            "group": "admissionregistration.k8s.io", "kind": "ValidatingAdmissionPolicy",
            "name": "kars-sre-private-mounts", "causes": [
                {"field": "spec.matchConditions[0].expression",
                 "message": "compilation failed: undefined field 'metadata'; optional overload do-not-publish"},
                {"field": "spec.arbitrary", "message": "do-not-publish"},
            ]}}
        facts = api_result(422, body, POLICIES)
        self.assertEqual(facts["compilationCauses"][0]["knownTokens"], ["metadata"])
        self.assertNotIn("do-not-publish", json.dumps(facts))
        body["details"]["name"] = "unrelated"
        self.assertNotIn("compilationCauses", api_result(422, body, POLICIES))

    def test_namespace_cleanup_proof_retains_guard_and_fences_all_deletes(self):
        from sre_authority.bootstrap_cases import namespace_cleanup_cases
        deployment = {"metadata": {"uid": "owned", "resourceVersion": "2"}}
        responses = [(404, {}), (201, deployment), (200, {"metadata": {"uid": "namespace-controller"}}),
                     (200, deployment), (200, {"metadata": {"uid": "kars-controller"}}),
                     (200, deployment), (200, deployment), (200, {})]
        with patch("sre_authority.bootstrap_cases.request", side_effect=responses) as api, \
                patch("sre_authority.bootstrap_cases.as_tenant", side_effect=[
                    (403, {"kind": "Status", "reason": "Forbidden",
                           "message": "kars-sre-consumer-authority denied do-not-publish"}),
                    (200, {"kind": "Status", "status": "Success"})]) as actor:
            cases = namespace_cleanup_cases(1, {"kars-sre-consumer-authority": {}})
        self.assertTrue(all(case["matched"] for case in cases))
        self.assertNotIn("do-not-publish", json.dumps(cases))
        for call in actor.call_args_list:
            self.assertEqual(call.kwargs["method"], "DELETE")
            self.assertNotIn("?", call.args[1])
            self.assertEqual(call.args[2]["dryRun"], ["All"])
            self.assertEqual(call.args[2]["preconditions"], {"uid": "owned", "resourceVersion": "2"})
        self.assertEqual(api.call_args_list[-1].args[1], "DELETE")
        self.assertEqual(api.call_args_list[-1].args[3]["preconditions"], {"uid": "owned", "resourceVersion": "2"})

    def test_cleanup_proof_never_adopts_an_unexpected_existing_consumer(self):
        from sre_authority.bootstrap_cases import namespace_cleanup_cases
        owned = {"metadata": {"uid": "owned"}, "spec": {"replicas": 0}}
        for current, expected in ((owned, None),
                                  ({"metadata": {"uid": "other"}, "spec": owned["spec"]}, owned),
                                  ({"metadata": owned["metadata"], "spec": {"replicas": 1}}, owned)):
            with patch("sre_authority.bootstrap_cases.request", return_value=(200, current)) as api, \
                    self.assertRaises(RuntimeError):
                namespace_cleanup_cases(1, {}, expected)
            api.assert_called_once()

    def test_http_failure_summary_reports_status_and_checked_source_not_body_url_or_headers(self):
        root = Path(__file__).resolve().parents[3]
        class Response:
            status_code = 401
            def json(self):
                return {"kind": "Status", "reason": "SREProxyDenied",
                        "message": "An SRE proxy credential is required", "data": "do-not-publish"}
        error = RuntimeError("do-not-publish URL/header/credential")
        error.response = Response()
        facts = exception_summary(error, root)
        self.assertEqual(facts["httpStatus"], 401)
        self.assertTrue(facts["responseSite"]["source"].endswith("sre_proxy/mod.rs"))
        self.assertNotIn("do-not-publish", json.dumps(facts))
        error.response.json = lambda: {"kind": "Status", "message": "do-not-publish"}
        self.assertNotIn("responseSite", exception_summary(error, root))
        self.assertNotIn("do-not-publish", json.dumps(exception_summary(error, root)))

    def test_firewall_summary_does_not_publish_rules_comments_addresses_or_unknown_chains(self):
        text = """*filter
:OUTPUT ACCEPT [12:640]
[4:240] -A OUTPUT -m owner --uid-owner 1000 -j DROP
[3:120] -A OUTPUT -m owner ! --uid-owner 1001 -o lo -j ACCEPT
[2:80] -A do-not-publish -s do-not-publish -m comment --comment do-not-publish -j DROP
COMMIT
"""
        value = firewall_summary(text)
        self.assertEqual(value["policies"], [{"table": "filter", "chain": "OUTPUT", "policy": "ACCEPT", "packets": 12}])
        self.assertEqual(value["rules"][0]["owner"], 1000)
        self.assertTrue(value["rules"][1]["ownerNegated"])
        self.assertEqual(value["rules"][2]["chain"], "custom")
        self.assertNotIn("do-not-publish", json.dumps(value))

    def test_router_startup_summary_reports_only_verified_source_coordinates(self):
        root = Path(__file__).resolve().parents[3]
        text = "\n".join([
            json.dumps({"fields": {"message": "kars Inference Router starting", "token": "do-not-publish"}}),
            json.dumps({"fields": {"message": "do-not-publish"}}),
            "do-not-publish plaintext",
        ])
        result = router_log_summary(text, root)
        self.assertEqual(result["lines"], 3)
        self.assertEqual(result["jsonEvents"], 2)
        self.assertEqual(len(result["sourceSites"]), 1)
        self.assertEqual(result["sourceSites"][0]["source"], "inference-router/src/main.rs")
        self.assertNotIn("do-not-publish", json.dumps(result))

    def test_actual_json_tracing_format_is_parsed_without_publishing_other_fields(self):
        events = [
            {"message": "SRE authority transport failure", "stage": "registration",
             "timed_out": True, "connect_error": False, "token": "do-not-publish"},
            {"message": "SRE authority request denied", "stage": "privacy-review", "http_status": 403},
            {"message": "SRE readiness authority slow", "authorized": True, "elapsed_seconds": 8},
            {"message": "SRE transport progress", "stage": "tls-accepted", "peer": "do-not-publish"},
            {"message": "SRE authority progress", "stage": "registration",
             "step": "request", "url": "do-not-publish"},
            {"message": "unrelated do-not-publish", "stage": "namespace"},
            {"message": "SRE authority request denied", "stage": "do-not-publish", "http_status": True},
        ]
        text = "\n".join(json.dumps({"fields": fields, "span": {"token": "do-not-publish"}}) for fields in events)
        facts = router_readiness_facts(text)
        self.assertEqual(facts, [
            {"stage": "registration", "timed_out": True, "connect_error": False},
            {"stage": "privacy-review", "httpStatus": 403},
            {"authorized": True, "elapsedSeconds": 8},
            {"stage": "tls-accepted"},
            {"stage": "registration", "step": "request"},
        ])
        self.assertNotIn("do-not-publish", json.dumps(facts))
        self.assertEqual(router_readiness_facts(json.dumps(events[0])), [facts[0]])

    def test_router_readiness_logs_expose_only_fixed_stage_status_and_timeout_facts(self):
        text = (
            '\x1b[33mSRE authority transport failure\x1b[0m stage="registration" timed_out=true connect_error=false token=do-not-publish\n'
            'SRE authority request denied stage="privacy-review" http_status=403 body=do-not-publish\n'
            'SRE readiness authority rejected category="authority-transport" do-not-publish\n'
            'SRE readiness authority slow elapsed_seconds=20 authorized=false do-not-publish\n'
            'unrelated log stage="source" do-not-publish\n'
            'SRE authority request denied stage="do-not-publish" http_status=999\n'
        )
        facts = router_readiness_facts(text)
        self.assertEqual(facts, [
            {"stage": "registration", "timed_out": True, "connect_error": False},
            {"stage": "privacy-review", "httpStatus": 403},
            {"category": "authority-transport"},
            {"authorized": False, "elapsedSeconds": 20},
        ])
        self.assertNotIn("do-not-publish", json.dumps(facts))

    def test_probe_diagnostics_never_publish_executable_output(self):
        for code, output, category in (
            (127, "exec: executable file not found in $PATH do-not-publish", "executable-not-found"),
            (1, "", "probe-not-ready"),
            (1, "command terminated with exit code 1\n", "probe-not-ready"),
            (0, "do-not-publish", "succeeded"),
            (1, "do-not-publish", "unclassified"),
        ):
            facts = probe_command_result(code, output)
            self.assertEqual(facts, {"exitCode": code, "category": category})
            self.assertNotIn("do-not-publish", json.dumps(facts))

    def test_failure_metadata_keeps_public_cause_not_body_or_credentials(self):
        message = ('Error creating Pod: kars-sre-private-mounts evaluation failed: no such key: namespace; '
                   'token=do-not-publish argv=do-not-publish')
        result = object_status({
            "kind": "ReplicaSet", "metadata": {"name": "kars-controller-abc", "uid": "rs-uid",
                "annotations": {"credential": "do-not-publish"}},
            "spec": {"template": {"spec": {"containers": [{"env": ["do-not-publish"]}]}}},
            "status": {"replicas": 0, "conditions": [{"type": "ReplicaFailure", "status": "True",
                "reason": "FailedCreate", "message": message}]},
        }, POLICIES)
        self.assertNotIn("do-not-publish", json.dumps(result))
        self.assertEqual(result["conditions"][0]["reason"], "FailedCreate")
        self.assertEqual(result["conditions"][0]["policies"], ["kars-sre-private-mounts"])
        self.assertEqual(result["conditions"][0]["missingFields"], ["namespace"])
        self.assertNotIn("do-not-publish", json.dumps(api_result(
            403, {"kind": "Status", "reason": "Forbidden", "message": message, "details": "do-not-publish"}, POLICIES)))

    def test_type_warning_scope_is_known_public_policy_field_only(self):
        obj = {"metadata": {"name": "kars-sre-private-mounts", "generation": 3},
               "status": {"observedGeneration": 3, "typeChecking": {"expressionWarnings": [
                   {"fieldRef": "spec.variables[0].expression", "warning": "undefined field namespace"},
                   {"fieldRef": "spec.containers[0].env", "warning": "do-not-publish"},
               ]}}}
        result = policy_status(obj, POLICIES)
        self.assertTrue(result["typeChecked"])
        self.assertEqual(len(result["warnings"]), 1)
        self.assertNotIn("do-not-publish", json.dumps(result))
        obj["metadata"]["name"] = "unrelated-policy"
        with self.assertRaises(AssertionError):
            policy_status(obj, POLICIES)

    def test_control_plane_metadata_and_go_frames_never_publish_logs_or_arguments(self):
        pod = {"metadata": {"name": "kube-controller-manager-kars-e2e-control-plane"},
            "spec": {"containers": [{"args": ["do-not-publish"]}]},
            "status": {"containerStatuses": [{"name": "kube-controller-manager", "ready": False,
                "restartCount": 4, "state": {"waiting": {"message": "do-not-publish"}},
                "lastState": {"terminated": {"exitCode": 2, "reason": "Error", "message": "do-not-publish"}}}]}}
        status = control_plane_status(pod)
        self.assertEqual(status["containers"][0]["restartCount"], 4)
        self.assertNotIn("do-not-publish", json.dumps(status))
        facts = public_stack_facts(
            "panic: runtime error: invalid memory address or nil pointer dereference\n"
            "token=do-not-publish request body do-not-publish\n"
            "k8s.io/apiserver/pkg/admission/plugin/policy/validating.(*TypeChecker).Check(do-not-publish)\n"
            "panic: do-not-publish\n")
        self.assertIn("nil-pointer", facts["panicCategories"])
        self.assertIn("panic-redacted", facts["panicCategories"])
        self.assertEqual(facts["publicFrames"], [
            "k8s.io/apiserver/pkg/admission/plugin/policy/validating.(*TypeChecker).Check"])
        self.assertNotIn("do-not-publish", json.dumps(facts))

    def test_no_scheduling_pulls_or_execution_but_original_controller_shape_remains(self):
        original = {"kind": "Deployment", "metadata": {"name": "kars-controller"},
                    "spec": {"replicas": 0, "template": {"spec": {
                        "serviceAccountName": "kars-controller", "automountServiceAccountToken": True,
                        "containers": [{"name": "controller", "image": "original", "env": [{"name": "X", "value": "Y"}]}],
                        "initContainers": [{"name": "init", "image": "original-init"}],
                    }}}}
        before = copy.deepcopy(original)
        result = safe_controller(original)
        self.assertEqual(original, before)
        pod = result["spec"]["template"]["spec"]
        self.assertEqual(result["spec"]["replicas"], 1)
        self.assertEqual(pod["serviceAccountName"], "kars-controller")
        self.assertEqual(pod["schedulerName"], "kars-e2e-admission-never-schedule")
        self.assertEqual(pod["containers"][0]["env"], [{"name": "X", "value": "Y"}])
        self.assertTrue(all(c["imagePullPolicy"] == "Never" and c["image"].startswith("registry.invalid/")
                            for c in pod["containers"] + pod["initContainers"]))
        with self.assertRaises(AssertionError):
            safe_controller({"kind": "Deployment", "metadata": {"name": "unrelated"}})

    def test_chart_selection_never_executes_other_workloads_or_loads_secret_data(self):
        rendered = "---\nkind: Secret\nmetadata:\n  name: private\nstringData:\n  token: do-not-publish\n"
        rendered += "---\nkind: Job\nmetadata:\n  name: execute\n---\nkind: ValidatingAdmissionPolicy\nmetadata:\n  name: public\n"
        result = builtin_documents(rendered)
        self.assertNotIn("do-not-publish", result)
        self.assertNotIn("kind: Job", result)
        self.assertIn("kind: ValidatingAdmissionPolicy", result)

    def test_kubectl_multiple_json_objects_and_list_are_both_parsed(self):
        first = {"kind": "ServiceAccount", "metadata": {"name": "kars-controller"}}
        second = {"kind": "ValidatingAdmissionPolicy", "metadata": {"name": "public"}}
        self.assertEqual(converted_objects(json.dumps(first) + "\n" + json.dumps(second)), [first, second])
        self.assertEqual(converted_objects(json.dumps({"kind": "List", "items": [first, second]})), [first, second])
        with self.assertRaises(RuntimeError):
            converted_objects(json.dumps({"kind": "Secret", "data": "do-not-publish"}))

    def test_candidate_changes_only_known_public_json_params_representation(self):
        params = {"type": "object", "additionalProperties": True, "description": "public"}
        action = {"kind": "CustomResourceDefinition", "metadata": {"name": "karssreactions.kars.azure.com"},
            "spec": {"versions": [{"schema": {"openAPIV3Schema": {"properties": {"spec": {"properties": {
                "action": {"properties": {"params": params}}, "approval": {"type": "object"}
            }}}}}}]}}
        policy = {"kind": "ValidatingAdmissionPolicy", "metadata": {"name": "kars-sre-pending-proposals"},
                  "spec": {"validations": [{"expression": "object.spec.approval.state == 'Pending'"}]}}
        source = [action, policy]
        before = copy.deepcopy(source)
        changed = preserved_json_candidate(source)
        self.assertEqual(source, before)
        self.assertEqual(changed[1], policy)
        actual = changed[0]["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"]["action"]["properties"]["params"]
        self.assertEqual(actual, {"type": "object", "x-kubernetes-preserve-unknown-fields": True, "description": "public"})
        params["additionalProperties"] = {"type": "string"}
        with self.assertRaises(RuntimeError):
            preserved_json_candidate(source)

    def test_arbitrary_failure_is_not_security_proof_in_candidate_cases(self):
        from sre_authority.bootstrap_cases import admission_cases
        with patch("sre_authority.bootstrap_cases.request", return_value=(201, {})), \
                patch("sre_authority.bootstrap_cases.upsert", return_value={"accepted": True}), \
                patch("sre_authority.bootstrap_cases.as_tenant", return_value=(
                    403, {"kind": "Status", "reason": "Forbidden", "message": "unrelated do-not-publish"})):
            cases = admission_cases(1, POLICIES)
        self.assertTrue(cases)
        self.assertFalse(any(case["matched"] for case in cases))
        self.assertNotIn("do-not-publish", json.dumps(cases))

    def test_builtin_controller_proof_preserves_real_identity_and_cluster_scope(self):
        from sre_authority.bootstrap_cases import DEPLOYMENT_CONTROLLER, deployment_controller_cases
        responses = [(200, {"metadata": {"uid": "actual-api-uid"}}),
                     (201, {"status": {"allowed": True}}),
                     (201, {"status": {"allowed": False}}),
                     (201, {"status": {"allowed": False}})]
        with patch("sre_authority.bootstrap_cases.request", side_effect=responses) as api, \
                patch("sre_authority.bootstrap_cases.as_tenant", return_value=(201, {"kind": "ReplicaSet"})) as actor:
            cases = deployment_controller_cases(1, POLICIES)
        self.assertTrue(all(case["matched"] for case in cases))
        self.assertEqual(cases[0]["authorization"], {
            "createReplicaSetsClusterWide": True, "createPodsClusterWide": False, "useRegistrar": False})
        for call in api.call_args_list[1:]:
            spec = call.args[3]["spec"]
            self.assertEqual(spec["user"], DEPLOYMENT_CONTROLLER)
            self.assertNotIn("namespace", spec["resourceAttributes"])
        for call in actor.call_args_list:
            self.assertEqual(call.kwargs["user"], DEPLOYMENT_CONTROLLER)
            self.assertTrue(call.args[1].endswith("?dryRun=All"))
            pod = call.args[2]["spec"]["template"]["spec"]
            self.assertEqual(pod["schedulerName"], "kars-e2e-admission-never-schedule")
            self.assertEqual(pod["containers"][0]["imagePullPolicy"], "Never")

    def test_private_controller_replicaset_denial_is_a_failure_not_a_security_pass(self):
        from sre_authority.bootstrap_cases import deployment_controller_cases
        policies = {"kars-sre-private-workloads": {}}
        for code in (403, 404, 422, 500):
            responses = [(200, {"metadata": {"uid": "actual-api-uid"}})] + [
                (201, {"status": {"allowed": allowed}}) for allowed in (True, False, False)]
            with patch("sre_authority.bootstrap_cases.request", side_effect=responses), \
                    patch("sre_authority.bootstrap_cases.as_tenant", side_effect=[
                        (201, {"kind": "ReplicaSet"}),
                        (code, {"kind": "Status", "reason": "Forbidden",
                                "message": "kars-sre-private-workloads forbids do-not-publish"})]):
                cases = deployment_controller_cases(1, policies)
            self.assertTrue(cases[0]["matched"])
            self.assertFalse(cases[1]["matched"])
            self.assertEqual(cases[1]["expectedStatus"], 201)
            self.assertNotIn("do-not-publish", json.dumps(cases))

    def test_controller_proof_requires_actual_account_and_valid_authorization_response(self):
        from sre_authority.bootstrap_cases import deployment_controller_cases
        for responses in ([(404, {})], [(200, {"metadata": {}})],
                          [(200, {"metadata": {"uid": "uid"}}), (403, {})],
                          [(200, {"metadata": {"uid": "uid"}}), (201, {"status": {"allowed": "true"}})]):
            with patch("sre_authority.bootstrap_cases.request", side_effect=responses), \
                    patch("sre_authority.bootstrap_cases.as_tenant") as actor, self.assertRaises(RuntimeError):
                deployment_controller_cases(1, POLICIES)
            actor.assert_not_called()

    def private_chain_api(self, *, pod_change=None, replaced=False, no_child=False):
        created = {}
        def api(_port, method, path, obj=None):
            if method == "POST":
                created.update(copy.deepcopy(obj))
                created["metadata"]["uid"] = "deployment-uid"
                return 201, created
            if "/deployments/" in path:
                current = copy.deepcopy(created)
                if replaced:
                    current["metadata"]["uid"] = "replacement"
                current["status"] = {"conditions": [{"type": "Progressing", "status": "False",
                    "reason": "ReplicaSetCreateError", "message": "kars-sre-private-workloads forbidden do-not-publish"}]}
                return 200, current
            if "/replicasets?" in path:
                return 200, {"items": [{"metadata": {"name": "owned-rs", "uid": "replicaset-uid",
                    "ownerReferences": [{"uid": "deployment-uid", "controller": True}]}}]}
            if "/pods?" in path:
                pod = {"metadata": {"name": "owned-pod", "uid": "pod-uid",
                    "ownerReferences": [{"uid": "replicaset-uid", "controller": True}]},
                    "spec": copy.deepcopy(created["spec"]["template"]["spec"])}
                if pod_change:
                    pod_change(pod)
                return 200, {"items": [] if no_child else [pod, {"metadata": {"name": "do-not-publish",
                    "uid": "foreign", "ownerReferences": [{"uid": "not-ours", "controller": True}]}}]}
            raise AssertionError("Unexpected private chain API request")
        return api

    def test_actual_private_chain_requires_deployment_replicaset_pod_uid_ownership(self):
        from sre_authority.bootstrap_cases import private_controller_chain
        reports = []
        with patch("sre_authority.bootstrap_cases.request", side_effect=self.private_chain_api()) as api:
            private_controller_chain(1, {"kars-sre-private-workloads": {}}, reports.append)
        self.assertTrue(reports[0]["privateMountPreserved"])
        self.assertTrue(reports[0]["noWorkloadExecution"])
        self.assertEqual([pod["uid"] for pod in reports[0]["pods"]], ["pod-uid"])
        self.assertNotIn("do-not-publish", json.dumps(reports))
        self.assertEqual([call.args[1] for call in api.call_args_list], ["POST", "GET", "GET", "GET"])
        self.assertTrue(api.call_args_list[0].args[2].endswith("/deployments"))

    def test_private_chain_rejects_replacement_missing_mount_or_execution(self):
        from sre_authority.bootstrap_cases import private_controller_chain
        variants = [
            {"replaced": True},
            {"pod_change": lambda pod: pod["spec"].update(nodeName="scheduled-node")},
            {"pod_change": lambda pod: pod["spec"].pop("volumes")},
            {"pod_change": lambda pod: pod["spec"]["containers"][0].pop("volumeMounts")},
        ]
        for variant in variants:
            with patch("sre_authority.bootstrap_cases.request", side_effect=self.private_chain_api(**variant)), \
                    self.subTest(variant=variant), self.assertRaises(RuntimeError):
                private_controller_chain(1, POLICIES, lambda _report: None)

    def test_private_chain_timeout_reports_sanitized_blocker_not_success(self):
        from sre_authority.bootstrap_cases import private_controller_chain
        for variant in ({"no_child": True},
                        {"pod_change": lambda pod: pod["metadata"]["ownerReferences"][0].update(uid="foreign")},
                        {"pod_change": lambda pod: pod["metadata"]["ownerReferences"][0].update(controller=False)}):
            reports = []
            with patch("sre_authority.bootstrap_cases.request", side_effect=self.private_chain_api(**variant)), \
                    patch("sre_authority.bootstrap_cases.time.monotonic", side_effect=[0, 1, 46]), \
                    patch("sre_authority.bootstrap_cases.time.sleep"), self.assertRaises(RuntimeError):
                private_controller_chain(1, {"kars-sre-private-workloads": {}}, reports.append)
            self.assertFalse(reports[0]["privateMountPreserved"])
            self.assertFalse(reports[0]["noWorkloadExecution"])
            self.assertNotIn("do-not-publish", json.dumps(reports))

    def test_collection_tracks_real_uid_chain_without_logging_other_pods(self):
        def request(_port, _method, path):
            if path.endswith("/deployments"):
                return 200, {"items": [{"metadata": {"name": "kars-controller", "uid": "dep"}}]}
            if path.endswith("/replicasets"):
                return 200, {"items": [{"metadata": {"name": "kars-controller-rs", "uid": "rs",
                    "ownerReferences": [{"uid": "dep"}]}, "status": {"replicas": 0}}]}
            if path.endswith("/pods"):
                return 200, {"items": [{"metadata": {"name": "created", "uid": "pod",
                    "ownerReferences": [{"uid": "rs"}]}},
                    {"metadata": {"name": "unrelated", "uid": "other"}, "spec": {"private": "do-not-publish"}}]}
            return 200, {"items": []}
        result = collect(1, {}, request)
        self.assertEqual([o["kind"] for o in result["workloads"]], ["Deployment", "ReplicaSet", "Pod"])
        self.assertNotIn("do-not-publish", json.dumps(result))
        self.assertNotIn("unrelated", json.dumps(result))

    def test_observation_timeout_collects_pod_evidence_but_still_fails(self):
        deployment = {"kind": "Deployment", "metadata": {"name": "kars-controller"},
                      "spec": {"template": {"metadata": {"labels": {}}, "spec": {"containers": []}}}}
        unobserved = {"policies": [{"generation": 1, "typeChecked": False}], "workloads": []}
        created = dict(unobserved, workloads=[{"kind": "Pod"}])
        with patch("sre_authority.bootstrap_probe.request", return_value=(201, {})) as api, \
                patch("sre_authority.bootstrap_probe.upsert", return_value={"accepted": True}), \
                patch("sre_authority.bootstrap_probe.collect", side_effect=[unobserved, created]), \
                patch("sre_authority.bootstrap_probe.time.monotonic", side_effect=[0, 1, 91, 100, 101]), \
                patch("sre_authority.bootstrap_probe.time.sleep"), \
                patch("sre_authority.bootstrap_probe.write_report") as report:
            with self.assertRaisesRegex(RuntimeError, "observation timed out"):
                exercise(Path("."), 1, [deployment], POLICIES)
        self.assertTrue(any(call.args[2].endswith("pods?dryRun=All") for call in api.call_args_list))
        result = next(call.args[2] for call in report.call_args_list if call.args[1] == "bootstrap-result.json")
        self.assertEqual(result["podCreation"], "accepted")
        self.assertFalse(result["allPoliciesObservedBeforeCreate"])
        self.assertEqual(result["readiness"], "not-claimed")

    def test_failure_diagnostics_precede_teardown_without_pod_spec_or_log_dump(self):
        source = (Path(__file__).resolve().parents[1] / "run.sh").read_text()
        install = source.split("install_crds() {", 1)[1].split("\nteardown()", 1)[0]
        self.assertIn("sre_authority.bootstrap_probe --diagnostics-only", install)
        self.assertNotIn("kubectl describe pod", install)
        self.assertNotIn("kubectl logs", install)
        self.assertIn("return 1", install)


if __name__ == "__main__":
    unittest.main()
