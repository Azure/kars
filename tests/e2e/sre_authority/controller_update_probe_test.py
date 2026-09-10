# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json
import unittest
from unittest.mock import patch

from sre_authority.bootstrap_cases import DEPLOYMENT_CONTROLLER, USER
from sre_authority.controller_update_probe import cases


class ControllerUpdateProofTests(unittest.TestCase):
    def fixture(self):
        parent = {"apiVersion": "apps/v1", "kind": "Deployment",
                  "metadata": {"name": "approved-parent", "namespace": "kars-sre", "uid": "parent", "resourceVersion": "1"},
                  "spec": {"replicas": 0, "template": {"spec": {
                      "containers": [{"name": "probe", "image": "registry.invalid/probe:never",
                                      "volumeMounts": [{"name": "private", "mountPath": "/private"}]}],
                      "volumes": [{"name": "private", "secret": {"secretName": "sre-api-router-identity"}}]}}}}
        path = "/apis/apps/v1/namespaces/kars-sre/deployments/approved-parent"
        objects = {path: copy.deepcopy(parent)}
        calls = []
        def api(_port, method, path, obj=None):
            calls.append((method, path, copy.deepcopy(obj)))
            if method == "GET":
                return 200, copy.deepcopy(objects[path])
            if method == "POST":
                created = copy.deepcopy(obj)
                created["metadata"].update(uid=created["metadata"]["name"], resourceVersion="1")
                objects[path + "/" + created["metadata"]["name"]] = created
                return 201, copy.deepcopy(created)
            if method == "DELETE":
                self.assertEqual(obj["preconditions"], {
                    "uid": objects[path]["metadata"]["uid"], "resourceVersion": objects[path]["metadata"]["resourceVersion"]})
                del objects[path]
                return 200, {"kind": "Status"}
            raise AssertionError("Unexpected API mutation")
        return parent, objects, calls, api

    def test_update_ownerrefs_use_actual_parent_and_never_persist_dry_runs(self):
        parent, objects, operations, api = self.fixture()
        outcomes = iter([403, 200, 403, 403, 200, 403])
        probes = []
        def actor(_port, path, obj, **kwargs):
            probes.append((path, copy.deepcopy(obj), kwargs))
            code = next(outcomes)
            return code, (copy.deepcopy(obj) if code == 200 else {
                "kind": "Status", "reason": "Forbidden",
                "message": "kars-sre-private-workloads denied do-not-publish"})
        with patch("sre_authority.controller_update_probe.request", side_effect=api), \
                patch("sre_authority.controller_update_probe.as_tenant", side_effect=actor):
            result = cases(1, {"kars-sre-private-workloads": {}}, parent)
        self.assertTrue(all(case["matched"] for case in result))
        self.assertNotIn("do-not-publish", json.dumps(result))
        self.assertTrue(all(path.endswith("?dryRun=All") for path, _, _ in probes))
        self.assertEqual(probes[0][2], {"user": USER, "method": "POST"})
        self.assertEqual(probes[4][2], {"user": DEPLOYMENT_CONTROLLER, "method": "PUT"})
        self.assertEqual(probes[5][1]["kind"], "Deployment")
        for index in (0, 2, 3, 4):
            self.assertEqual(probes[index][1]["metadata"]["ownerReferences"], [{
                "apiVersion": "apps/v1", "kind": "Deployment", "name": "approved-parent",
                "uid": "parent", "controller": True}])
            self.assertIn("volumes", probes[index][1]["spec"]["template"]["spec"])
        self.assertNotIn("volumes", probes[1][1]["spec"]["template"]["spec"])
        self.assertEqual(len(objects), 1)
        self.assertEqual(sum(method == "DELETE" for method, _, _ in operations), 2)

    def test_unrelated_denial_or_missing_endpoint_is_not_an_admission_pass(self):
        for code in (403, 404, 422):
            parent, _, _, api = self.fixture()
            with patch("sre_authority.controller_update_probe.request", side_effect=api), \
                    patch("sre_authority.controller_update_probe.as_tenant",
                          return_value=(code, {"kind": "Status", "reason": "Forbidden", "message": "unrelated"})):
                result = cases(1, {"kars-sre-private-workloads": {}}, parent)
            self.assertFalse(any(case["matched"] for case in result))

    def test_replaced_or_changed_approved_parent_stops_before_fixture_creation(self):
        parent, _, _, _ = self.fixture()
        for changed in ({"metadata": {"uid": "other"}, "spec": parent["spec"]},
                        {"metadata": parent["metadata"], "spec": {}}):
            with patch("sre_authority.controller_update_probe.request", return_value=(200, changed)) as api, \
                    self.assertRaises(AssertionError):
                cases(1, {}, parent)
            api.assert_called_once()
