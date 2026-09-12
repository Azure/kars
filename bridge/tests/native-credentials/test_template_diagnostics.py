"""Prove template drift diagnostics retain only fixed, identity-bound facts."""

import copy
import hashlib
import io
import json
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

from native_api import Failure
import template_diagnostics as diagnostics

PRIVATE = "DO-NOT-EMIT-PRIVATE-TEMPLATE-VALUES"
FAILURE = "Native operator apply failed: consumer-template-drift (source=lib/private-activation:680)"


def deployment(name):
    return {"apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": name, "namespace": "work", "uid": name + "-uid", "resourceVersion": "1"},
            "spec": {"template": {"metadata": {"labels": {"app": PRIVATE}},
                                 "spec": {"containers": [{"name": "router", "env": [
                                     {"name": "PRIVATE_TEST_INPUT", "value": PRIVATE}]}]}}}}


def fixture_hashes(values):
    return [hashlib.sha256(diagnostics.encoded(diagnostics.template(value)).encode()).hexdigest()
            for value in values]


def review(value):
    return {"kind": "Deployment", "object": {key: value["metadata"][key]
            for key in ("name", "uid", "resourceVersion")}, "templateDigest": fixture_hashes([value])[0]}


class TemplateDiagnosticsTests(unittest.TestCase):
    def test_projection_distinguishes_review_drift_without_retaining_values_or_hashes(self):
        before = deployment("runtime")
        current = copy.deepcopy(before)
        current["spec"]["template"]["spec"]["containers"][0]["env"][0]["value"] += "-changed"
        facts = diagnostics.project(before, current, review(before), *fixture_hashes([before, current]))
        self.assertTrue(facts["available"])
        self.assertTrue(facts["sameUid"])
        self.assertTrue(facts["baselineMatchesReview"])
        self.assertFalse(facts["currentMatchesReview"])
        self.assertFalse(facts["sameTemplate"])
        self.assertTrue(facts["containersChanged"])
        self.assertTrue(facts["environmentChanged"])
        self.assertFalse(facts["imagesChanged"])
        self.assertFalse(facts["otherContainerFieldsChanged"])
        self.assertFalse(facts["otherPodSpecChanged"])
        self.assertTrue(all(type(value) is bool for value in facts.values()))
        self.assertNotIn(PRIVATE, json.dumps(facts))
        for value in fixture_hashes([before, current]):
            self.assertNotIn(value, json.dumps(facts))

    def test_baseline_must_match_the_actual_review_before_changes_can_be_attributed(self):
        before = deployment("runtime")
        approved = review(before)
        approved["templateDigest"] = "f" * 64
        facts = diagnostics.project(before, before, approved, *fixture_hashes([before, before]))
        self.assertFalse(facts["baselineMatchesReview"])
        self.assertFalse(facts["currentMatchesReview"])
        self.assertTrue(facts["sameTemplate"])
        replacement = copy.deepcopy(before)
        replacement["metadata"]["uid"] = "replacement"
        facts = diagnostics.project(before, replacement, review(before), *fixture_hashes([before, replacement]))
        self.assertFalse(facts["sameUid"])
        self.assertFalse(facts["currentMatchesReview"])

    def test_section_comparisons_preserve_types_and_cover_other_pod_fields(self):
        before = deployment("runtime")
        current = copy.deepcopy(before)
        before["spec"]["template"]["spec"]["hostNetwork"] = False
        current["spec"]["template"]["spec"]["hostNetwork"] = 0
        facts = diagnostics.project(before, current, review(before), *fixture_hashes([before, current]))
        self.assertTrue(facts["otherPodSpecChanged"])
        self.assertFalse(facts["containersChanged"])
        for invalid in (None, {}, {"metadata": {}, "spec": None}):
            value = copy.deepcopy(before)
            value["spec"]["template"] = invalid
            with self.assertRaises(Failure):
                diagnostics.project(value, current, review(before), "a" * 64, "b" * 64)

    def test_wrong_namespaces_owners_and_review_shapes_are_refused(self):
        before = deployment("runtime")
        for key in ("namespace", "name"):
            current = copy.deepcopy(before)
            current["metadata"][key] = "foreign"
            with self.assertRaises(Failure):
                diagnostics.project(before, current, review(before), "a" * 64, "b" * 64)
        for invalid in (None, {}, {**review(before), "templateDigest": PRIVATE},
                        {**review(before), "object": {"name": "runtime", "uid": "foreign"}}):
            with self.assertRaises(Failure):
                diagnostics.project(before, before, invalid, "a" * 64, "b" * 64)

    def collect(self, *, mutate_review=None, changed_on_recheck=False, unavailable_hashes=False):
        baselines = {actor: deployment(actor) for actor in diagnostics.ACTORS}
        document = {"apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsCredentialGrant",
                    "metadata": {"name": "workspace", "namespace": "kars-system"},
                    "spec": {"privateActivation": {"phase": "reviewed", "namespaces": [
            {"namespace": {"name": "work"}, "consumers": [review(value) for value in baselines.values()]}
        ]}}}
        if mutate_review:
            mutate_review(document)
        reads = []

        def get(path):
            reads.append(path)
            value = copy.deepcopy(baselines[path.rsplit("/", 1)[-1]])
            if changed_on_recheck and len(reads) % 2 == 0:
                value["metadata"]["resourceVersion"] = "2"
            return value

        with tempfile.TemporaryDirectory(prefix="native-template-review-") as directory:
            root = Path(directory)
            (root / ".native").mkdir()
            (root / ".native/grant-review-kars-system.json").write_text(json.dumps(document))
            side_effect = Failure(PRIVATE) if unavailable_hashes else fixture_hashes
            output = io.StringIO()
            with patch.object(diagnostics, "ROOT", root), \
                 patch.object(diagnostics, "hashes", side_effect=side_effect), \
                 redirect_stdout(output), redirect_stderr(output):
                result = diagnostics.collect(SimpleNamespace(admin=SimpleNamespace(get=get)), baselines, FAILURE)
            self.assertEqual(output.getvalue(), "")
            self.assertNotIn(PRIVATE, json.dumps(result))
            return result, reads

    def test_complete_collector_rechecks_all_three_exact_deployments(self):
        result, reads = self.collect()
        self.assertEqual(result["category"], "compared")
        self.assertTrue(result["available"])
        self.assertTrue(result["diagnosticOnly"])
        self.assertEqual(set(result["actors"]), set(diagnostics.ACTORS))
        self.assertEqual(len(reads), 6)
        self.assertTrue(all(value["currentMatchesReview"] for value in result["actors"].values()))

    def test_changed_resource_or_failed_hashing_never_returns_partial_comparisons(self):
        for options in ({"changed_on_recheck": True}, {"unavailable_hashes": True}):
            result, _ = self.collect(**options)
            self.assertFalse(result["available"])
            self.assertEqual(result["category"], "provenance-or-comparison-unavailable")
            self.assertNotIn("actors", result)

    def test_ambiguous_or_nonreviewed_documents_are_unavailable(self):
        def duplicate(document):
            scope = document["spec"]["privateActivation"]["namespaces"][0]
            scope["consumers"].append(copy.deepcopy(scope["consumers"][0]))

        def malformed(document):
            document["spec"]["privateActivation"]["namespaces"][0]["consumers"] = [None]

        def qualified(document):
            document["spec"]["privateActivation"]["phase"] = "qualified"

        def foreign(document):
            document["metadata"]["namespace"] = "another-workspace"

        for mutation in (duplicate, malformed, qualified, foreign):
            result, _ = self.collect(mutate_review=mutation)
            self.assertFalse(result["available"])
            self.assertNotIn("actors", result)

    def test_ineligible_failures_do_not_read_anything(self):
        for failure in ("Deadline: core-issued current observer capability", "", None):
            with patch.object(diagnostics, "hashes") as hashing:
                result = diagnostics.collect(None, None, failure)
            hashing.assert_not_called()
            self.assertEqual(result["category"], "not-eligible")

    def test_hashing_invokes_the_shipped_helper_and_enforces_closed_output(self):
        value = deployment("runtime")
        with patch.object(diagnostics, "command", return_value=json.dumps(["a" * 64])) as execute:
            self.assertEqual(diagnostics.hashes([value]), ["a" * 64])
        args, kwargs = execute.call_args
        self.assertEqual(args[:3], ("node", "--input-type=module", "-e"))
        self.assertIn(".map(templateDigest)", args[3])
        self.assertEqual(Path(args[4]), diagnostics.ROOT / ".native/core/cli/dist/lib/private-activation.js")
        self.assertEqual(json.loads(kwargs["stdin"]), [value])
        self.assertEqual(kwargs["timeout"], 15)
        for invalid in (json.dumps([PRIVATE]), json.dumps(["a" * 64, "b" * 64]), "null", " " * 513):
            with patch.object(diagnostics, "command", return_value=invalid), self.assertRaises(Failure):
                diagnostics.hashes([value])
        with patch.object(diagnostics, "command") as execute, self.assertRaises(Failure):
            diagnostics.hashes([{"private": "x" * diagnostics.LIMIT}])
        execute.assert_not_called()


if __name__ == "__main__":
    unittest.main()
