# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

from pathlib import Path
import re
import unittest

from controller_failure_diagnostics import GRANT_STAGES

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

    def test_late_material_regression_uses_existing_native_worker_and_runtime_fence(self):
        workflow = (ROOT / ".github/workflows/bridge-native.yml").read_text()
        step = workflow.split("      - name: Exercise Team projection", 1)[1].split("      - name:", 1)[0]
        test = "runtime_control_material_receipts_preserve_rotation_and_resume_fences"
        self.assertIn("private_activation::late_scope::tests::" + test, step)
        self.assertEqual(step.count("-- --exact --nocapture"), 2)
        source = (ROOT / "controller/src/private_activation/late_scope.rs").read_text()
        self.assertIn('#[tokio::test(flavor = "multi_thread", worker_threads = 1)]\n    async fn ' + test, source)
        regression = source.split("async fn " + test, 1)[1].split("    #[tokio::test]", 1)[0]
        self.assertIn("tokio::spawn(async", regression)
        self.assertIn("api.apply().await", regression)
        for phase in ("apply-poll", "apply-returned", "complete"):
            self.assertIn("native-late-material-regression: " + phase, regression)
        for fault in ("tls-bytes", "memory-bytes", "extra-data", "tls-key", "memory-key", "anchor", "epoch", "binding", "version", "missing-tls", "unknown-proof"):
            self.assertIn('"' + fault + '"', regression)
        for forbidden in ("Box::pin", "stack_size", "timeout(", "catch_unwind"):
            self.assertNotIn(forbidden, regression)

    def test_controller_receipt_keeps_exact_proofs_and_shared_control_version(self):
        source = (ROOT / "controller/src/private_activation/late_scope.rs").read_text()
        self.assertIn('fields(value, "object key", "tls memory")', source)
        self.assertIn('fields(tls, "configuration key", "")', source)
        self.assertIn('"configuration key anchor epoch binding version"', source)
        self.assertIn('rotated_authority(&state["baseline"], &value["material"])', source)
        rotated = source.split("fn rotated(", 1)[1].split("pub(super) async fn fence", 1)[0]
        self.assertIn("crate::sre_registration::control_credential_version(secret)", rotated)
        self.assertIn('proof["configuration"] == hash(&json!(STANDARD.encode(&bytes.0)))', rotated)
        self.assertIn("data.len() != expected_keys", rotated)
        self.assertIn('memory["version"] != version', rotated)
        self.assertIn('material["object"]["resourceVersion"] == baseline["object"]["resourceVersion"]', rotated)
        self.assertNotIn("data.len() != 1", rotated)

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


