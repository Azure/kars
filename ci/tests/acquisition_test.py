# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline subprocess fixtures; these do not claim a live Kind/API result."""

import contextlib
from concurrent.futures import ThreadPoolExecutor
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import time
import unittest
from unittest.mock import Mock, patch
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "tests/e2e"))
from ci import acquire_test_tools as acquisition
from sre_authority.common import Harness, command_error_category
from sre_authority import proxy

PRIVATE = "fixture-PAT-must-not-leak https://private.invalid/?token=fixture-secret"
BINARY = "#!/bin/sh\nprintf 'UNVERIFIED-EXECUTION' >&2\nexit 99\n"
SHA = hashlib.sha256(BINARY.encode()).hexdigest()
CHECKSUM = SHA + "  kind-linux-amd64\n"
MANIFEST = "apiVersion: v1\nkind: List\nitems: []\n"

FAKE_CURL = r'''
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
import json
import os
from pathlib import Path
import sys
import time

plan = Path(os.environ["ACQUISITION_FIXTURE"])
responses = json.loads(plan.read_text())
log = plan.with_suffix(".calls")
calls = json.loads(log.read_text()) if log.exists() else []
args = sys.argv[1:]
output = Path(args[args.index("--output") + 1])
path_file = Path(os.environ["GITHUB_PATH"])
calls.append({"args": args, "mode": output.stat().st_mode & 0o777,
              "directoryMode": output.parent.stat().st_mode & 0o777,
              "previousSize": output.stat().st_size, "path": path_file.read_text()})
response = responses[min(len(calls) - 1, len(responses) - 1)]
time.sleep(response.get("startup_sleep", 0))
log.write_text(json.dumps(calls))
output.write_text(response.get("body", ""))
time.sleep(response.get("sleep", 0))
sys.stdout.write(response.get("status", "200"))
sys.stderr.write(response.get("stderr", ""))
sys.exit(response.get("exit", 0))
'''


