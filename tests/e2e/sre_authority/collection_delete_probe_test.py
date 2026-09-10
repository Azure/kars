# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json
import unittest
from urllib.parse import parse_qs, urlsplit
from unittest.mock import patch

from sre_authority.collection_delete_probe import NAMESPACE_CONTROLLER, cases, dry_run_delete


class CollectionDeleteProofTests(unittest.TestCase):
    def test_dry_run_retries_only_status_only_rv_conflicts_and_keeps_both_preconditions(self):
        original = {"metadata": {"uid": "owned", "resourceVersion": "1"}, "spec": {"replicas": 0}}
        updated = copy.deepcopy(original)
        updated["metadata"]["resourceVersion"] = "2"
        updated["status"] = {"observedGeneration": 1}
        with patch("sre_authority.collection_delete_probe.request", return_value=(200, updated)), \
                patch("sre_authority.collection_delete_probe.as_tenant",
                      side_effect=[(409, {"reason": "Conflict"}), (200, {"kind": "Status"})]) as actor, \
                patch("sre_authority.collection_delete_probe.time.sleep"):
            code, _ = dry_run_delete(1, "/collection?fieldSelector=name", "/item", original, False)
        self.assertEqual(code, 200)
        self.assertEqual([call.args[2]["preconditions"] for call in actor.call_args_list], [
            {"uid": "owned", "resourceVersion": "1"}, {"uid": "owned", "resourceVersion": "2"}])
        self.assertTrue(all(call.args[2]["dryRun"] == ["All"] for call in actor.call_args_list))
        self.assertEqual(original["metadata"]["resourceVersion"], "1")

    def test_dry_run_does_not_retry_replacement_content_change_or_authorization_failure(self):
        original = {"metadata": {"uid": "owned", "resourceVersion": "1"}, "spec": {"replicas": 0}}
        for refreshed in (
            {"metadata": {"uid": "other", "resourceVersion": "2"}, "spec": {"replicas": 0}},
            {"metadata": {"uid": "owned", "resourceVersion": "2"}, "spec": {"replicas": 1}},
        ):
            with patch("sre_authority.collection_delete_probe.request", return_value=(200, refreshed)), \
                    patch("sre_authority.collection_delete_probe.as_tenant",
                          return_value=(409, {"reason": "Conflict"})) as actor, self.assertRaises(AssertionError):
                dry_run_delete(1, "/collection", "/item", original, False)
            actor.assert_called_once()
        with patch("sre_authority.collection_delete_probe.request") as read, \
                patch("sre_authority.collection_delete_probe.as_tenant",
                      return_value=(403, {"reason": "Forbidden"})) as actor:
            self.assertEqual(dry_run_delete(1, "/collection", "/item", original, False)[0], 403)
        actor.assert_called_once()
        read.assert_not_called()

    def fixture(self):
        objects, deletes, probes = {}, [], []
        def api(_port, method, path, obj=None):
            parsed = urlsplit(path)
            if method == "POST":
                value = copy.deepcopy(obj)
                value["metadata"].update(uid=value["metadata"]["name"] + "-uid", resourceVersion="1")
                objects[path + "/" + value["metadata"]["name"]] = value
                return 201, copy.deepcopy(value)
            if method == "GET":
                return 200, copy.deepcopy(objects[path])
            if parsed.query:
                name = parse_qs(parsed.query)["fieldSelector"][0].removeprefix("metadata.name=")
                target = parsed.path + "/" + name
                self.assertEqual(obj["dryRun"], ["All"])
            else:
                target = path
            self.assertEqual(obj["preconditions"], {
                "uid": objects[target]["metadata"]["uid"], "resourceVersion": objects[target]["metadata"]["resourceVersion"]})
            if not obj.get("dryRun"):
                deletes.append(path)
                del objects[path]
            return 200, {"kind": "Status", "status": "Success"}
        def actor(_port, path, obj, **kwargs):
            self.assertEqual(kwargs, {"user": NAMESPACE_CONTROLLER, "method": "DELETE"})
            self.assertEqual(obj["dryRun"], ["All"])
            probes.append((path, obj))
            name = parse_qs(urlsplit(path).query)["fieldSelector"][0].removeprefix("metadata.name=")
            policy = {"sre-api-router": "kars-sre-private-identity",
                      "sre-api-self-renew": "kars-sre-role-authority",
                      "sre": "kars-sre-consumer-authority"}.get(name)
            if policy:
                return 403, {"kind": "Status", "reason": "Forbidden", "message": policy + " denied do-not-publish"}
            return 200, {"kind": "Status", "status": "Success"}
        return objects, deletes, probes, api, actor

    def test_collections_are_body_dry_runs_with_specific_guard_denials_and_owned_cleanup(self):
        objects, deletes, probes, api, actor = self.fixture()
        policies = {name: {} for name in (
            "kars-sre-private-identity", "kars-sre-role-authority", "kars-sre-consumer-authority")}
        with patch("sre_authority.collection_delete_probe.request", side_effect=api), \
                patch("sre_authority.collection_delete_probe.as_tenant", side_effect=actor):
            results = cases(1, policies)
        self.assertEqual(len(results), 9)
        self.assertTrue(all(result["matched"] for result in results))
        self.assertEqual(len(probes), 6)
        self.assertEqual(len(deletes), 6)
        self.assertEqual(objects, {})
        self.assertNotIn("do-not-publish", json.dumps(results))

    def test_cel_missing_name_errors_are_not_intended_authority_denials(self):
        _, _, _, api, _ = self.fixture()
        policies = {name: {} for name in (
            "kars-sre-private-identity", "kars-sre-role-authority", "kars-sre-consumer-authority")}
        body = {"kind": "Status", "reason": "Forbidden",
                "message": "kars-sre-private-identity kars-sre-role-authority kars-sre-consumer-authority "
                           "evaluation failed: no such key: name do-not-publish"}
        with patch("sre_authority.collection_delete_probe.request", side_effect=api), \
                patch("sre_authority.collection_delete_probe.as_tenant", return_value=(403, body)):
            results = cases(1, policies)
        self.assertFalse(any(result["matched"] for result in results if "namespace-controller" in result["case"]))
        self.assertNotIn("do-not-publish", json.dumps(results))