class GrantStageSourceTests(unittest.TestCase):
    @staticmethod
    def compact(value):
        return re.sub(r"\s+", "", value).replace(",)", ")")

    @staticmethod
    def source(path):
        return (ROOT / "controller/src" / path).read_text()

    @staticmethod
    def marker(stage, phase):
        field = f'stage = "{stage}"' if stage else "stage"
        return (f'tracing::info!(target: "kars_controller::credential_grants", {field}, '
                f'phase = "{phase}", "Credential grant lifecycle stage");')

    def blocks(self, source, stage):
        return re.findall(re.escape(self.compact(self.marker(stage, "before"))) + r"(.*?)"
                          + re.escape(self.compact(self.marker(stage, "returned"))), self.compact(source))

    def assert_awaits(self, source, stage, statements):
        self.assertEqual(self.blocks(source, stage), [self.compact(value) for value in statements])

    def test_parent_events_admit_real_call_and_first_poll_before_validation(self):
        source = self.source("credential_grants.rs")
        parent = source.split("pub(crate) async fn reconcile(", 1)[1].split("pub async fn run(", 1)[0]
        self.assertTrue(parent.split("{", 1)[1].lstrip().startswith(self.marker("parent", "entered")))
        self.assert_awaits(source, "parent", ["let result = reconcile(&client, &grant).await;"])
        self.assertIn(self.compact(self.marker("parent", "returned") + "if let Err(error) = result {"), self.compact(source))
        self.assert_awaits(parent, "validation", ["""
            let validation = async {
                verify(client, grant).await?;
                admission::verify(client).await?;
                let sources = sources::inventory(client, grant).await?;
                let legacy = legacy::inventory(client, grant).await?;
                Ok::<_, String>((sources, legacy))
            }.await;
        """])
        self.assert_awaits(parent, "writer-authority", ["let (active, writer_error) = writers::authority(client, grant, &sources).await;"])
        self.assert_awaits(parent, "observer-issuance", ["let result = operator::reconcile(client, &active).await;"])
        self.assert_awaits(parent, "observer-revocation", [
            f"let {name} = operator::revoke(client, grant).await;" for name in ("result", "result", "revoked", "operators")])
        self.assert_awaits(parent, "control", ["let controls = control::reconcile(client, grant).await;"])
        self.assert_awaits(parent, "writer-release", ["let result = writers::release(client, grant).await;"] * 2)
        publications = self.blocks(parent, "publication")
        self.assertEqual(len(publications), 2)
        for block in publications:
            self.assertTrue(block.startswith("letresult=publish(client,grant,"))
            self.assertTrue(block.endswith(").await;"))
            self.assertEqual(block.count(".await"), 1)
        self.assertIn('"Ready"', publications[0])
        self.assertIn('ifgrant.spec.enabled{"Blocked"}else{"Revoked"}', publications[1])

    def test_parent_retains_authority_branches_finalizer_and_error_propagation(self):
        source = self.compact(self.source("credential_grants.rs"))
        self.assertIn(self.compact('''if writer_error.is_some() && !grant.spec.observation_targets.is_empty() {
            Err("Observation recipient authority is unavailable".into())
        } else if writer_error.is_some() {'''), source)
        self.assertIn(self.compact('if revoked.is_ok() && operators.is_ok() {'
                                  + self.marker("writer-release", "before")), source)
        self.assertEqual(source.count(self.compact(self.marker("writer-release", "returned") + "result?;")), 2)
        self.assertIn(self.compact(self.marker("publication", "returned") + "result?; Err(reason)"), source)
        deletion = source.split("ifgrant.metadata.deletion_timestamp.is_some(){", 1)[1].split("letmutowned=grant.clone();", 1)[0]
        calls = [deletion.index(call) for call in (
            "github::revoke(client,grant).await?", "operator::revoke(client,grant).await",
            "rbac::revoke(client,grant).await?", "writers::release(client,grant).await", "api.patch(")]
        self.assertEqual(calls, sorted(calls))
        self.assertIn(".filter(|entry|entry!=FINALIZER)", deletion)
        self.assertIn('"uid":grant.metadata.uid,"resourceVersion":grant.metadata.resource_version', deletion)
        self.assertIn(self.compact('tokio::time::sleep(std::time::Duration::from_secs(15)).await;'), source)

    def test_writer_events_bracket_actual_authority_and_ordered_retirement(self):
        source = self.source("credential_grants/writers.rs").split("pub(super) async fn authority(", 1)[1]
        for stage, call in (
            ("writer-reconcile", "reconcile(client, grant)"),
            ("writer-rbac-apply", "super::rbac::apply(client, &active, sources)"),
            ("writer-rbac-revoke", "super::rbac::revoke(client, grant)"),
            ("writer-observer-revoke", "super::operator::revoke(client, grant)"),
            ("writer-release", "release(client, grant)"),
        ):
            self.assert_awaits(source, stage, [f"let result = {call}.await;"])
        compact = self.compact(source)
        self.assertIn(self.compact(self.marker("writer-reconcile", "returned") + "let active = result?;"), compact)
        self.assertIn(self.compact(self.marker("writer-rbac-apply", "returned") + "result?;"), compact)
        # Both revocations still complete before the existing array is inspected.
        self.assertIn("forrevokedin[{", compact)
        self.assertLess(compact.index('stage="writer-rbac-revoke"'), compact.index('stage="writer-observer-revoke"'))
        self.assertLess(compact.index('stage="writer-observer-revoke"'), compact.index("ifletErr(revoke)=revoked"))
        self.assertLess(compact.index("ifletErr(revoke)=revoked"), compact.index('stage="writer-release"'))
        self.assertIn(self.compact(self.marker("writer-release", "returned") + "if let Err(held) = result {"), compact)

    def test_observer_events_bracket_real_purpose_specific_ensure_and_retire(self):
        source = self.source("credential_grants/operator.rs")
        for stage, purpose, configuration in (
            ("observer-tls-ensure", "OBSERVER_TLS", "tls_configuration"),
            ("observer-ensure", "OBSERVER", "configuration"),
        ):
            self.assert_awaits(source, stage, [f"""
                let result = governed_services::credentials::ensure_for(
                    client, &sandbox, &namespace, governed_services::credentials::{purpose}, Some(&{configuration}),
                ).await;
            """])
        compact = self.compact(source)
        self.assertIn(self.compact(self.marker("observer-tls-ensure", "returned") + "result?;"), compact)
        self.assertIn(self.compact(self.marker("observer-ensure", "returned") + "let credential = result?;"), compact)
        self.assertIn(self.compact('''for (purpose, stage) in [
            (governed_services::credentials::OBSERVER, "observer-retire"),
            (governed_services::credentials::OBSERVER_TLS, "observer-tls-retire"),
        ]'''), compact)
        self.assert_awaits(source, None, ["let result = governed_services::credentials::retire_for(client, sandbox, namespace, purpose).await;"])
        self.assertIn(self.compact(self.marker(None, "returned") + "result?;"), compact)

    def test_every_stage_event_has_only_closed_fields_and_no_wrapping_runtime(self):
        found = set()
        for path in ("credential_grants.rs", "credential_grants/writers.rs", "credential_grants/operator.rs"):
            source = self.source(path)
            events = re.findall(r'tracing::info!\((.*?)\);', source, re.S)
            self.assertTrue(events)
            for event in events:
                match = re.fullmatch(
                    r'\s*target:\s*"kars_controller::credential_grants",\s*stage(?:\s*=\s*"([a-z-]+)")?,\s*'
                    r'phase\s*=\s*"(before|returned|entered)",\s*"Credential grant lifecycle stage"\s*', event)
                self.assertIsNotNone(match, event)
                stage, phase = match.groups()
                if stage is None:
                    self.assertEqual(path, "credential_grants/operator.rs")
                    found.update({"observer-retire", "observer-tls-retire"})
                else:
                    self.assertIn(stage, GRANT_STAGES)
                    found.add(stage)
                if phase == "entered":
                    self.assertEqual(stage, "parent")
            for forbidden in ("Box::pin", "stack_size", "spawn_blocking", "catch_unwind", "RUST_MIN_STACK"):
                self.assertNotIn(forbidden, source)
        self.assertEqual(found, GRANT_STAGES)

    def test_projector_keeps_existing_tail_and_identity_fences(self):
        source = (ROOT / "bridge/tests/native-credentials/controller_failure_diagnostics.py").read_text()
        self.assertIn('len(raw.encode()) <= 131072', source)
        self.assertIn('raw.splitlines()[-512:]', source)
        self.assertIn('del result["grantStages"][:-32]', source)
        self.assertIn('"--tail=512", "--limit-bytes=131072", "--request-timeout=10s"', source)
        self.assertIn('recheck_actor(setup, actor)\n        return {"available": True', source)
        self.assertIn('"concurrent-work-precludes-crash-causation"', source)


if __name__ == "__main__":
    unittest.main()
