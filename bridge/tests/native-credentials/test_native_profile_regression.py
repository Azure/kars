# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[3]
TEST = "kars_task_reconciler::rebind::tests::credential_rebind_full_reconcile_preserves_authority_before_resume"


class NativeProfileRegressionTests(unittest.TestCase):
    def test_regression_uses_native_profile_without_hiding_runtime_failure(self):
        workflow = (ROOT / ".github/workflows/bridge-native.yml").read_text()
        runtime = workflow.split("  native-runtime:\n", 1)[1].split("  native-required-gates:\n", 1)[0]
        step = runtime.split("      - name: Exercise Team projection", 1)[1].split("      - name:", 1)[0]
        self.assertIn("CARGO_PROFILE_DEV_DEBUG: '0'", runtime)
        self.assertIn("cargo test --manifest-path .native/core/Cargo.toml --locked", step)
        self.assertIn("-p kars-controller --bin kars-controller", step)
        self.assertIn(TEST, step)
        self.assertIn("-- --exact --nocapture", step)
        self.assertIn("steps.native_runtime.outcome == 'success'", step)
        self.assertIn("steps.native_runtime.outcome == 'failure'", step)
        self.assertIn("!cancelled()", step)
        for forbidden in ("--release", "continue-on-error", "RUST_MIN_STACK", "RUSTFLAGS", "|| true", "timeout-minutes"):
            self.assertNotIn(forbidden, step)
        self.assertNotIn("RUST_MIN_STACK", runtime)
        self.assertLess(runtime.index("run: python3 tests/native-credentials/run.py"), runtime.index("Exercise Team projection"))
        self.assertLess(runtime.index("Exercise Team projection"), runtime.index("Retain only bounded secret-free native outcomes"))

    def test_exact_test_registration_and_default_worker_projection_phases_are_maintained(self):
        controller = ROOT / "controller/src"
        self.assertIn("mod kars_task_reconciler;", (controller / "main.rs").read_text())
        self.assertIn('pub(crate) mod rebind;', (controller / "kars_task_reconciler.rs").read_text())
        self.assertIn('mod tests;', (controller / "kars_task_rebind.rs").read_text())
        source = (controller / "kars_task_rebind/tests.rs").read_text()
        self.assertIn('#[tokio::test(flavor = "multi_thread", worker_threads = 1)]\nasync fn ' + TEST.split("::")[-1], source)
        self.assertIn("tokio::spawn(full_task_credential_rebind()).await.unwrap();", source)
        for phase in ("resumed-projection", "projection-rv-drift-cleanup", "projection-retry", "projection-complete"):
            self.assertIn("native-rebind-regression: " + phase, source)


if __name__ == "__main__":
    unittest.main()
