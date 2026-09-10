# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Unit checks for the nonexecuting native RC admission probe orchestration."""

import copy
import unittest

from sre_authority.admission import replication_controller_cases
from sre_authority.common import PRIVATE, RUNTIME


class Response:
    def __init__(self, code, body):
        self.status_code = code
        self.body = body

    def json(self):
        return copy.deepcopy(self.body)


class Harness:
    def __init__(self, *, allow_private=False, wrong_denial=False, conflict=False, replace=False):
        self.calls = []
        self.object = None
        self.completed = []
        self.allow_private = allow_private
        self.wrong_denial = wrong_denial
        self.conflict = conflict
        self.replace = replace

    def passed(self, message):
        self.completed.append(message)

    def poll(self, _message, operation, **_options):
        if not operation():
            raise AssertionError("fixture polling did not converge")

    def api(self, method, path, *, body=None, user="admin", status=None):
        self.calls.append((method, path, copy.deepcopy(body), user))
        if "selfsubjectaccessreviews" in path:
            attributes = body["spec"]["resourceAttributes"]
            allowed = attributes["resource"] == "replicationcontrollers" or attributes.get("subresource") == "log"
            response = Response(201, {"status": {"allowed": allowed}})
        elif method == "GET":
            if self.object is not None and self.replace:
                self.object["metadata"]["uid"] = "replacement"
            response = Response(200, self.object) if self.object is not None else Response(404, {})
        elif method == "DELETE":
            assert body["preconditions"] == {
                "uid": self.object["metadata"]["uid"],
                "resourceVersion": self.object["metadata"]["resourceVersion"],
            }
            if self.conflict:
                self.conflict = False
                self.object["metadata"]["resourceVersion"] = "2"
                response = Response(409, {"reason": "Conflict"})
            else:
                self.object = None
                response = Response(200, {})
        elif "?dryRun=All" in path:
            if "template" not in body["spec"]:
                response = Response(422, {"kind": "Status", "reason": "Invalid", "details": {"causes": [
                    {"field": "spec.template", "reason": "FieldValueRequired"},
                ]}})
                if status is not None:
                    assert response.status_code in (status if isinstance(status, tuple) else (status,))
                return response
            pod = body["spec"].get("template", {}).get("spec", {})
            private = bool(pod.get("volumes")) or pod.get("serviceAccountName") == "sre-api-router"
            containers = pod.get("containers", []) + pod.get("initContainers", [])
            private = private or any(container.get("envFrom") or any(
                item.get("valueFrom", {}).get("secretKeyRef", {}).get("name") == PRIVATE
                for item in container.get("env", [])) for container in containers)
            if private and not self.allow_private:
                policy = "unrelated" if self.wrong_denial else "kars-sre-private-workloads"
                response = Response(403, {"kind": "Status", "reason": "Forbidden",
                                          "message": f"{policy} denied request"})
            else:
                response = Response(201 if method == "POST" else 200, body)
        else:
            assert method == "POST" and body["spec"]["replicas"] == 0
            assert body["spec"]["template"]["spec"].get("volumes") is None
            self.object = copy.deepcopy(body)
            self.object["metadata"].update(uid="owned-rc", resourceVersion="1")
            response = Response(201, self.object)
        if status is not None:
            assert response.status_code in (status if isinstance(status, tuple) else (status,))
        return response


class ReplicationControllerProofTests(unittest.TestCase):
    def test_all_private_forms_are_denied_for_create_and_update_without_workload_execution(self):
        harness = Harness()
        replication_controller_cases(harness)
        self.assertEqual(len(harness.completed), 1)
        self.assertIsNone(harness.object)
        attempts = [call for call in harness.calls if call[0] in ("POST", "PUT")
                    and "replicationcontrollers" in call[1] and "?dryRun=All" in call[1]]
        self.assertEqual(len(attempts), 15)
        self.assertEqual(sum(method == "PUT" for method, *_ in attempts), 7)
        for _method, _path, body, user in attempts:
            self.assertEqual(user, "tenant")
            self.assertEqual(body["spec"]["replicas"], 0)
            if "template" in body["spec"]:
                pod = body["spec"]["template"]["spec"]
                self.assertEqual(pod["schedulerName"], "kars-e2e-admission-never-schedule")
                self.assertEqual(pod["containers"][0]["command"], ["/bin/true"])
                self.assertEqual(pod["containers"][0]["imagePullPolicy"], "Never")
        self.assertFalse(any("/secrets/" in path or "/pods/" in path for _, path, *_ in harness.calls))
        rights = [body["spec"]["resourceAttributes"] for _, path, body, _ in harness.calls
                  if "selfsubjectaccessreviews" in path]
        self.assertIn({"group": "", "resource": "pods", "verb": "get", "namespace": RUNTIME,
                       "subresource": "log"}, rights)
        self.assertIn({"group": "", "resource": "pods", "verb": "create", "namespace": ""}, rights)
        self.assertTrue(any(PRIVATE in str(body) for _, _, body, _ in attempts))

    def test_an_admitted_private_controller_or_wrong_denial_never_counts_as_success(self):
        for options in ({"allow_private": True}, {"wrong_denial": True}):
            harness = Harness(**options)
            with self.assertRaises(AssertionError):
                replication_controller_cases(harness)
            self.assertFalse(harness.completed)
            self.assertIsNone(harness.object)

    def test_cleanup_conflict_uses_a_fresh_version_without_dropping_uid_fence(self):
        harness = Harness(conflict=True)
        replication_controller_cases(harness)
        deletions = [body for method, _, body, _ in harness.calls if method == "DELETE"]
        self.assertEqual([body["preconditions"] for body in deletions], [
            {"uid": "owned-rc", "resourceVersion": "1"},
            {"uid": "owned-rc", "resourceVersion": "2"},
        ])

    def test_replacement_refuses_update_and_cleanup_of_the_foreign_object(self):
        harness = Harness(replace=True)
        with self.assertRaises(AssertionError):
            replication_controller_cases(harness)
        self.assertFalse(any(method in ("PUT", "DELETE") for method, *_ in harness.calls))
        self.assertFalse(harness.completed)


if __name__ == "__main__":
    unittest.main()