class AcquisitionTests(unittest.TestCase):
    def setUp(self):
        self.work = ROOT / (".ci-acquisition-test-" + uuid.uuid4().hex)
        self.work.mkdir(mode=0o700)
        self.addCleanup(shutil.rmtree, self.work)
        self.tools = self.work / "tools"
        self.tools.mkdir(mode=0o700)
        curl = self.tools / "curl"
        curl.write_text("#!" + sys.executable + "\n" + FAKE_CURL)
        curl.chmod(0o700)
        self.plan = self.work / "responses.json"
        self.github_path = self.work / "github-path"
        self.github_path.write_text("")
        self.environment = patch.dict(os.environ, {
            "PATH": str(self.tools) + os.pathsep + os.environ.get("PATH", ""),
            "ACQUISITION_FIXTURE": str(self.plan), "GITHUB_PATH": str(self.github_path),
            "GITHUB_ACTIONS": "true", "PYTHONDONTWRITEBYTECODE": "1",
        })
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.destination = self.work / "asset"

    def responses(self, *values):
        self.plan.write_text(json.dumps(values))
        self.plan.with_suffix(".calls").unlink(missing_ok=True)

    def calls(self):
        log = self.plan.with_suffix(".calls")
        return json.loads(log.read_text()) if log.exists() else []

    def download(self, seconds=15, max_bytes=1024):
        acquisition.download("https://public.invalid/asset", self.destination,
                             time.monotonic() + seconds, max_bytes)

    def assert_no_download(self):
        self.assertFalse(self.destination.exists())
        self.assertFalse(self.destination.with_suffix(".part").exists())

    def install(self):
        with patch.object(acquisition.platform, "system", return_value="Linux"), \
             patch.object(acquisition.platform, "machine", return_value="x86_64"):
            return acquisition.install_kind(self.work, self.github_path)

    def harness(self, seconds=650):
        h = Harness.__new__(Harness)
        h.root, h.work, h.phase = ROOT, self.work, "acquisition-test"
        h.deadline = time.monotonic() + seconds
        h.get = Mock(return_value=None)
        h.k = Mock(return_value=subprocess.CompletedProcess([], 0, "", ""))
        h.poll = Mock()
        return h

    def assert_metrics_clean(self):
        self.assertEqual(list(self.work.glob("metrics-*")), [])

    def test_checksum_requires_one_exact_official_filename_and_hex_digest(self):
        for data in (
            "<html>" + PRIVATE + "</html>", "", SHA, SHA + "  kind-linux-arm64\n",
            SHA + "  ./kind-linux-amd64\n", CHECKSUM + CHECKSUM,
            CHECKSUM + PRIVATE, "x" * 64 + "  kind-linux-amd64\n",
            SHA + "\tkind-linux-amd64\n", SHA + "  kind-linux-amd64.extra\n",
            PRIVATE + "\n" + CHECKSUM, SHA + "  kind-linux-amd64\n\n",
        ):
            with self.subTest(checksum_case=data[:8]):
                self.responses({"body": data})
                category = "checksum-format" if data else "invalid-size"
                with self.assertRaisesRegex(acquisition.AcquisitionError, "^" + category + "$"):
                    self.install()
                self.assertEqual(len(self.calls()), 1)
                self.assertEqual(self.github_path.read_text(), "")
                self.assertEqual(list(self.work.glob(".ci-kind-*")), [])
        self.assertEqual(acquisition.checksum_for(CHECKSUM.encode(), "kind-linux-amd64"), SHA)
        self.assertEqual(acquisition.checksum_for(
            (SHA.upper() + " *kind-linux-amd64").encode(), "kind-linux-amd64"), SHA)

    def test_binary_mismatch_never_chmods_publishes_executes_or_retries(self):
        self.responses({"body": CHECKSUM}, {"body": "<html>" + PRIVATE + "</html>"})
        with patch.object(Path, "chmod") as chmod, \
             self.assertRaisesRegex(acquisition.AcquisitionError, "^checksum-mismatch$"):
            self.install()
        chmod.assert_not_called()
        self.assertEqual(len(self.calls()), 2)
        self.assertEqual(self.github_path.read_text(), "")
        self.assertEqual(list(self.work.glob(".ci-kind-*")), [])

    def test_verified_kind_has_unique_private_install_without_cache_or_execution(self):
        other = self.work / ".ci-kind-not-owned"
        other.mkdir()
        (other / "kind").write_text("do not use or remove")
        self.responses({"body": CHECKSUM}, {"body": BINARY},
                       {"body": CHECKSUM}, {"body": BINARY})
        first = self.install()
        second = self.install()
        self.assertNotEqual(first, second)
        self.assertEqual(first.read_text(), BINARY)
        self.assertEqual(stat.S_IMODE(first.stat().st_mode), 0o700)
        self.assertEqual(list(first.parent.iterdir()), [first])
        self.assertEqual(self.github_path.read_text().splitlines(),
                         [str(first.parent), str(second.parent)])
        self.assertEqual((other / "kind").read_text(), "do not use or remove")
        self.assertEqual(len(self.calls()), 4)
        for call in self.calls()[:2]:
            self.assertEqual((call["mode"], call["directoryMode"], call["path"]), (0o600, 0o700, ""))
        self.assertEqual([call["args"][-1] for call in self.calls()[:2]], [
            acquisition.KIND_RELEASE + "/kind-linux-amd64.sha256sum",
            acquisition.KIND_RELEASE + "/kind-linux-amd64",
        ])

    def test_publication_failure_cleans_verified_binary_not_other_files(self):
        self.responses({"body": CHECKSUM}, {"body": BINARY})
        self.github_path = self.work / "missing-parent" / "path"
        # The fixture's path stays valid; only the installer's publication fails.
        with self.assertRaises(OSError):
            self.install()
        self.assertEqual(list(self.work.glob(".ci-kind-*")), [])

    def test_kind_checksum_binary_and_verification_share_one_total_deadline(self):
        clock = [0]
        deadlines = []

        def fetch(url, destination, deadline, _max_bytes):
            deadlines.append(deadline)
            checksum = url.endswith(".sha256sum")
            destination.write_text(CHECKSUM if checksum else BINARY)
            clock[0] = 80 if checksum else 121

        with patch.object(acquisition, "monotonic", side_effect=lambda: clock[0]), \
             patch.object(acquisition, "download", side_effect=fetch), \
             patch.object(Path, "chmod") as chmod, \
             self.assertRaisesRegex(acquisition.AcquisitionError, "^deadline$"):
            self.install()
        self.assertEqual(deadlines, [120, 120])
        chmod.assert_not_called()
        self.assertEqual(self.github_path.read_text(), "")
        self.assertEqual(list(self.work.glob(".ci-kind-*")), [])

    def test_wrong_platform_fails_before_download_or_path_publication(self):
        for system, machine in (("Darwin", "arm64"), ("Linux", "riscv64")):
            with self.subTest(system=system, machine=machine), \
                 patch.object(acquisition.platform, "system", return_value=system), \
                 patch.object(acquisition.platform, "machine", return_value=machine), \
                 self.assertRaisesRegex(acquisition.AcquisitionError, "^unsupported-platform$"):
                acquisition.install_kind(self.work, self.github_path)
        self.assertEqual(self.calls(), [])
        self.assertEqual(self.github_path.read_text(), "")

    def test_arm64_uses_matching_official_checksum_and_binary(self):
        self.responses({"body": SHA + "  kind-linux-arm64\n"}, {"body": BINARY})
        with patch.object(acquisition.platform, "system", return_value="Linux"), \
             patch.object(acquisition.platform, "machine", return_value="aarch64"):
            binary = acquisition.install_kind(self.work, self.github_path)
        self.assertEqual(binary.read_text(), BINARY)
        self.assertTrue(all(call["args"][-1].split("/")[-1].startswith("kind-linux-arm64")
                            for call in self.calls()))

    def test_permanent_http_failures_never_retry_or_publish_partial_body(self):
        for status in ("401", "403", "404", "408", "410", "422"):
            with self.subTest(status=status):
                self.responses({"status": status, "exit": 22, "body": PRIVATE, "stderr": PRIVATE})
                with patch.object(acquisition, "sleep") as sleep, \
                     self.assertRaisesRegex(acquisition.AcquisitionError, "^http-" + status + "$"):
                    self.download()
                self.assertEqual(len(self.calls()), 1)
                sleep.assert_not_called()
                self.assert_no_download()

    def test_transient_http_and_transport_failures_retry_from_empty_file(self):
        failures = [{"status": str(status), "exit": 22} for status in (429, 500, 502, 503, 504, 599)]
        failures += [{"status": "000", "exit": code} for code in acquisition.TRANSIENT_CURL]
        failures += [{"status": "200", "exit": 18}]
        for failure in failures:
            with self.subTest(failure=failure):
                self.responses({**failure, "body": PRIVATE, "stderr": PRIVATE}, {"body": "complete"})
                with patch.object(acquisition, "sleep") as sleep:
                    self.download()
                self.assertEqual(self.destination.read_text(), "complete")
                self.destination.unlink()
                self.assertEqual(len(self.calls()), 2)
                self.assertEqual(self.calls()[1]["previousSize"], 0)
                sleep.assert_called_once_with(1)

    def test_retry_exhaustion_is_three_attempts_with_only_one_two_second_backoffs(self):
        for response, category in (({"status": "503", "exit": 22}, "http-503"),
                                   ({"status": "000", "exit": 28}, "timeout"),
                                   ({"status": "000", "exit": 7}, "transport")):
            with self.subTest(category=category):
                self.responses({**response, "body": PRIVATE})
                with patch.object(acquisition, "sleep") as sleep, \
                     self.assertRaisesRegex(acquisition.AcquisitionError, "^" + category + "$"):
                    self.download()
                self.assertEqual(len(self.calls()), 3)
                self.assertEqual([call.args[0] for call in sleep.call_args_list], [1, 2])
                self.assert_no_download()

    def test_permanent_curl_failures_and_malformed_status_never_retry(self):
        for response, category in (
            ({"status": "000", "exit": 60}, "transport"),
            ({"status": "000", "exit": 23}, "transport"),
            ({"status": "000", "exit": 1}, "transport"),
            ({"status": "302", "exit": 1}, "http-302"),
            ({"status": "200 " + PRIVATE}, "invalid-status"),
            ({"status": "000"}, "invalid-status"),
        ):
            with self.subTest(category=category):
                self.responses({**response, "body": PRIVATE, "stderr": PRIVATE})
                with self.assertRaisesRegex(acquisition.AcquisitionError, "^" + category + "$"):
                    self.download()
                self.assertEqual(len(self.calls()), 1)
                self.assert_no_download()

    def test_curl_arguments_enforce_https_redirects_http_failures_and_both_time_bounds(self):
        self.responses({"body": "asset"})
        self.download(seconds=12)
        args = self.calls()[0]["args"]
        self.assertEqual(args[0], "--disable")
        for flag, value in (("--proto", "=https"), ("--proto-redir", "=https"),
                            ("--max-redirs", "5"), ("--retry", "0"), ("--max-filesize", "1024")):
            self.assertEqual(args[args.index(flag) + 1], value)
        for flag in ("--fail", "--location", "--silent", "--show-error"):
            self.assertIn(flag, args)
        self.assertLessEqual(float(args[args.index("--connect-timeout") + 1]), 10)
        self.assertLessEqual(float(args[args.index("--max-time") + 1]), 12)
        self.assertNotIn("--insecure", args)
        self.assertEqual(stat.S_IMODE(self.destination.stat().st_mode), 0o600)

    def test_real_subprocess_deadline_kills_partial_transfer_without_retry(self):
        self.responses({"body": PRIVATE, "startup_sleep": 0.3, "sleep": 30})
        partial = self.destination.with_suffix(".part")
        processes = []
        popen = subprocess.Popen

        def record_process(*args, **kwargs):
            process = popen(*args, **kwargs)
            processes.append(process)
            return process

        start = time.monotonic()
        with patch.object(acquisition.subprocess, "Popen", side_effect=record_process), \
             patch.object(acquisition, "sleep") as backoff, \
             ThreadPoolExecutor(max_workers=1) as executor:
            # Observe actual partial bytes within the startup allowance, while
            # the unchanged downloader enforces its real five-second deadline.
            result = executor.submit(self.download, seconds=5)
            while not (partial.exists() and partial.read_bytes() == PRIVATE.encode()):
                self.assertFalse(result.done(), "Download ended before writing partial data")
                self.assertLess(time.monotonic() - start, 3, "Fixture startup timed out")
                time.sleep(0.01)
            self.assertLess(time.monotonic() - start, 3, "Fixture startup timed out")
            self.assertEqual(len(processes), 1)
            self.assertIsNone(processes[0].poll(), "Fixture must still be transferring")
            self.assertEqual(len(self.calls()), 1)
            with self.assertRaisesRegex(acquisition.AcquisitionError, "^deadline$"):
                result.result(timeout=7 - (time.monotonic() - start))
        self.assertGreaterEqual(time.monotonic() - start, 5)
        self.assertLess(time.monotonic() - start, 7)
        self.assertIsNotNone(processes[0].returncode)
        self.assertNotEqual(processes[0].returncode, 0)
        backoff.assert_not_called()
        self.assertEqual(len(self.calls()), 1)
        self.assert_no_download()

    def test_deadline_prevents_initial_attempt_and_excess_backoff(self):
        self.responses({"body": PRIVATE, "status": "503", "exit": 22})
        with self.assertRaisesRegex(acquisition.AcquisitionError, "^deadline$"):
            self.download(seconds=-1)
        self.assertEqual(self.calls(), [])
        self.assert_no_download()
        with patch.object(acquisition, "sleep") as sleep, \
             self.assertRaisesRegex(acquisition.AcquisitionError, "^deadline$"):
            self.download(seconds=0.5)
        sleep.assert_not_called()
        self.assertEqual(len(self.calls()), 1)
        self.assert_no_download()

    def test_empty_or_oversize_body_is_not_a_success_or_retry(self):
        for body in ("", "too large"):
            with self.subTest(body=body):
                self.responses({"body": body})
                with self.assertRaisesRegex(acquisition.AcquisitionError, "^invalid-size$"):
                    self.download(max_bytes=2)
                self.assertEqual(len(self.calls()), 1)
                self.assert_no_download()

    def test_existing_files_are_not_adopted_overwritten_or_cleaned(self):
        for target in (self.destination, self.destination.with_suffix(".part")):
            with self.subTest(target=target.name):
                target.write_text("owned by another invocation")
                with self.assertRaises((acquisition.AcquisitionError, FileExistsError)):
                    self.download()
                self.assertEqual(target.read_text(), "owned by another invocation")
                self.assertEqual(self.calls(), [])
                target.unlink()

    def test_child_cli_diagnostics_do_not_leak_private_body_stderr_or_urls(self):
        self.responses({"status": "403", "exit": 22, "body": PRIVATE, "stderr": PRIVATE})
        result = subprocess.run(
            [sys.executable, str(ROOT / "ci/acquire_test_tools.py"), "metrics",
             "--destination", str(self.destination), "--budget", "3"],
            capture_output=True, text=True, timeout=5,
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertEqual(result.stderr, "CI-ACQUISITION-FAILURE http-403\n")
        self.assert_no_download()

    def test_kind_cli_requires_ci_environment_before_acquisition(self):
        with patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}), \
             patch.object(sys, "argv", ["acquire_test_tools.py", "kind"]), \
             contextlib.redirect_stderr(io.StringIO()) as stderr:
            self.assertEqual(acquisition.main(), 1)
        self.assertEqual(stderr.getvalue(), "CI-ACQUISITION-FAILURE ci-environment\n")
        self.assertEqual(self.calls(), [])

    def test_metrics_uses_local_private_manifest_and_retains_patch_rollout_and_poll(self):
        self.responses({"body": MANIFEST})
        h = self.harness()

        def kubectl(*args, **kwargs):
            if args[0] == "apply":
                manifest = Path(args[2])
                self.assertTrue(manifest.is_relative_to(self.work))
                self.assertEqual(manifest.read_text(), MANIFEST)
                self.assertEqual(stat.S_IMODE(manifest.stat().st_mode), 0o600)
                self.assertEqual(stat.S_IMODE(manifest.parent.stat().st_mode), 0o700)
                self.assertLess(kwargs["timeout"], 90)
                self.assertEqual(kwargs["expected"], None)
            return subprocess.CompletedProcess([], 0, "", "")

        h.k.side_effect = kubectl
        proxy.install_metrics(h)
        self.assertEqual([call.args[0] for call in h.k.call_args_list], ["apply", "patch", "rollout"])
        self.assertIn("--kubelet-insecure-tls", h.k.call_args_list[1].args[-1])
        self.assertEqual(h.k.call_args_list[2].kwargs["timeout"], 130)
        h.poll.assert_called_once()
        self.assertEqual(self.calls()[0]["args"][-1], acquisition.METRICS_URL)
        self.assert_metrics_clean()

    def test_metrics_download_failure_never_calls_kubernetes_or_leaks_child_data(self):
        self.responses({"status": "404", "exit": 22, "body": PRIVATE, "stderr": PRIVATE})
        h = self.harness()
        with self.assertRaisesRegex(
            AssertionError, "^Metrics manifest download failed; category=ci-acquisition:http-404$"
        ):
            proxy.install_metrics(h)
        h.k.assert_not_called()
        h.poll.assert_not_called()
        self.assertEqual(len(self.calls()), 1)
        self.assert_metrics_clean()

    def test_metrics_apply_failure_is_separate_sanitized_and_never_retried(self):
        self.responses({"body": MANIFEST})
        h = self.harness()
        h.k.return_value = subprocess.CompletedProcess(
            [], 1, PRIVATE, "Error from server (Forbidden): " + PRIVATE)
        with self.assertRaisesRegex(
            AssertionError, "^Metrics manifest Kubernetes apply failed; category=Forbidden$"
        ):
            proxy.install_metrics(h)
        h.k.assert_called_once()
        self.assertEqual(h.k.call_args.args[0], "apply")
        h.poll.assert_not_called()
        self.assertEqual(len(self.calls()), 1)
        self.assert_metrics_clean()

    def test_metrics_apply_timeout_never_retries_mutations(self):
        self.responses({"body": MANIFEST})
        h = self.harness()
        h.k.side_effect = AssertionError("Command exceeded its bounded timeout at apply_metrics")
        with self.assertRaisesRegex(AssertionError, "bounded timeout at apply_metrics"):
            proxy.install_metrics(h)
        h.k.assert_called_once()
        self.assert_metrics_clean()

    def test_metrics_parent_timeout_cleans_partial_download_and_never_applies(self):
        self.responses({"body": PRIVATE, "sleep": 3})
        h = self.harness(seconds=0.3)
        start = time.monotonic()
        with self.assertRaisesRegex(AssertionError, "bounded timeout|ci-acquisition:deadline"):
            proxy.install_metrics(h)
        self.assertLess(time.monotonic() - start, 2)
        h.k.assert_not_called()
        self.assert_metrics_clean()

    def test_metrics_fetch_and_apply_share_original_ninety_seconds_and_phase_deadline(self):
        for phase_budget, spent in ((650, 7), (30, 7), (30, 30), (650, 90)):
            with self.subTest(phase_budget=phase_budget, spent=spent):
                h = self.harness()
                h.deadline = phase_budget
                clock = [0]

                def fetch(args, **kwargs):
                    self.assertEqual(kwargs["timeout"], min(90, phase_budget))
                    self.assertEqual(float(args[-1]), min(90, phase_budget))
                    Path(args[args.index("--destination") + 1]).write_text(MANIFEST)
                    clock[0] = spent
                    return subprocess.CompletedProcess([], 0, "", "")

                h.run = Mock(side_effect=fetch)
                with patch.object(proxy.time, "monotonic", side_effect=lambda: clock[0]):
                    if spent >= min(90, phase_budget):
                        with self.assertRaisesRegex(AssertionError, "Kubernetes apply exceeded"):
                            proxy.install_metrics(h)
                        h.k.assert_not_called()
                    else:
                        proxy.install_metrics(h)
                        self.assertEqual(h.k.call_args_list[0].kwargs["timeout"],
                                         min(90, phase_budget) - spent)
                self.assert_metrics_clean()

    def test_existing_metrics_service_preserves_no_install_behavior(self):
        h = self.harness()
        h.get.return_value = {"metadata": {"name": "v1beta1.metrics.k8s.io"}}
        h.run = Mock()
        proxy.install_metrics(h)
        h.run.assert_not_called()
        h.k.assert_not_called()
        h.poll.assert_called_once()

    def test_acquisition_classification_accepts_only_fixed_categories(self):
        prefix = "CI-ACQUISITION-FAILURE "
        self.assertEqual(command_error_category(PRIVATE + "\n" + prefix + "http-403\n" + PRIVATE),
                         "ci-acquisition:http-403")
        for text in (prefix + PRIVATE, prefix + "http-403 " + PRIVATE, prefix + "http-999"):
            self.assertEqual(command_error_category(text), "unclassified")
        self.assertEqual(command_error_category(prefix + "deadline\n" + prefix + "http-503"),
                         "ci-acquisition:ambiguous")

    def test_ci_wires_tests_before_install_without_changing_other_tool_or_cluster_versions(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        e2e = workflow.split("\n  e2e-kind:", 1)[1].split("\n  bench-regression:", 1)[0]
        tests = "python3 -m unittest discover -s ci/tests -p acquisition_test.py"
        installer = "python3 ci/acquire_test_tools.py kind"
        self.assertLess(e2e.index(tests), e2e.index(installer))
        self.assertNotIn("helm/kind-action@", e2e)
        self.assertIn("version: v1.30.5", e2e)
        self.assertIn("azure/setup-helm@9bc31f4ebc9c6b171d7bfbaa5d006ae7abdb4310", e2e)
        self.assertIn("make test-e2e", e2e)
        self.assertEqual(workflow.count("helm/kind-action@ef37e7f390d99f746eb8b610417061a60e82a6cc"), 2)
        self.assertEqual(acquisition.KIND_VERSION, "v0.24.0")
        self.assertEqual(acquisition.METRICS_URL,
            "https://github.com/kubernetes-sigs/metrics-server/releases/download/v0.7.2/components.yaml")
        self.assertEqual((ROOT / "tests/e2e/kind-config.yaml").read_bytes(),
                         b"# Copyright (c) Microsoft Corporation.\n"
                         b"# Licensed under the MIT License.\n\n"
                         b"kind: Cluster\napiVersion: kind.x-k8s.io/v1alpha4\nnodes:\n"
                         b"  - role: control-plane\n  - role: worker\n"
                         b"    labels:\n      kars.azure.com/pool: sandbox\n")
        self.assertIn('kind create cluster --name "$CLUSTER_NAME" --config "$SCRIPT_DIR/kind-config.yaml"',
                      (ROOT / "tests/e2e/run.sh").read_text())
        self.assertIn("      - name: Test bounded CI dependency acquisition\n"
                      "        run: " + tests, e2e)


if __name__ == "__main__":
    unittest.main()
