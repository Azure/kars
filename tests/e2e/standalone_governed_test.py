# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Runner routing checks only; these do not provide native execution evidence."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "tests/e2e/standalone-governed.sh"
CHECKS = (
    "setup_cluster", "build_images", "prepare_managed_mcp", "prepare_standalone_namespace",
    "install_crds",
    "test_crd_installed", "test_controller_running", "test_controller_metrics_endpoint",
    "test_admission_policies_installed", "test_operator_default_deny_np",
    "test_create_sandbox", "test_sandbox_deployment_exists", "test_sandbox_pod_starts",
    "test_governed_services", "test_managed_mcp", "test_credential_sources",
    "test_cleanup_sandbox",
)


class StandaloneGovernedTests(unittest.TestCase):
    def invoke(self, *, failure="", clusters="", changed_uid=False, existing_config=False, diagnostic_failure=False):
        with tempfile.TemporaryDirectory(prefix=".standalone-unit-", dir=ROOT) as directory:
            config = Path(directory) / "owned-kubeconfig"
            if existing_config:
                config.write_text("existing fixture")
            definitions = "\n".join(
                f'{check}() {{ printf "STEP {check}\\n"; '
                f'[ "$FIXTURE_FAILURE" != "{check}" ]; }}' for check in CHECKS
            )
            script = r'''
source "$FIXTURE_RUNNER"
E2E_KUBECONFIG="$FIXTURE_CONFIG"
kind() {
    if [ "$1 $2" = "get clusters" ]; then
        printf '%s\n' "$FIXTURE_CLUSTERS"
    elif [ "$1 $2" = "delete cluster" ]; then
        printf 'DELETE OWNED CLUSTER\n'
    else
        return 90
    fi
}
kubectl() { printf '%s' "$FIXTURE_UID"; }
node() { printf 'STEP budget\n'; [ "$FIXTURE_FAILURE" != "budget" ]; }
standalone_diagnostics() { printf 'SNAPSHOT %s\n' "$1"; [ "$FIXTURE_DIAGNOSTIC_FAILURE" != 1 ]; }
prepare_sre_authority_legacy() { printf 'FORBIDDEN SRE PREPARE\n'; return 91; }
test_sre_authority_migration() { printf 'FORBIDDEN SRE MIGRATION\n'; return 92; }
sre_authority_cleanup() { printf 'FORBIDDEN SRE CLEANUP\n'; return 93; }
'''
            script += definitions + "\n"
            if changed_uid:
                script += 'test_cleanup_sandbox() { FIXTURE_UID="replaced"; return 0; }\n'
            script += "standalone_governed_main\n"
            env = dict(os.environ, FIXTURE_RUNNER=str(RUNNER), FIXTURE_CONFIG=str(config),
                       FIXTURE_FAILURE=failure, FIXTURE_CLUSTERS=clusters, FIXTURE_UID="original")
            env["FIXTURE_DIAGNOSTIC_FAILURE"] = "1" if diagnostic_failure else "0"
            result = subprocess.run(["bash", "-c", script], env=env, text=True,
                                    capture_output=True, timeout=20, check=False)
            return result, config.exists()

    def test_standalone_runs_real_entrypoints_without_active_sre_routing(self):
        result, _ = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        for check in CHECKS:
            self.assertIn("STEP " + check, result.stdout)
        self.assertIn("STEP budget", result.stdout)
        self.assertIn("DELETE OWNED CLUSTER", result.stdout)
        self.assertLess(result.stdout.index("SNAPSHOT final"), result.stdout.index("DELETE OWNED CLUSTER"))
        self.assertNotIn("FORBIDDEN", result.stdout)

    def test_each_feature_failure_remains_a_failed_run_and_still_cleans_owned_cluster(self):
        for check in ("test_governed_services", "test_managed_mcp",
                      "test_credential_sources", "budget", "test_cleanup_sandbox"):
            with self.subTest(check=check):
                result, _ = self.invoke(failure=check)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("DELETE OWNED CLUSTER", result.stdout)
                self.assertLess(result.stdout.index("SNAPSHOT "), result.stdout.index("DELETE OWNED CLUSTER"))

    def test_diagnostic_failure_is_fatal_without_masking_the_original_failure(self):
        for failed in ("", "budget"):
            with self.subTest(failed=failed):
                result, _ = self.invoke(failure=failed, diagnostic_failure=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("diagnostics incomplete", result.stderr)
                self.assertIn("DELETE OWNED CLUSTER", result.stdout)
                if failed:
                    self.assertIn("Standalone check failed: standalone_budget", result.stdout)

    def test_deployment_wait_observes_current_generation_before_pod_assertions(self):
        with tempfile.TemporaryDirectory(prefix=".standalone-unit-", dir=ROOT) as directory:
            counter = Path(directory) / "calls"
            counter.write_text("0")
            script = r'''
source "$FIXTURE_MAIN"
kubectl() {
    count=$(cat "$FIXTURE_COUNTER"); count=$((count + 1)); printf '%s' "$count" > "$FIXTURE_COUNTER"
    case "$count" in
      1) return 1 ;;
      2) printf 'deployment-uid|2|1' ;;
      *) printf 'deployment-uid|2|2' ;;
    esac
}
sleep() { printf 'BOUNDED POLL\n'; }
test_sandbox_deployment_exists
'''
            result = subprocess.run(["bash", "-c", script], text=True, capture_output=True, timeout=10,
                env=dict(os.environ, FIXTURE_MAIN=str(ROOT / "tests/e2e/run.sh"), FIXTURE_COUNTER=str(counter)))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(counter.read_text(), "3")
            self.assertIn("created and observed", result.stdout)
            self.assertEqual(result.stdout.count("BOUNDED POLL"), 2)

    def test_exit_handler_preserves_a_nonstandard_original_failure_code(self):
        script = r'''
source "$FIXTURE_RUNNER"
standalone_diagnostics() { return 1; }
standalone_cleanup() { printf 'OWNED CLEANUP\n'; return 0; }
standalone_exit 23
'''
        result = subprocess.run(["bash", "-c", script], text=True, capture_output=True, timeout=10,
            env=dict(os.environ, FIXTURE_RUNNER=str(RUNNER)))
        self.assertEqual(result.returncode, 23)
        self.assertIn("OWNED CLEANUP", result.stdout)

    def test_standalone_ci_does_not_dump_raw_pods_or_logs_after_teardown(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        diagnostics = workflow.split("      - name: Collect cluster diagnostics on failure\n", 1)[1]
        self.assertIn("&& !(github.event_name == 'workflow_dispatch' && inputs.e2e_suite == 'standalone-governed')",
                      diagnostics.split("run:", 1)[0])
        self.assertIn("standalone_diagnostics_test mcp_probe_test", workflow)

    def test_setup_failure_does_not_adopt_or_teardown_unestablished_cluster(self):
        result, _ = self.invoke(failure="setup_cluster")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("DELETE OWNED CLUSTER", result.stdout)
        self.assertNotIn("STEP build_images", result.stdout)

    def test_existing_cluster_and_config_are_not_adopted_or_removed(self):
        for arguments in ({"clusters": "other\nkars-e2e"}, {"existing_config": True}):
            with self.subTest(arguments=arguments):
                result, remains = self.invoke(**arguments)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("STEP setup_cluster", result.stdout)
                self.assertNotIn("DELETE OWNED CLUSTER", result.stdout)
                if arguments.get("existing_config"):
                    self.assertTrue(remains)

    def test_replaced_cluster_is_never_deleted_and_cleanup_failure_is_visible(self):
        result, _ = self.invoke(changed_uid=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("DELETE OWNED CLUSTER", result.stdout)
        self.assertIn("identity changed", result.stderr)

    def test_default_runner_still_requires_legacy_migration_before_feature_tests(self):
        runner = (ROOT / "tests/e2e/run.sh").read_text()
        main = runner.split("main() {", 1)[1]
        self.assertLess(main.index("prepare_sre_authority_legacy"), main.index("install_crds"))
        self.assertLess(main.index("test_sre_authority_migration"), main.index("test_managed_mcp"))
        self.assertIn('if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then\n    main "$@"', main)
        installation = runner.split("install_crds() {", 1)[1].split("\nteardown()", 1)[0]
        self.assertIn('case "${1:-full}" in', installation)
        self.assertIn('if [ "$SRE_LEGACY_PREPARED" != "0" ]; then', installation)
        self.assertIn("extra_set_args+=(--set sre.enabled=false)", installation)
        self.assertIn("extra_set_args+=(--set sre.enabled=true --set sre.authorityStage=true", installation)
        self.assertIn("install_crds standalone-governed", RUNNER.read_text())

    def test_manual_subset_cannot_report_the_required_full_e2e_check(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        job = workflow.split("  e2e-kind:\n", 1)[1].split("  bench-regression:\n", 1)[0]
        self.assertIn("github.event_name == 'workflow_dispatch' && inputs.e2e_suite == 'standalone-governed'", job)
        self.assertIn("'Standalone governed services (Kind)' || 'E2E (Kind)'", job)
        self.assertIn("KARS_E2E_SUITE: ${{ github.event_name == 'workflow_dispatch' && inputs.e2e_suite || 'full' }}", job)
        self.assertIn("full) make test-e2e", job)
        self.assertIn('case "$KARS_E2E_SUITE" in', job)
        self.assertIn('*) echo "Unsupported E2E qualification suite" >&2; exit 2', job)
        self.assertNotIn("continue-on-error", job)

    def test_install_mode_preserves_full_authority_and_rejects_mixed_setup(self):
        script = r'''
source "$FIXTURE_MAIN"
helm() {
    if [ "$1" = version ]; then printf 'v4.1.3'; else printf 'HELM_ARG %s\n' "$@"; fi
}
sre_migration_helm_wait_arg() { printf '%s' --wait=legacy; }
SRE_LEGACY_PREPARED="$FIXTURE_PREPARED"
install_crds "$FIXTURE_MODE"
'''
        for mode, prepared, expected in (
            ("full", "1", "sre.enabled=true"),
            ("standalone-governed", "0", "sre.enabled=false"),
            ("standalone-governed", "1", None),
            ("unknown", "0", None),
        ):
            with self.subTest(mode=mode, prepared=prepared):
                env = dict(os.environ, FIXTURE_MAIN=str(ROOT / "tests/e2e/run.sh"),
                           FIXTURE_MODE=mode, FIXTURE_PREPARED=prepared)
                result = subprocess.run(["bash", "-c", script], env=env, text=True,
                                        capture_output=True, timeout=20, check=False)
                if expected is None:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertNotIn("HELM_ARG", result.stdout)
                else:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn("HELM_ARG " + expected, result.stdout)

    def test_fresh_namespace_uses_chart_security_labels_and_exclusive_creation(self):
        namespace = {
            "apiVersion": "v1", "kind": "Namespace",
            "metadata": {"name": "kars-system", "labels": {
                "pod-security.kubernetes.io/enforce": "restricted",
            }, "annotations": {"retained": "annotation"}},
        }
        script = r'''
source "$FIXTURE_RUNNER"
helm() { printf '%s' "$FIXTURE_NAMESPACE"; }
kubectl() {
    printf 'KUBE_CALL %s\n' "$*" >&2
    cat
}
prepare_standalone_namespace
'''
        env = dict(os.environ, FIXTURE_RUNNER=str(RUNNER),
                   FIXTURE_NAMESPACE=json.dumps(namespace))
        result = subprocess.run(["bash", "-c", script], env=env, text=True,
                                capture_output=True, timeout=20, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        metadata = json.loads(result.stdout)["metadata"]
        self.assertEqual(metadata["labels"]["pod-security.kubernetes.io/enforce"], "restricted")
        self.assertEqual(metadata["labels"]["app.kubernetes.io/managed-by"], "Helm")
        self.assertEqual(metadata["annotations"], {
            "retained": "annotation",
            "meta.helm.sh/release-name": "kars",
            "meta.helm.sh/release-namespace": "kars-system",
        })
        calls = [line for line in result.stderr.splitlines() if line.startswith("KUBE_CALL ")]
        self.assertEqual(len(calls), 2)
        self.assertEqual(sum("create --dry-run=client --validate=strict" in call for call in calls), 1)
        self.assertEqual(sum(call.endswith("create -f -") for call in calls), 1)
        self.assertTrue(all("--context kind-kars-e2e" in call for call in calls))
        self.assertFalse(any("apply" in call or "patch" in call for call in calls))

    def test_namespace_preparation_failure_cleans_only_the_owned_cluster(self):
        result, _ = self.invoke(failure="prepare_standalone_namespace")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("STEP install_crds", result.stdout)
        self.assertIn("DELETE OWNED CLUSTER", result.stdout)


if __name__ == "__main__":
    unittest.main()
