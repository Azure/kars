# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = "google_discovery_test.go"
OVERLAY = "google-discovery-overlay.json"
TARGET = "connector/google/kars_discovery_test.go"


class GoogleDiscoveryContracts(unittest.TestCase):
    def test_overlay_adds_only_one_test_and_does_not_change_source_inventory(self):
        overlay = json.loads((ROOT / "patches" / OVERLAY).read_text())
        self.assertEqual(overlay, {"Replace": {TARGET: "/packaging/patches/" + FIXTURE}})
        manifest = dict(line.split("  ", 1)[::-1]
                        for line in (ROOT / "patches/SHA256SUMS").read_text().splitlines())
        for name in (FIXTURE, OVERLAY):
            self.assertEqual(manifest[name], hashlib.sha256((ROOT / "patches" / name).read_bytes()).hexdigest())
        for name in ("patched.sha256", "upstream.sha256", "series"):
            self.assertNotIn("google", (ROOT / "patches" / name).read_text())
        self.assertNotIn("google", (ROOT / "scripts/apply-source-patches.sh").read_text())
        self.assertNotIn("-overlay", (ROOT / "scripts/build.sh").read_text())
        dockerfile = (ROOT / "Dockerfile").read_text()
        self.assertIn("COPY patches/ /packaging/patches/", dockerfile)
        runtime = dockerfile.split(" AS runtime\n", 1)[1]
        self.assertNotIn("--from=upstream-tests", runtime)

    def test_fixture_is_package_local_fail_closed_and_keeps_issuer_validation(self):
        fixture = (ROOT / "patches" / FIXTURE).read_text()
        for text in ("package google", "func TestMain(m *testing.M)",
                     "httptest.NewServer", "local.Proxy = nil", "r.Clone(r.Context())",
                     'r.Method == http.MethodGet && r.URL.String() == issuerURL+"/.well-known/openid-configuration"',
                     'r.URL.Scheme == "http" && net.ParseIP(r.URL.Hostname()).IsLoopback()',
                     'return nil, fmt.Errorf("Google qualification forbids external request:',
                     '"issuer":', "document.Issuer != issuerURL", "defer discovery.Close()",
                     "http.DefaultTransport = original", "TestKarsGoogleDiscoveryIsolation",
                     "TestKarsGoogleLoopbackFixtures", '"http://169.254.169.254/computeMetadata/v1/"'):
            self.assertIn(text, fixture)
        for text in ("InsecureSkipVerify", "InsecureIssuerURLContext", "SkipIssuerCheck", "t.Skip",
                     "t.Parallel", "os.Setenv", "func TestOpen", "func TestGetGroups"):
            self.assertNotIn(text, fixture)
        self.assertLess(fixture.index("http.DefaultTransport = karsGoogleTestTransport"),
                        fixture.index("return m.Run()"))


class QualificationScriptContracts(unittest.TestCase):
    def setUp(self):
        self.work = ROOT / "tests" / (".google-contract-" + uuid.uuid4().hex)
        self.work.mkdir()
        self.addCleanup(shutil.rmtree, self.work)
        self.source = self.work / "source"
        (self.source / "connector/google").mkdir(parents=True)
        (self.source / "api/v2").mkdir(parents=True)
        self.packaging = self.work / "packaging"
        shutil.copytree(ROOT / "patches", self.packaging / "patches")
        self.out = self.work / "out/doc"
        self.out.mkdir(parents=True)
        baseline = self.source / "connector/google/google.go"
        baseline.write_text("package google\n")
        self.original = baseline.read_bytes()
        (self.packaging / "source.sha256").write_text(
            hashlib.sha256(self.original).hexdigest() + "  ./connector/google/google.go\n")
        self.script = self.work / "upstream-tests.sh"
        self.script.write_text((ROOT / "scripts/upstream-tests.sh").read_text()
                               .replace("/packaging", str(self.packaging))
                               .replace("/out", str(self.work / "out")))
        (self.work / "bin").mkdir()
        shim = self.work / "bin/go"
        # Test the qualification orchestration, not Go behavior or compilation.
        shim.write_text('''#!/bin/sh
printf '%s|%s\\n' "$PWD" "$*" >> "$GO_CALLS"
case "$*" in
  *-list*) echo TestNewAuthenticateMessage_ChallengeTargetInfoOffsetOverflowNoPanics ;;
  *-overlay=*)
    if test "${FAIL_ROOT:-0}" = 1; then
      echo '{"Action":"fail","Package":"github.com/dexidp/dex/connector/google"}'
      exit 19
    fi ;;
esac
''')
        shim.chmod(0o755)
        self.calls = self.work / "calls"
        self.env = {**os.environ, "PATH": str(self.work / "bin") + os.pathsep + os.environ["PATH"],
                    "GO_CALLS": str(self.calls)}

    def run_script(self):
        return subprocess.run(["sh", str(self.script)], cwd=self.source, env=self.env,
                              capture_output=True, text=True)

    def test_full_root_and_api_races_remain_and_source_is_untouched(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.calls.read_text().splitlines()
        self.assertEqual(len(calls), 4)
        self.assertIn("test -json -count=1 -race github.com/Azure/go-ntlmssp/...", calls[1])
        self.assertEqual(calls[2], str(self.source) + "|test -overlay=" +
                         str(self.packaging / "patches" / OVERLAY) + " -json -count=1 -race ./...")
        self.assertEqual(calls[3], str(self.source / "api/v2") + "|test -json -count=1 -race ./...")
        self.assertEqual((self.source / "connector/google/google.go").read_bytes(), self.original)
        self.assertFalse((self.source / TARGET).exists())

    def test_failed_root_suite_retains_failure_and_never_runs_api(self):
        self.env["FAIL_ROOT"] = "1"
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(self.calls.read_text().splitlines()), 3)
        evidence = (self.out / "upstream-tests.json").read_text()
        self.assertIn('"Action":"fail"', evidence)
        self.assertIn(evidence, result.stdout)
        self.assertFalse((self.out / "api-tests.json").exists())

    def test_tampered_fixture_or_overlay_fails_before_any_go_command(self):
        for name in (FIXTURE, OVERLAY):
            with self.subTest(name=name):
                path = self.packaging / "patches" / name
                original = path.read_bytes()
                path.write_bytes(original + b"\n")
                result = self.run_script()
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.calls.exists())
                path.write_bytes(original)

    def test_existing_overlay_target_is_not_hidden(self):
        (self.source / TARGET).write_text("package google\n")
        self.assertNotEqual(self.run_script().returncode, 0)
        self.assertFalse(self.calls.exists())

    def test_source_drift_fails_before_any_go_command(self):
        (self.source / "connector/google/google.go").write_bytes(self.original + b"\n")
        self.assertNotEqual(self.run_script().returncode, 0)
        self.assertFalse(self.calls.exists())


if __name__ == "__main__":
    unittest.main()
