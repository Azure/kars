# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json
import ssl
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

import audit_evidence as audit
from native_api import Api, CommandFailure, Failure


def identifier(number):
    return f"00000000-0000-4000-8000-{number:012d}"


def event(number, verb="get", resource="karscredentialgrants", code=200):
    return {"auditID": identifier(number), "stage": "ResponseComplete", "verb": verb,
            "user": {"username": f"system:serviceaccount:{audit.BRIDGE}:{audit.WRITER}"},
            "objectRef": {"namespace": "work", "resource": resource, "name": "workspace"},
            "responseStatus": {"code": code}}


class AuditEvidenceTests(unittest.TestCase):
    def test_retained_rotation_file_is_read_and_duplicate_snapshot_records_are_not_double_counted(self):
        old = audit.DIRECTORY + "/audit-2026-09-16T10-30-00.001.log"
        active = audit.DIRECTORY + "/audit.log"
        created = event(1, "create", "secrets", 201)
        raw = "\n".join(json.dumps(value) for value in [created, created, event(2)])
        with patch.object(audit, "paths", return_value=[old, active]), \
             patch.object(audit, "command", return_value=raw) as command:
            values = audit.read("work")
        self.assertEqual(values, [created, event(2)])
        command.assert_called_once_with("docker", "exec", audit.CONTROL_PLANE,
                                        "cat", old, active, timeout=30)

    def test_changed_rotation_inventory_retries_instead_of_returning_partial_coverage(self):
        active = audit.DIRECTORY + "/audit.log"
        first = [audit.DIRECTORY + "/audit-2026-09-16T10-30-00.001.log", active]
        second = [audit.DIRECTORY + "/audit-2026-09-16T10-31-00.001.log", active]
        with patch.object(audit, "paths", side_effect=[first, second, second, second]), \
             patch.object(audit, "command", side_effect=[json.dumps(event(1)), json.dumps(event(2))]):
            self.assertEqual(audit.read("work"), [event(2)])

    def test_distinct_creates_are_retained_and_conflicting_duplicate_ids_are_refused(self):
        one, two = event(1, "create", "secrets", 201), event(2, "create", "secrets", 201)
        self.assertEqual(len(audit.parse_events(json.dumps(one) + "\n" + json.dumps(two), "work")), 2)
        bad = copy.deepcopy(one)
        bad["responseStatus"]["code"] = 403
        with self.assertRaisesRegex(Failure, "conflicting"):
            audit.parse_events(json.dumps(one) + "\n" + json.dumps(bad), "work")

    def test_scope_filtering_and_bounded_inventory_fail_closed(self):
        other = event(1)
        other["objectRef"]["namespace"] = "other"
        self.assertEqual(audit.parse_events(json.dumps(other), "work"), [])
        with patch.object(audit, "MAX_BYTES", 1), self.assertRaises(Failure):
            audit.parse_events(json.dumps(event(1)), "work")
        for paths in ("", audit.DIRECTORY + "/audit-private.log", "/elsewhere/audit.log"):
            with patch.object(audit, "command", return_value=paths), self.assertRaises(Failure):
                audit.paths()
        with patch.object(audit, "command", side_effect=CommandFailure("docker", 1, "private")), \
             self.assertRaisesRegex(Failure, "did not settle"):
            audit.read("work")

    def test_barrier_waits_for_its_exact_successful_response_complete_event(self):
        marker = identifier(3)
        actor = SimpleNamespace(audit_marker=Mock(return_value=marker))
        setup = SimpleNamespace(audit=Mock(side_effect=[[], [event(2), event(3)]]))

        def wait(description, check, timeout):
            self.assertEqual(timeout, 30)
            self.assertIsNone(check())
            return check()

        with patch.object(audit, "until", side_effect=wait):
            result = audit.barrier(setup, actor, "work")
        self.assertIsInstance(result, audit.AuditSnapshot)
        self.assertEqual(result.marker, marker)
        self.assertEqual(len(result), 2)

    def test_barrier_rejects_wrong_response_and_interval_requires_retained_start(self):
        actor = SimpleNamespace(audit_marker=lambda path: identifier(1))
        setup = SimpleNamespace(audit=lambda namespace: [event(1, code=403)])
        with patch.object(audit, "until", side_effect=lambda description, check, timeout: check()), \
             self.assertRaisesRegex(Failure, "barrier"):
            audit.barrier(setup, actor, "work")
        before = audit.AuditSnapshot([event(1)], identifier(1))
        after = audit.AuditSnapshot([event(1), event(2), event(3)], identifier(3))
        self.assertEqual(audit.changes(before, after), [event(2), event(3)])
        with self.assertRaisesRegex(Failure, "starting barrier"):
            audit.changes(before, audit.AuditSnapshot([event(3)], identifier(3)))
        with self.assertRaisesRegex(Failure, "distinct"):
            audit.changes(before, before)

    def test_actual_api_client_reads_the_server_audit_header_without_changing_its_return_contract(self):
        response = Mock(status=200)
        response.read.return_value = b'{"metadata":{"name":"workspace"}}'
        response.getheader.return_value = identifier(7)
        connection = Mock()
        connection.getresponse.return_value = response
        with patch("native_api.http.client.HTTPSConnection", return_value=connection):
            api = Api("https://127.0.0.1:6443", ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT))
            self.assertEqual(api.audit_marker("/fixture"), identifier(7))
            self.assertEqual(api.get("/fixture"), {"metadata": {"name": "workspace"}})
            response.getheader.return_value = "private-invalid-audit-header"
            with self.assertRaisesRegex(Failure, "audit request identity") as failure:
                api.audit_marker("/fixture")
            self.assertNotIn("private-invalid", str(failure.exception))


if __name__ == "__main__":
    unittest.main()
