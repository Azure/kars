# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import unittest

from private_consumption import KINDS, PRIVATE, denied, pod_spec, variants, workload


class Response:
    def __init__(self, status, message):
        self.status_code, self.message = status, message

    def json(self):
        return {"message": self.message}


class PrivateConsumptionFixtures(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
