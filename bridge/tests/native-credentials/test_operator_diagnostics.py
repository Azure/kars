"""Run real subprocess failures and prove no CLI body is published."""

import io
import json
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import native_api
from native_api import Failure
from operator_diagnostics import (
    CHECK_PREFIX, COMMAND_PREFIX, ERRORS, WRITER_CHECK_PREFIX, category, command_facts,
    operator_command, sandbox_checks, source_location, writer_checks,
)

PRIVATE = "DO-NOT-EMIT-TOKENS-OR-PRIVATE-API-BODIES"


class OperatorDiagnosticsTests(unittest.TestCase):
    def test_actual_subprocess_failure_reports_only_stage_category_and_source_location(self):
        error = next(iter(ERRORS))
        stderr = (f"{PRIVATE}\nError: {error}\n"
                  f"    at verifyPrivateBundle (/private/{PRIVATE}/cli/dist/lib/private-activation.js:156:19)\n")
        output = io.StringIO()
        with tempfile.TemporaryDirectory(prefix="native-operator-error-") as directory, \
             patch.object(native_api, "ROOT", Path(directory)), redirect_stdout(output), redirect_stderr(output):
            with self.assertRaises(Failure) as failure:
                operator_command("preview", sys.executable, "-c",
                                 "import sys; print(sys.argv[1],file=sys.stderr); sys.exit(1)",
                                 stderr, timeout=5)
        self.assertEqual(str(failure.exception),
                         "Native operator preview failed: admission-bundle-mismatch "
                         "(source=lib/private-activation:156)")
        self.assertEqual(output.getvalue(), "")
        self.assertNotIn(PRIVATE, str(failure.exception))

    def test_error_bodies_cannot_create_arbitrary_categories_or_paths(self):
        for message, expected in ERRORS.items():
            self.assertEqual(category("Error: " + message), expected)
            self.assertEqual(category("Error: " + message + PRIVATE), "unclassified-cli-error")
        self.assertEqual(category(PRIVATE), "unclassified-cli-error")
        for line in ("/cli/dist/lib/private-activation.js:1:2",
                     f"    at object (/private/{PRIVATE}.js:1:2)",
                     f"    at object (/cli/dist/lib/private-activation.js:1:2){PRIVATE}"):
            self.assertEqual(source_location(line), "unavailable")

    def test_template_drift_uses_a_fixed_category_without_weakening_the_failure(self):
        stderr = ("Error: Private consumer template changed after protection was enabled\n"
                  f"    at reviewedOwner (/private/{PRIVATE}/cli/dist/lib/private-activation.js:680:23)\n")
        with self.assertRaises(Failure) as failure:
            operator_command("apply", sys.executable, "-c",
                             "import sys; print(sys.argv[1],file=sys.stderr); sys.exit(1)", stderr, timeout=5)
        self.assertEqual(str(failure.exception), "Native operator apply failed: consumer-template-drift "
                         "(source=lib/private-activation:680)")
        self.assertNotIn(PRIVATE, str(failure.exception))

    def test_typed_template_errors_keep_only_the_exact_fixed_category(self):
        message = "Private consumer template changed after protection was enabled"
        for prefix in ("PrivateConsumerTemplateChanged", "PrivateConsumerTemplateChanged [Error]"):
            line = f"{prefix}: {message}"
            self.assertEqual(category(line), "consumer-template-drift")
            self.assertEqual(category(line + PRIVATE), "unclassified-cli-error")
            self.assertEqual(category(prefix + ": " + PRIVATE), "unclassified-cli-error")
        self.assertEqual(category(f"{PRIVATE}: {message}"), "unclassified-cli-error")

    def test_late_scope_leaf_is_retained_instead_of_only_its_awaiting_caller(self):
        stderr = (
            f"{PRIVATE}\n"
            "Error: Customized admin credential mount requires explicit recovery\n"
            f"    at supportedTemplate (/private/{PRIVATE}/cli/dist/lib/private-activation-late-scope.js:285:19)\n"
            "    at scopePlan (/cli/dist/lib/private-activation-continuity.js:231:22)\n"
        )
        self.assertEqual(category(stderr), "late-admin-mount")
        self.assertEqual(source_location(stderr), "lib/private-activation-late-scope:285")
        self.assertNotIn(PRIVATE, category(stderr) + source_location(stderr))
        for module in ("private-activation-late-scope-private", "private-activation-late-scope/unknown"):
            self.assertEqual(source_location(f"    at function (/cli/dist/lib/{module}.js:1:2)"), "unavailable")

    def test_actual_failure_retains_only_the_four_boolean_snapshot_checks(self):
        facts = {"resourceVersionMatch": False, "observedGenerationMatch": True,
                 "phaseRunningMatch": True, "readyConditionMatch": True}
        stderr = f"{PRIVATE}\n{CHECK_PREFIX}{json.dumps(facts)}\n{PRIVATE}"
        output = io.StringIO()
        with tempfile.TemporaryDirectory(prefix="native-operator-checks-") as directory, \
             patch.object(native_api, "ROOT", Path(directory)), redirect_stdout(output), redirect_stderr(output):
            with self.assertRaises(Failure) as failure:
                operator_command("preview", sys.executable, "-c",
                                 "import sys; print(sys.argv[1],file=sys.stderr); sys.exit(1)",
                                 stderr, timeout=5)
        self.assertIn("(sandbox-checks=rv=false,generation=true,running=true,ready=true)", str(failure.exception))
        self.assertNotIn(PRIVATE, str(failure.exception))
        self.assertEqual(output.getvalue(), "")

    def test_writer_settling_reports_only_its_exact_module_location(self):
        stderr = (
            f"{PRIVATE}\n"
            f"    at settled (/private/{PRIVATE}/cli/dist/lib/private-activation-writer-settle.js:222:10)\n"
            "    at applyReviewedGrant (/cli/dist/commands/credential-grants.js:170:20)\n"
        )
        self.assertEqual(source_location(stderr), "lib/private-activation-writer-settle:222")
        self.assertNotIn(PRIVATE, source_location(stderr))
        self.assertEqual(source_location(
            "    at function (/cli/dist/lib/private-activation-writer-settle-private.js:1:2)"), "unavailable")

    def test_malformed_ambiguous_or_extended_snapshot_checks_remain_unavailable(self):
        valid = {"resourceVersionMatch": False, "observedGenerationMatch": True,
                 "phaseRunningMatch": True, "readyConditionMatch": True}
        for value in ({}, None, list(valid.items()), {**valid, "private": PRIVATE},
                      {**valid, "resourceVersionMatch": 0}, {**valid, "phaseRunningMatch": PRIVATE}):
            with self.subTest(value=value):
                self.assertEqual(sandbox_checks(CHECK_PREFIX + json.dumps(value)), "unavailable")
        line = CHECK_PREFIX + json.dumps(valid)
        for value in (line + PRIVATE, line + "\n" + line, CHECK_PREFIX + " " * 512,
                      CHECK_PREFIX + '{"resourceVersionMatch":false,"resourceVersionMatch":true,'
                      '"phaseRunningMatch":true,"readyConditionMatch":true}'):
            self.assertEqual(sandbox_checks(value), "unavailable")
        self.assertEqual(sandbox_checks(PRIVATE), "")

    def test_writer_recheck_retains_only_its_three_fixed_booleans(self):
        facts = {"projectionMetadataPresent": True, "projectionMetadataMatches": False,
                 "deploymentTransitionMatches": True}
        marker = WRITER_CHECK_PREFIX + json.dumps(facts)
        stderr = f"{PRIVATE}\n{marker}\n{PRIVATE}"
        self.assertEqual(writer_checks(stderr), "metadata-present=true,metadata-matches=false,deployment=true")
        output = io.StringIO()
        with tempfile.TemporaryDirectory(prefix="native-writer-checks-") as directory, \
             patch.object(native_api, "ROOT", Path(directory)), redirect_stdout(output), redirect_stderr(output):
            with self.assertRaises(Failure) as failure:
                operator_command("apply", sys.executable, "-c",
                                 "import sys; print(sys.argv[1],file=sys.stderr); sys.exit(1)",
                                 stderr, timeout=5)
        self.assertIn("(writer-recheck=metadata-present=true,metadata-matches=false,deployment=true)", str(failure.exception))
        self.assertNotIn(PRIVATE, str(failure.exception))
        self.assertEqual(output.getvalue(), "")
        for value in (marker + PRIVATE, marker + "\n" + marker,
                      WRITER_CHECK_PREFIX + json.dumps({**facts, "private": PRIVATE}),
                      WRITER_CHECK_PREFIX + json.dumps({**facts, "projectionMetadataMatches": 0}),
                      WRITER_CHECK_PREFIX + '{"projectionMetadataPresent":true,'
                      '"projectionMetadataPresent":false,"deploymentTransitionMatches":true}'):
            self.assertEqual(writer_checks(value), "unavailable")
        self.assertEqual(writer_checks(PRIVATE), "")

    def test_actual_sanitized_command_failure_retains_only_closed_facts(self):
        facts = {"version": 1, "phase": "Pausing", "operation": "patch", "resourceKind": "KarsSandbox",
                 "serverReason": "Conflict", "exitCode": 1}
        marker = COMMAND_PREFIX + json.dumps(facts)
        expected = "phase=Pausing,operation=patch,kind=KarsSandbox,reason=Conflict,exit=1"
        self.assertEqual(command_facts(marker), expected)
        self.assertEqual(command_facts("PrivateCommandFailure: " + marker), expected)
        output = io.StringIO()
        with tempfile.TemporaryDirectory(prefix="native-command-facts-") as directory, \
             patch.object(native_api, "ROOT", Path(directory)), redirect_stdout(output), redirect_stderr(output):
            with self.assertRaises(Failure) as failure:
                operator_command("apply", sys.executable, "-c",
                                 "import sys; print(sys.argv[1],file=sys.stderr); sys.exit(1)",
                                 f"{PRIVATE}\nPrivateCommandFailure: {marker}\n{PRIVATE}", timeout=5)
        self.assertIn("(command=" + expected + ")", str(failure.exception))
        self.assertNotIn(PRIVATE, str(failure.exception))
        self.assertEqual(output.getvalue(), "")

    def test_command_fact_parser_rejects_extensions_duplicates_and_unknown_values(self):
        facts = {"version": 1, "phase": "Unscoped", "operation": "other", "resourceKind": "Other",
                 "serverReason": "Unknown", "exitCode": None}
        self.assertIn("reason=Unknown,exit=unknown", command_facts(COMMAND_PREFIX + json.dumps(facts)))
        invalid = [{**facts, key: PRIVATE} for key in ("phase", "operation", "resourceKind", "serverReason")]
        invalid += [{**facts, "version": value} for value in (True, 1.0, 2)]
        invalid += [{**facts, "exitCode": value} for value in (True, -1, 256, 1.5)]
        invalid += [{**facts, "private": PRIVATE}, list(facts.items()), None]
        for value in invalid:
            with self.subTest(value=value):
                self.assertEqual(command_facts(COMMAND_PREFIX + json.dumps(value)), "unavailable")
        marker = COMMAND_PREFIX + json.dumps(facts)
        for value in (marker + PRIVATE, marker + "\n" + marker, COMMAND_PREFIX + " " * 513,
                      COMMAND_PREFIX + '{"version":1,"version":1,"phase":"Unscoped","operation":"other",'
                      '"resourceKind":"Other","serverReason":"Unknown","exitCode":null}'):
            self.assertEqual(command_facts(value), "unavailable")
        self.assertEqual(command_facts(PRIVATE), "")

    def test_success_and_unknown_stage_do_not_change_authority(self):
        with tempfile.TemporaryDirectory(prefix="native-operator-success-") as directory, \
             patch.object(native_api, "ROOT", Path(directory)):
            result = operator_command("preview", sys.executable, "-c", "print('review')", timeout=5)
        self.assertEqual(result, "review\n")
        with self.assertRaises(Failure):
            operator_command("bypass", "never-run", timeout=1)


if __name__ == "__main__":
    unittest.main()
