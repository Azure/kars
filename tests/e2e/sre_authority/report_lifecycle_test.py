# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
from pathlib import Path
import unittest
from unittest.mock import Mock

from sre_authority.admission import retained_report_denials


class ReportLifecycleProofTests(unittest.TestCase):
    def fixture(self):
        namespace = {"metadata": {"uid": "namespace"}, "status": {"phase": "Active"}}
        report = {"metadata": {"name": "sre-health-report", "namespace": "kars-sre",
                              "uid": "report", "resourceVersion": "7",
                              "annotations": {"kars.azure.com/sre-registration-uid": "registration"}},
                  "data": {"report.json": "retained evidence"}}
        response = Mock(status_code=403)
        response.json.return_value = {"kind": "Status", "reason": "Forbidden",
                                      "message": "kars-sre-report-authority forbidden"}
        harness = Mock()
        harness.get.side_effect = [namespace, report, copy.deepcopy(report)]
        harness.api.return_value = response
        return harness, namespace, report

    def test_retained_report_denials_use_real_principal_and_fenced_dry_runs(self):
        harness, _, report = self.fixture()
        retained_report_denials(harness, "registration")
        harness.token_identity.assert_called_once_with(
            "namespace-controller", "kube-system", "namespace-controller")
        calls = harness.api.call_args_list
        self.assertEqual(len(calls), 4)
        self.assertEqual(calls[0].args, (
            "PUT", "/api/v1/namespaces/kars-sre/configmaps/sre-health-report?dryRun=All"))
        self.assertEqual(calls[0].kwargs["user"], "tenant")
        self.assertEqual(calls[0].kwargs["body"]["metadata"], report["metadata"])
        self.assertNotEqual(calls[0].kwargs["body"]["data"], report["data"])
        self.assertEqual(report["data"], {"report.json": "retained evidence"})
        self.assertEqual([call.kwargs["user"] for call in calls[1:]],
                         ["tenant", "namespace-controller", "namespace-controller"])
        for call in calls[1:]:
            self.assertEqual(call.args[0], "DELETE")
            self.assertEqual(call.kwargs["body"]["dryRun"], ["All"])
            self.assertEqual(call.kwargs["body"]["preconditions"],
                             {"uid": "report", "resourceVersion": "7"})
        self.assertEqual(calls[-1].args[1],
                         "/api/v1/namespaces/kars-sre/configmaps?fieldSelector=metadata.name%3Dsre-health-report")
        harness.passed.assert_called_once()

    def test_missing_or_foreign_report_and_terminating_namespace_are_not_proof(self):
        for problem in ("absent-namespace", "terminating", "deleting", "absent-report", "foreign", "deleted-report"):
            with self.subTest(problem=problem):
                harness, namespace, report = self.fixture()
                if problem == "absent-namespace":
                    namespace = None
                elif problem == "terminating":
                    namespace["status"]["phase"] = "Terminating"
                elif problem == "deleting":
                    namespace["metadata"]["deletionTimestamp"] = "2026-11-09T00:00:00Z"
                elif problem == "absent-report":
                    report = None
                elif problem == "foreign":
                    report["metadata"]["annotations"]["kars.azure.com/sre-registration-uid"] = "other"
                else:
                    report["metadata"]["deletionTimestamp"] = "2026-11-09T00:00:00Z"
                harness.get.side_effect = [namespace, report]
                with self.assertRaises(AssertionError):
                    retained_report_denials(harness, "registration")
                harness.api.assert_not_called()
                harness.passed.assert_not_called()

    def test_allowed_rbac_denied_and_cel_error_responses_are_not_intended_denials(self):
        for status, message in ((200, "accepted"), (403, "RBAC forbidden"),
                                (403, "kars-sre-report-authority evaluation failed: no such key: metadata"),
                                (403, "kars-sre-report-authority compilation failed")):
            with self.subTest(status=status, message=message):
                harness, _, _ = self.fixture()
                harness.api.return_value.status_code = status
                harness.api.return_value.json.return_value["message"] = message
                with self.assertRaises(AssertionError):
                    retained_report_denials(harness, "registration")
                harness.passed.assert_not_called()

    def test_changed_report_is_not_preserved_evidence(self):
        harness, namespace, report = self.fixture()
        changed = copy.deepcopy(report)
        changed["metadata"]["uid"] = "replacement"
        harness.get.side_effect = [namespace, report, changed]
        with self.assertRaisesRegex(AssertionError, "changed the retained report"):
            retained_report_denials(harness, "registration")
        harness.passed.assert_not_called()

    def test_maintained_uninstall_proves_report_retention_before_real_namespace_cleanup(self):
        source = (Path(__file__).parent / "migration.py").read_text()
        retirement = source.split("def retire_and_uninstall(h):", 1)[1].split("def fresh_reenrollment", 1)[0]
        self.assertLess(retirement.index('retained_report_denials(h, reg["metadata"]["uid"])'),
                        retirement.index('h.cli("uninstall", timeout=180)'))
        self.assertIn('h.poll("SRE namespace removal", lambda: h.get("namespace", RUNTIME) is None, seconds=120)',
                      retirement)
        self.assertIn('retained["metadata"]["uid"] == reg["metadata"]["uid"]', retirement)
        self.assertNotIn("finalizers", retirement)


if __name__ == "__main__":
    unittest.main()
