# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Local helper/diagnostic tests; these do not qualify API schema behavior."""

import copy
import unittest
from unittest.mock import patch

import sandbox_condition_schema as probe


class SandboxConditionSchemaTests(unittest.TestCase):
    def test_fixture_cannot_execute_and_does_not_assert_readiness(self):
        sandbox = probe.fixture("owned", "token")
        self.assertIs(sandbox["spec"]["suspended"], True)
        self.assertNotIn("status", sandbox)
        for value in (1, "not-an-integer"):
            condition = probe.probe_status(value)["conditions"][0]
            self.assertEqual(condition["type"], "SchemaProbe")
            self.assertEqual(condition["status"], "Unknown")
            self.assertEqual(condition["observedGeneration"], value)
        self.assertNotIn("observedGeneration",
                         probe.probe_status(1, include_generation=False)["conditions"][0])

    def test_denial_must_be_native_type_validation_at_the_exact_condition_field(self):
        body = {"kind": "Status", "status": "Failure", "reason": "Invalid", "details": {
            "name": "schema-probe", "group": "kars.azure.com",
            "causes": [{"field": probe.FIELD, "reason": "FieldValueTypeInvalid"}],
        }}
        self.assertTrue(probe.intended_type_denial(422, body, "schema-probe"))
        for field in ("spec.suspended", "status.observedGeneration",
                      "status.conditions[0].status", "status.conditions[1].observedGeneration"):
            changed = copy.deepcopy(body)
            changed["details"]["causes"][0]["field"] = field
            self.assertFalse(probe.intended_type_denial(422, changed, "schema-probe"))
        for code in (200, 400, 403, 409, 500):
            self.assertFalse(probe.intended_type_denial(code, body, "schema-probe"))
        self.assertFalse(probe.intended_type_denial(422, body, "another-object"))
        changed = copy.deepcopy(body)
        changed["details"]["causes"] = [{"field": probe.FIELD, "reason": "Forbidden"}]
        self.assertFalse(probe.intended_type_denial(422, changed, "schema-probe"))

    def test_cleanup_is_fenced_by_owned_uid_label_and_latest_resource_version(self):
        owned = probe.Owned(1234, "proof")
        owned.resources.append(("/owned/fixture", "owned-uid"))
        current = {"metadata": {"uid": "owned-uid", "resourceVersion": "43",
                                "labels": {probe.LABEL: "proof"}}}
        with patch.object(probe, "request", side_effect=[
            (200, current), (200, {}), (404, {}),
        ]) as request:
            owned.cleanup()
        deletion = request.call_args_list[1].args
        self.assertEqual(deletion[1:3], ("DELETE", "/owned/fixture"))
        self.assertEqual(deletion[3]["preconditions"],
                         {"uid": "owned-uid", "resourceVersion": "43"})
        for change in ({"uid": "replacement"}, {"labels": {probe.LABEL: "foreign"}}):
            changed = copy.deepcopy(current)
            changed["metadata"].update(change)
            with patch.object(probe, "request", return_value=(200, changed)) as request:
                with self.assertRaises(probe.Failure):
                    owned.cleanup()
                self.assertEqual(request.call_count, 1)

    def test_status_patch_does_not_change_intent_generation_or_unowned_identity(self):
        owned = probe.Owned(1234, "proof")
        original = probe.fixture("owned", "proof")
        original["metadata"].update(uid="owned-uid", resourceVersion="12", generation=1)
        status = probe.probe_status(1)
        with patch.object(probe, "request", side_effect=[
            (200, original), (200, {}),
        ]) as request:
            probe.patch_status(owned, "/owned/fixture", original, status, case="new-retained")
        body = request.call_args_list[1].args[3]
        self.assertEqual(body, {"metadata": {"uid": "owned-uid", "resourceVersion": "12"},
                                "status": status})
        for part, key, value in (("metadata", "uid", "foreign"),
                                 ("metadata", "generation", 2),
                                 ("spec", "suspended", False)):
            changed = copy.deepcopy(original)
            changed[part][key] = value
            with patch.object(probe, "request", return_value=(200, changed)) as request:
                with self.assertRaises(probe.Failure):
                    probe.patch_status(owned, "/owned/fixture", original, status,
                                       case="new-retained")
                self.assertEqual(request.call_count, 1)

    def test_failed_create_never_adopts_or_deletes_an_existing_object(self):
        owned = probe.Owned(1234, "proof")
        with patch.object(probe, "request", return_value=(409, {})) as request:
            with self.assertRaises(probe.Failure):
                owned.create("/fixtures", probe.fixture("owned", "proof"))
            owned.cleanup()
        self.assertEqual(request.call_count, 1)
        self.assertEqual(owned.resources, [])

    def test_diagnostics_reject_unbounded_cases_and_non_http_statuses(self):
        failure = probe.Failure("private body", "private code")
        self.assertEqual(failure.case, "complete")
        self.assertEqual(failure.code, 0)
        self.assertNotIn("private", str(failure))


if __name__ == "__main__":
    unittest.main()
