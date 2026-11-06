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

    def test_rebound_fixture_routes_rv_drift_through_parent_before_retry(self):
        source = (ROOT / "controller/src/kars_task_rebind/tests.rs").read_text()
        branch = source.split('eprintln!("native-rebind-regression: projection-rv-drift-cleanup");', 1)[1]
        branch = branch.split('eprintln!("native-rebind-regression: projection-retry");', 1)[0]
        self.assertIn("namespace_ownership::finalization_tests::reconcile_rebind_projection_failure(", branch)
        self.assertIn("ctx.client.clone(),", branch)
        self.assertIn("sandbox.clone(),", branch)
        self.assertNotIn("credential_sources::reconcile(", branch)
        self.assertIn('("GET", SANDBOX)', branch)
        self.assertIn('("GET", RUNTIME)', branch)
        self.assertIn("namespace_ownership::FINALIZER", branch)
        self.assertIn("assert!(!s.drift_sandbox_on_deployment_read)", branch)
        self.assertIn('format!("{SANDBOX}/status")', branch)
        self.assertIn('method != "DELETE"', branch)
        self.assertIn('assert_eq!(s.objects[DEPLOYMENT]["spec"]["replicas"], 0)', branch)
        self.assertIn('s.objects[projection]["data"]', branch)
        self.assertIn("data.is_empty()", branch)

    def test_parent_probe_reuses_real_reconcile_metrics_and_error_policy(self):
        source = (ROOT / "controller/src/reconciler/namespace_ownership_finalization_tests.rs").read_text()
        helper = source.split("pub(crate) async fn reconcile_rebind_projection_failure(", 1)[1].split("\nfn memory_account_path", 1)[0]
        self.assertIn("let mut ctx = context(client);", helper)
        self.assertIn("let future = reconcile(sandbox.clone(), ctx.clone());", helper)
        self.assertIn('crate::metrics::observe_reconcile("KarsSandbox", future)', helper)
        self.assertIn("let error = measured.await.unwrap_err();", helper)
        self.assertIn("crate::reconciler::ReconcileError::Credentials(", helper)
        self.assertIn('"Sandbox identity/reference changed"', helper)
        self.assertIn("error_policy(sandbox, &error, ctx)", helper)
        for forbidden in ("Box::pin", "tokio::spawn", "spawn_blocking", "stack_size", "timeout(", "set_var", "catch_unwind"):
            self.assertNotIn(forbidden, helper)
        phases = ("sandbox-parent-poll", "sandbox-parent-returned", "sandbox-parent-error-policy")
        offsets = [helper.index("native-rebind-regression: " + phase) for phase in phases]
        self.assertEqual(offsets, sorted(offsets))
        self.assertLess(offsets[0], helper.index("measured.await"))
        self.assertLess(helper.index("measured.await"), offsets[1])
        self.assertLess(helper.index("error_policy(sandbox"), offsets[2])

    def test_provisioning_constructor_is_outside_parent_error_poll(self):
        source = (ROOT / "controller/src/reconciler/mod.rs").read_text()
        parent = source.split("async fn reconcile_inner(", 1)[1].split("\nfn reconcile_provisioning(", 1)[0]
        factory = source.split("\nfn reconcile_provisioning(", 1)[1].split("\nasync fn reconcile_provisioning_inner(", 1)[0]
        provisioning = source.split("\nasync fn reconcile_provisioning_inner(", 1)[1].split("\n/// How long to wait", 1)[0]
        self.assertIn("let _namespace_lock = namespace_ownership::lock(&name).await;", parent)
        self.assertIn("namespace_ownership::ensure(client, &sandbox).await", parent)
        self.assertIn("crate::sre_authority::pod::authorize(", parent)
        self.assertIn("let credentials = credential_sources::reconcile(", parent)
        self.assertIn(".await?;\n\n    // Keep the namespace lock through provisioning", parent)
        self.assertLess(parent.index("credential_sources::reconcile("), parent.index("\n    reconcile_provisioning("))
        self.assertNotIn("drop(_namespace_lock)", parent)
        self.assertNotIn("skills::project(", parent)
        self.assertNotIn("Box::pin(reconcile_provisioning_inner(", parent)
        self.assertIn("std::pin::Pin<Box<impl std::future::Future<Output = Result<Action, ReconcileError>> + Send>>", factory)
        self.assertIn("Box::pin(reconcile_provisioning_inner(", factory)
        self.assertNotIn(".await", factory)
        for argument in ("sandbox", "ctx", "owned_namespace", "witness_pending", "sre_projection", "credentials"):
            self.assertIn("        " + argument + ",", parent)
            self.assertIn("        " + argument + ",", factory)
        self.assertIn("let skill_catalog = match skills::project(", provisioning)
        self.assertIn("credential_sources::refresh_interval(&sandbox)", provisioning)
        self.assertNotIn("namespace_ownership::lock(", provisioning)

    def test_parent_probe_measures_future_types_without_constructing_continuation(self):
        source = (ROOT / "controller/src/reconciler/namespace_ownership_finalization_tests.rs").read_text()
        helper = source.split("pub(crate) async fn reconcile_rebind_projection_failure(", 1)[1].split("\nfn memory_account_path", 1)[0]
        self.assertIn("fn future_size<F: std::future::Future>(_: impl FnOnce() -> F) -> usize", helper)
        self.assertIn("std::mem::size_of::<F>()", helper)
        self.assertIn("let provisioning_size = future_size(|| {", helper)
        self.assertIn("crate::reconciler::reconcile_provisioning(", helper)
        self.assertIn("provisioning_size <= 1024", helper)
        self.assertLess(helper.index("sandbox-future-bytes"), helper.index("sandbox-parent-poll"))

    def test_parent_probe_is_registered_only_for_tests(self):
        source = (ROOT / "controller/src/reconciler/namespace_ownership.rs").read_text()
        self.assertIn('#[cfg(test)]\n#[path = "namespace_ownership_finalization_tests.rs"]\npub(crate) mod finalization_tests;', source)


if __name__ == "__main__":
    unittest.main()
