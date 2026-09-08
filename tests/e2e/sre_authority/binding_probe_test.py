# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pure privacy/request-shape checks; the A/B result requires actual Kind."""

import base64
import copy
import json
from pathlib import Path
import types
import unittest
from unittest.mock import patch

from sre_authority.binding_probe import (
    CONTROLLER, LEGACY, RETIRED, SURVIVOR, ControllerAPI, authorization_category, retirement_patch,
)


class BindingProbeTests(unittest.TestCase):
    def test_patch_is_exact_subtractive_review_with_uid_rv_and_registration_fence(self):
        binding = {"metadata": {"uid": "binding-uid", "resourceVersion": "123"},
                   "subjects": [LEGACY, SURVIVOR]}
        original = copy.deepcopy(binding)
        result = retirement_patch(binding, "registration-uid")
        self.assertEqual(result, {"metadata": {"uid": "binding-uid", "resourceVersion": "123",
            "annotations": {RETIRED: "registration-uid"}}, "subjects": [SURVIVOR]})
        self.assertEqual(binding, original)
        for subjects in ([SURVIVOR], [LEGACY], [LEGACY, SURVIVOR, {"kind": "User", "name": "unreviewed"}]):
            with self.assertRaises(AssertionError):
                retirement_patch({**binding, "subjects": subjects}, "registration-uid")

    def test_arbitrary_forbidden_or_failure_is_not_escalation_proof(self):
        denied = {"kind": "Status", "reason": "Forbidden",
                  "message": "user is attempting to grant RBAC permissions not currently held: private-not-for-logs"}
        self.assertEqual(authorization_category(403, denied), "rbac-permissions-not-held")
        for message in ("cannot patch clusterrolebindings", "kars-sre-binding-authority denied", None, {}, ""):
            self.assertEqual(authorization_category(403, {**denied, "message": message}), "other-forbidden")
        for code in (200, 201, 401, 404, 409, 422, 500):
            self.assertEqual(authorization_category(code, denied), "unexpected-response")
        self.assertEqual(authorization_category(200, {"kind": "ClusterRoleBinding"}), "accepted")
        self.assertEqual(authorization_category(409, {"kind": "Status", "reason": "Conflict"}), "cas-conflict")
        self.assertEqual(authorization_category(403, None), "unexpected-response")
        self.assertNotIn("private-not-for-logs", authorization_category(403, denied))

    def test_controller_token_is_memory_only_no_admin_certificate_and_real_uid_is_required(self):
        account = {"metadata": {"uid": "actual-controller-uid"}}
        captured = []
        def api_request(_self, method, path, obj):
            captured.append((method, path, obj))
            return 201, {"status": {"userInfo": {"username": CONTROLLER, "uid": "actual-controller-uid"}}}
        def config(stage, *_args, **_kwargs):
            return "https://127.0.0.1:6443" if stage == "public-api-server" else base64.b64encode(b"public CA").decode()
        context = types.SimpleNamespace()
        with patch("sre_authority.binding_probe.command", side_effect=config), \
                patch("sre_authority.binding_probe.get", return_value=account), \
                patch("sre_authority.binding_probe.request", return_value=(201, {"status": {"token": "unit-only-token"}})), \
                patch("sre_authority.binding_probe.ssl.create_default_context", return_value=context) as ssl_context, \
                patch("sre_authority.binding_probe.HTTPSHandler") as https, \
                patch("sre_authority.binding_probe.build_opener"), \
                patch.object(ControllerAPI, "request", api_request):
            client = ControllerAPI(Path("."), 1)
            self.assertEqual(client.account, account)
            ssl_context.assert_called_once_with(cadata="public CA")
            https.assert_called_once_with(context=context)
            self.assertTrue(captured[0][1].endswith("/selfsubjectreviews"))
            with patch.object(ControllerAPI, "request", return_value=(201, {
                    "status": {"userInfo": {"username": CONTROLLER, "uid": "replacement"}}})):
                with self.assertRaises(AssertionError):
                    ControllerAPI(Path("."), 1)

    def test_controller_dry_run_has_real_bearer_and_merge_patch_without_impersonation(self):
        class Response:
            code = 403
            def __enter__(self):
                return self
            def __exit__(self, *_args):
                pass
            def read(self, _limit):
                return b'{"kind":"Status","reason":"Forbidden"}'
        sent = []
        def open_request(request, **_kwargs):
            sent.append(request)
            return Response()
        client = ControllerAPI.__new__(ControllerAPI)
        client.server, client.token = "https://127.0.0.1:6443", "unit-only-token"
        client.opener = types.SimpleNamespace(open=open_request)
        code, _ = client.request("PATCH", "/fixture?dryRun=All", {"subjects": [SURVIVOR]})
        self.assertEqual(code, 403)
        self.assertEqual(sent[0].get_header("Authorization"), "Bearer unit-only-token")
        self.assertEqual(sent[0].get_header("Content-type"), "application/merge-patch+json")
        self.assertFalse(any(key.lower().startswith("impersonate") for key in sent[0].headers))
        self.assertEqual(json.loads(sent[0].data), {"subjects": [SURVIVOR]})

    def test_fast_hosted_gate_keeps_prior_admission_cases_and_adds_real_bind_experiment(self):
        root = Path(__file__).resolve().parents[3]
        workflow = (root / ".github/workflows/ci.yml").read_text()
        fast = workflow.split("  sre-crd-schema:", 1)[1].split("  helm-lint:", 1)[0]
        self.assertIn("sre_authority.bootstrap_probe --retirement-bind-proof", fast)
        self.assertIn("sre_authority.binding_probe_test", fast)
        source = (root / "tests/e2e/sre_authority/bootstrap_probe.py").read_text()
        self.assertLess(source.index("cases = admission_cases"), source.index("prove(root, port, state"))
        probe = (root / "tests/e2e/sre_authority/binding_probe.py").read_text()
        self.assertIn('"resourceNames": [READER], "verbs": ["bind"]', probe)
        self.assertNotIn('"verbs": ["escalate"]', probe)
        self.assertNotIn('"resourceNames": ["*"]', probe)


if __name__ == "__main__":
    unittest.main()
