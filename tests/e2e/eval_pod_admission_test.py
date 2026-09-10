# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Fixture/interface fences, not a claim that native Pod admission ran."""

import contextlib
import copy
import io
import json
from pathlib import Path
import unittest
from unittest.mock import MagicMock, patch
from urllib.error import URLError

import eval_pod_admission as probe
from eval_pod_admission_fixtures import Cluster, JOB, PRIVATE, objects, status


class EvalPodAdmissionTests(unittest.TestCase):
    def run_probe(self, cluster):
        with patch.object(probe.api, "request", side_effect=cluster.request):
            return probe.probe(1234, JOB)

    def test_both_live_producers_and_legacy_controls_use_dry_run_without_spec_replacement(self):
        cluster = Cluster()
        original = copy.deepcopy(cluster.sources)
        results = self.run_probe(cluster)
        self.assertEqual([(r["producer"], r["httpStatus"]) for r in results],
                         [("Job", 201), ("Job", 403), ("CronJob", 201), ("CronJob", 403)])
        self.assertEqual(cluster.sources, original)
        self.assertIsNone(cluster.namespace)
        pods = [obj for method, path, obj in cluster.calls if "/pods?" in path]
        templates = (list(original.values())[1]["spec"]["template"],
                     list(original.values())[2]["spec"]["jobTemplate"]["spec"]["template"])
        for index, template in zip((0, 2), templates):
            self.assertEqual(pods[index]["spec"], template["spec"])
            self.assertEqual(pods[index]["metadata"]["labels"], template["metadata"]["labels"])
            self.assertNotIn("runAsUser", pods[index]["spec"]["securityContext"])
            self.assertNotIn("runAsUser", pods[index]["spec"]["containers"][0]["securityContext"])
            expected = copy.deepcopy(template["spec"])
            expected.pop("securityContext")
            expected["containers"][0].pop("securityContext")
            self.assertEqual(pods[index + 1]["spec"], expected)
        writes = [(method, path) for method, path, _ in cluster.calls if method != "GET"]
        self.assertEqual(len(writes), 6)
        self.assertEqual(writes[0], ("POST", probe.NAMESPACES))
        self.assertTrue(all(path.endswith("/pods?dryRun=All") for _, path in writes[1:5]))
        self.assertEqual(writes[-1][0], "DELETE")
        create = next(obj for method, path, obj in cluster.calls if path == probe.NAMESPACES)
        self.assertTrue(create["metadata"]["name"].startswith("kars-eval-admission-"))
        for key, value in probe.PSS_LABELS.items():
            self.assertEqual(create["metadata"]["labels"][key], value)

    def test_defaulting_may_add_fields_but_never_change_submitted_spec_values(self):
        self.assertTrue(probe.contains_spec({"a": [{"name": "runner", "default": True}, {}]},
                                            {"a": [{"name": "runner"}]}))
        for current in ({"a": [{"name": "replacement"}]}, {"a": []}, {"a": "wrong"}):
            self.assertFalse(probe.contains_spec(current, {"a": [{"name": "runner"}]}))
        self.assertFalse(probe.contains_spec({"runAsNonRoot": 1}, {"runAsNonRoot": True}))

    def test_negative_requires_exact_pod_security_status_not_rbac_or_other_policy_denial(self):
        pod = probe.pod_from_template(objects()[1]["spec"]["template"], "fixture", "job-legacy", legacy=True)
        valid = status(403, "Forbidden", "job-legacy", "pods",
                       'pods "job-legacy" is forbidden: violates PodSecurity "restricted:v1.31": '
                       + ", ".join(probe.PSS_FAILURES))
        self.assertEqual(probe.admission_result(403, valid, pod, True), "PodSecurityForbidden")
        responses = [(201, pod), (401, valid), (422, valid), (403, None), (403, PRIVATE), (403, []),
                     (403, {**valid, "kind": "Pod"}), (403, {**valid, "code": 200}),
                     (403, {**valid, "status": "Success"}), (403, {**valid, "reason": "Unauthorized"}),
                     (403, {**valid, "details": {"name": "foreign", "kind": "pods"}}),
                     (403, {**valid, "message": "User cannot create resource pods " + PRIVATE}),
                     (403, {**valid, "message": 'admission webhook denied ' + PRIVATE}),
                     (403, {**valid, "message": valid["message"].replace("v1.31", "v1.30")}),
                     (403, {**valid, "message": valid["message"].replace("seccompProfile", PRIVATE)})]
        for code, body in responses:
            with self.subTest(code=code, body=type(body).__name__), self.assertRaises(probe.ProbeFailure) as error:
                probe.admission_result(code, body, pod, True)
            self.assertNotIn(PRIVATE, str(error.exception))

    def test_positive_requires_201_for_exact_pod_and_preserved_spec(self):
        pod = probe.pod_from_template(objects()[1]["spec"]["template"], "fixture", "job-current")
        for code, body in [(200, pod), (202, pod), (201, None), (201, []), (201, PRIVATE),
                           (201, {"kind": "Status", "status": "Success"}),
                           (201, {**pod, "metadata": {"name": "foreign", "namespace": "fixture"}}),
                           (201, {**pod, "spec": {}})]:
            with self.subTest(code=code), self.assertRaises(probe.ProbeFailure):
                probe.admission_result(code, body, pod, False)

    def test_foreign_sources_bad_generations_and_mismatched_specs_do_not_create_namespace(self):
        def mutated(kind, action):
            cluster = Cluster()
            action(next(obj for obj in cluster.sources.values() if obj["kind"] == kind))
            return cluster
        cases = [
            mutated("Job", lambda obj: obj["metadata"]["ownerReferences"][0].update(uid="foreign")),
            mutated("CronJob", lambda obj: obj["metadata"].update(ownerReferences=[])),
            mutated("KarsEval", lambda obj: obj["metadata"].update(uid="replacement")),
            mutated("Job", lambda obj: obj["metadata"].update(generation=None)),
            mutated("CronJob", lambda obj: obj["metadata"].update(generation=True)),
            mutated("Job", lambda obj: obj["metadata"].update(resourceVersion="")),
            mutated("KarsEval", lambda obj: obj["metadata"].update(deletionTimestamp="terminating")),
            mutated("KarsEval", lambda obj: obj["spec"].update(schedule="* * * * *")),
            mutated("CronJob", lambda obj: obj["spec"]["jobTemplate"]["spec"]["template"]["spec"].update(nodeName="foreign")),
            mutated("Job", lambda obj: obj["spec"]["template"]["spec"]["containers"][0]["args"].extend(["--corpus-label", PRIVATE])),
        ]
        for cluster in cases:
            with self.subTest(), self.assertRaises(probe.ProbeFailure):
                self.run_probe(cluster)
            self.assertTrue(all(method == "GET" for method, _, _ in cluster.calls))

    def test_source_read_http_and_non_object_failures_do_not_write(self):
        for response in ((500, {"message": PRIVATE}), (403, {"kind": "Status"}),
                         (200, None), (200, PRIVATE), (200, {"kind": "KarsEval"})):
            with patch.object(probe.api, "request", return_value=response) as request, \
                 self.assertRaises(probe.ProbeFailure):
                probe.probe(1234, JOB)
            self.assertTrue(all(call.args[1] == "GET" for call in request.call_args_list))

    def test_invalid_job_name_never_reaches_the_api(self):
        for name in ("customer-job", "../other", f"{JOB}?dryRun=false"):
            with patch.object(probe.api, "request") as request, self.assertRaises(probe.ProbeFailure):
                probe.probe(1234, name)
            request.assert_not_called()

    def test_each_source_uid_generation_owner_or_spec_change_is_fatal_and_cleans_owned_namespace(self):
        for kind in ("KarsEval", "Job", "CronJob"):
            for field in ("uid", "generation", "ownerReferences", "spec"):
                cluster = Cluster()
                def change(method, path, _obj):
                    if "/pods?" in path:
                        source = next(obj for obj in cluster.sources.values() if obj["kind"] == kind)
                        if field == "spec":
                            source["spec"]["changed"] = True
                        else:
                            source["metadata"][field] = {"uid": "replacement", "generation": 8,
                                                         "ownerReferences": []}[field]
                cluster.after = change
                with self.subTest(kind=kind, field=field), self.assertRaisesRegex(probe.ProbeFailure, "source-continuity"):
                    self.run_probe(cluster)
                self.assertIsNone(cluster.namespace)
                self.assertEqual(len([1 for _, path, _ in cluster.calls if "/pods?" in path]), 1)

    def test_status_only_source_rv_updates_do_not_invalidate_spec_continuity(self):
        cluster = Cluster()
        def update(_method, _path, _obj):
            for source in cluster.sources.values():
                source["metadata"]["resourceVersion"] = str(int(source["metadata"]["resourceVersion"]) + 1)
                source["status"] = {"changing": True}
        cluster.after = update
        self.assertEqual(len(self.run_probe(cluster)), 4)

    def test_source_change_during_cleanup_cannot_report_a_completed_proof(self):
        cluster = Cluster()
        def change(method, _path, _obj):
            if method == "DELETE":
                next(iter(cluster.sources.values()))["spec"]["changed"] = True
        cluster.after = change
        with self.assertRaisesRegex(probe.ProbeFailure, "source-continuity"):
            self.run_probe(cluster)
        self.assertIsNone(cluster.namespace)

    def test_namespace_name_collision_does_not_adopt_or_delete(self):
        cluster = Cluster()
        cluster.namespace = {"kind": "Namespace", "metadata": {"uid": "customer"}}
        with self.assertRaisesRegex(probe.ProbeFailure, "namespace-not-fresh"):
            self.run_probe(cluster)
        self.assertTrue(all(method == "GET" for method, _, _ in cluster.calls))

    def test_namespace_lookup_failure_is_not_treated_as_absence(self):
        for response in ((404, None), (404, {"kind": "Status", "message": PRIVATE}),
                         (403, status(403, "Forbidden", "fixture", "namespaces")),
                         (500, {"message": PRIVATE})):
            cluster = Cluster()
            def request(port, method, path, obj=None):
                if path.startswith(probe.NAMESPACES):
                    cluster.calls.append((method, path, obj))
                    return response
                return cluster.request(port, method, path, obj)
            with self.subTest(code=response[0]), patch.object(probe.api, "request", side_effect=request), \
                 self.assertRaisesRegex(probe.ProbeFailure, "namespace-not-fresh"):
                probe.probe(1234, JOB)
            self.assertTrue(all(method == "GET" for method, _, _ in cluster.calls))

    def test_unproven_namespace_create_response_never_authorizes_cleanup(self):
        for response in ((409, status(409, "AlreadyExists", "foreign", "namespaces")),
                         (201, None), (201, {"kind": "Namespace", "metadata": {"uid": "foreign"}})):
            cluster = Cluster()
            def request(port, method, path, obj=None):
                if method == "POST" and path == probe.NAMESPACES:
                    cluster.calls.append((method, path, obj))
                    return response
                return cluster.request(port, method, path, obj)
            with self.subTest(code=response[0]), patch.object(probe.api, "request", side_effect=request), \
                 self.assertRaises(probe.ProbeFailure):
                probe.probe(1234, JOB)
            self.assertFalse(any(method == "DELETE" or "/pods?" in path for method, path, _ in cluster.calls))

    def test_cleanup_uses_latest_rv_but_never_a_replacement_uid_or_changed_spec(self):
        for field in ("uid", "spec", "label"):
            cluster = Cluster()
            def change(_method, path, _obj):
                if "/pods?" in path:
                    if field == "uid":
                        cluster.namespace["metadata"]["uid"] = "foreign"
                    elif field == "label":
                        cluster.namespace["metadata"]["labels"][probe.PROOF_LABEL] = "foreign"
                    else:
                        cluster.namespace["spec"]["finalizers"].append("foreign")
            cluster.after = change
            with self.subTest(field=field), self.assertRaises(probe.ProbeFailure):
                self.run_probe(cluster)
            self.assertFalse(any(method == "DELETE" for method, _, _ in cluster.calls))
        cluster = Cluster()
        def advance(_method, path, _obj):
            if "/pods?" in path:
                cluster.namespace["metadata"]["resourceVersion"] = "99"
        cluster.after = advance
        self.run_probe(cluster)
        deleted = next(obj for method, _, obj in cluster.calls if method == "DELETE")
        self.assertEqual(deleted["preconditions"], {"uid": "owned-namespace", "resourceVersion": "99"})
        self.assertNotIn("gracePeriodSeconds", deleted)

    def test_failure_still_cleans_owned_namespace_but_unexpected_delete_response_is_fatal(self):
        cluster = Cluster()
        cluster.pod_response = lambda _obj: (500, {"message": PRIVATE})
        with self.assertRaises(probe.ProbeFailure):
            self.run_probe(cluster)
        self.assertIsNone(cluster.namespace)
        for response in ((409, status(409, "Conflict", "fixture", "namespaces")),
                         (202, None), (200, {"kind": "Status", "status": "Success"}),
                         (202, {"kind": "Namespace", "metadata": {"uid": "foreign"}})):
            cluster = Cluster()
            cluster.delete_response = response
            with self.subTest(response=response[0]), self.assertRaises(probe.ProbeFailure):
                self.run_probe(cluster)

    def test_default_service_account_wait_is_bounded_and_errors_are_fatal(self):
        cluster = Cluster()
        cluster.service_account_response = (404, status(404, "NotFound", "default", "serviceaccounts"))
        with patch.object(probe.time, "monotonic", side_effect=[0, 31, 31]), self.assertRaisesRegex(
                probe.ProbeFailure, "service-account-timeout"):
            self.run_probe(cluster)
        for response in ((403, status(403, "Forbidden", "default", "serviceaccounts")),
                         (404, None), (200, {"kind": "ServiceAccount", "metadata": {}})):
            cluster = Cluster()
            cluster.service_account_response = response
            with self.subTest(code=response[0]), self.assertRaises(probe.ProbeFailure):
                self.run_probe(cluster)
            self.assertIsNone(cluster.namespace)

    def test_waits_for_default_service_account_without_changing_the_produced_spec(self):
        cluster = Cluster()
        cluster.service_account_response = (404, status(404, "NotFound", "default", "serviceaccounts"))
        def ready(_method, path, _obj):
            if path.endswith("/serviceaccounts/default"):
                cluster.service_account_response = None
        cluster.after = ready
        with patch.object(probe.time, "sleep") as sleep:
            self.assertEqual(len(self.run_probe(cluster)), 4)
        sleep.assert_called_once_with(0.2)

    def test_namespace_replacement_after_delete_is_not_deleted_again(self):
        cluster = Cluster()
        def replace(method, path, _obj):
            if method == "DELETE":
                name = path.rsplit("/", 1)[1]
                cluster.namespace = {
                    "apiVersion": "v1", "kind": "Namespace", "spec": {"finalizers": ["kubernetes"]},
                    "metadata": {"name": name, "uid": "foreign", "resourceVersion": "100",
                                 "labels": {**probe.PSS_LABELS, probe.PROOF_LABEL: name.removeprefix("kars-eval-admission-")}},
                }
        cluster.after = replace
        with self.assertRaisesRegex(probe.ProbeFailure, "namespace-delete-replacement"):
            self.run_probe(cluster)
        self.assertEqual(len([1 for method, _, _ in cluster.calls if method == "DELETE"]), 1)

    def test_non_kind_or_non_loopback_context_cannot_start_proxy_or_write(self):
        for context, server in (("production", "https://127.0.0.1:6443"),
                                (probe.api.CONTEXT, "https://customer.example:6443"),
                                (probe.api.CONTEXT, "https://10.0.0.1:6443")):
            config = {"contexts": [{"name": context}], "clusters": [{"cluster": {"server": server}}]}
            with patch.object(probe.api, "command", return_value=json.dumps(config)), \
                 patch.object(probe.api.subprocess, "Popen") as process, \
                 patch.object(probe.api, "request") as request, contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(probe.main(["--job", JOB]), 1)
            process.assert_not_called()
            request.assert_not_called()

    def test_wrong_api_version_prevents_all_probe_writes(self):
        with patch.object(probe.api, "kind_proxy", return_value=contextlib.nullcontext(
                (1, {"gitVersion": "v1.30.0"}))), patch.object(probe.api, "request") as request, \
                contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(probe.main(["--job", JOB]), 1)
        request.assert_not_called()

    def test_existing_proxy_helper_cleans_only_its_owned_process_on_failure(self):
        config = {"contexts": [{"name": probe.api.CONTEXT}],
                  "clusters": [{"cluster": {"server": "https://127.0.0.1:6443"}}]}
        process = MagicMock()
        process.poll.return_value = None
        listener = MagicMock()
        listener.__enter__.return_value.getsockname.return_value = ("127.0.0.1", 32145)
        with patch.object(probe.api, "command", return_value=json.dumps(config)), \
             patch.object(probe.api.socket, "socket", return_value=listener), \
             patch.object(probe.api.subprocess, "Popen", return_value=process) as start, \
             patch.object(probe.api, "request", return_value=(200, {"gitVersion": "v1.31.0"})):
            with self.assertRaisesRegex(RuntimeError, "fixed-test-failure"):
                with probe.api.kind_proxy(Path(".")):
                    raise RuntimeError("fixed-test-failure")
        process.terminate.assert_called_once_with()
        process.wait.assert_called_once_with(timeout=5)
        self.assertIn("--address=127.0.0.1", start.call_args.args[0])
        self.assertIn(probe.api.CONTEXT, start.call_args.args[0])

    def test_main_emits_only_fixed_status_facts_and_hides_transport_bodies(self):
        for error in (URLError(PRIVATE), ValueError(PRIVATE), OSError(PRIVATE)):
            output = io.StringIO()
            with patch.object(probe.api, "kind_proxy", side_effect=error), contextlib.redirect_stdout(output):
                self.assertEqual(probe.main(["--job", JOB]), 1)
            self.assertNotIn(PRIVATE, output.getvalue())
        cluster = Cluster()
        output = io.StringIO()
        with patch.object(probe.api, "kind_proxy", return_value=contextlib.nullcontext(
                (1, {"gitVersion": "v1.31.0"}))), \
             patch.object(probe.api, "request", side_effect=cluster.request), \
             contextlib.redirect_stdout(output):
            self.assertEqual(probe.main(["--job", JOB]), 0)
        report = json.loads(output.getvalue().split(" ", 1)[1])
        self.assertEqual(set(report), {"result", "policy", "cases"})
        self.assertEqual(len(report["cases"]), 4)
        self.assertTrue(all(set(case) == {"producer", "variant", "httpStatus", "result"} for case in report["cases"]))
        for forbidden in (PRIVATE, "uid", "args", "image", "corpus.json", "fixture-router"):
            self.assertNotIn(forbidden, output.getvalue())

    def test_hook_consumes_discovered_job_before_schedule_clear_and_existing_ci_runs_unit_fences(self):
        root = Path(__file__).resolve().parents[2]
        shell = (root / "tests/e2e/run.sh").read_text()
        lifecycle = shell.split("test_crd_kars_eval_lifecycle() {", 1)[1].split("\n}\n", 1)[0]
        hook = 'python3 -m eval_pod_admission --job "$job_name"'
        self.assertIn(hook, lifecycle)
        self.assertLess(lifecycle.index('wait_for_resource cronjob'), lifecycle.index(hook))
        self.assertLess(lifecycle.index(hook), lifecycle.index("# ---- clearing the schedule"))
        self.assertIn("eval_pod_admission_test", shell)
        self.assertIn("eval_pod_admission_test", (root / ".github/workflows/ci.yml").read_text())


if __name__ == "__main__":
    unittest.main()
