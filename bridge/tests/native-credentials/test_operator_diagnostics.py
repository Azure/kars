"""Run real subprocess failures and prove no CLI body is published."""

import io
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import native_api
from native_api import Failure
from operator_diagnostics import ERRORS, category, operator_command, source_location

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

    def test_success_and_unknown_stage_do_not_change_authority(self):
        with tempfile.TemporaryDirectory(prefix="native-operator-success-") as directory, \
             patch.object(native_api, "ROOT", Path(directory)):
            result = operator_command("preview", sys.executable, "-c", "print('review')", timeout=5)
        self.assertEqual(result, "review\n")
        with self.assertRaises(Failure):
            operator_command("bypass", "never-run", timeout=1)


if __name__ == "__main__":
    unittest.main()
