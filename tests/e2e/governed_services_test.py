# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute the real shell gate with offline command fixtures, not native auth."""

import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("governed-services.sh")
PRIVATE = "PRIVATE-FIXTURE-DO-NOT-EMIT"
FIXTURE = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import time

root = Path(os.environ["FIXTURE_ROOT"])
mode = os.environ["FIXTURE_MODE"]
private = "PRIVATE-FIXTURE-DO-NOT-EMIT"
name = Path(sys.argv[0]).name
args = sys.argv[1:]
if name != "mktemp":
    print(private, file=sys.stderr)
if name == "mktemp":
    scratch = tempfile.mkdtemp(dir=root)
    (root / "scratch-path").write_text(scratch)
    print(scratch)
elif name == "kubectl":
    assert args[:2] == ["--context", "kind-kars-e2e"]
    args = args[2:]
    if args[0] == "port-forward":
        assert "--address" in args and "127.0.0.1" in args and ":8443" in args
        (root / "forward-pid").write_text(str(os.getpid()))
        if mode == "forward-exit":
            sys.exit(1)
        def stopped(_signal, _frame):
            (root / "forward-stopped").write_text("true")
            sys.exit(0)
        signal.signal(signal.SIGTERM, stopped)
        print("Forwarding from 127.0.0.1:18443 -> 8443", flush=True)
        while True:
            time.sleep(1)
    elif args[1] == "secret":
        if mode == "token-read-failure":
            sys.exit(1)
        if args[2] == "router-services-admin":
            print(private + "-operator")
        else:
            print(private + ("-operator" if mode == "equal-tokens" else "-agent"))
    elif args[1] == "karssandbox":
        print(private + "-sandbox")
    elif args[1] == "namespace":
        print(private + "-namespace")
    elif args[1] == "deployment":
        print(json.dumps({"spec": {"template": {"spec": {
            "volumes": [{"name": "private", "secret": {"secretName": "router-services-admin"}}],
            "containers": [
                {"name": "inference-router", "volumeMounts": [{
                    "name": "private", "mountPath": "/etc/kars/services", "readOnly": True}]},
                {"name": "agent", "volumeMounts": []},
            ],
        }}}}))
    else:
        raise AssertionError("Unexpected fixture operation")
elif name == "curl":
    assert private not in " ".join(args)
    count = root / "request-count"
    index = int(count.read_text()) if count.exists() else 0
    count.write_text(str(index + 1))
    headers = sys.stdin.read()
    bearers = ["", "-agent", "-operator", "", "-agent", "-operator",
               "-agent", "-operator", "-operator", "", ""]
    expected_bearer = bearers[index]
    if expected_bearer:
        assert private + expected_bearer in headers
    else:
        assert "Authorization" not in headers
    expected = [401, 401, 200, 202, 401, 200, 401, 200, 409, 409, 200][index]
    identity = {"sandbox": {"namespace": "kars-system", "name": "e2e-test",
                           "uid": private + "-sandbox"},
                "namespace_uid": private + "-namespace", "task": None}
    scope = private + ("-scope-new" if index >= 7 else "-scope-old")
    if mode == "same-scope" and index == 7:
        scope = private + "-scope-old"
    if mode == "wrong-sandbox" and index == 7:
        identity["sandbox"]["uid"] = private + "-replacement"
    response = {"scope": {"id": scope, "identity": identity}, "enforcement_changed": False,
                "request": {"request_id": private + "-request", "status": "approved"},
                "scope_id": scope, "private": private}
    if mode == "wrong-telemetry" and index == 10:
        response["scope_id"] = private + "-unrelated"
    output = Path(args[args.index("--output") + 1])
    output.write_text(private if mode == "malformed-json" and index == 2 else json.dumps(response))
    if mode == "transport-failure":
        print("000", end="")
        sys.exit(7)
    if mode == "invalid-http-status":
        print(private, end="")
    else:
        print(403 if mode == "wrong-status" else expected, end="")
else:
    raise AssertionError("Unexpected fixture command")
'''


class GovernedServicesDiagnosticsTests(unittest.TestCase):
    def run_gate(self, mode):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            commands = root / "bin"
            commands.mkdir()
            for name in ("kubectl", "curl", "mktemp"):
                executable = commands / name
                executable.write_text(FIXTURE)
                executable.chmod(0o700)
            env = dict(os.environ, FIXTURE_ROOT=directory, FIXTURE_MODE=mode,
                       PATH=str(commands) + os.pathsep + os.environ["PATH"])
            try:
                result = subprocess.run([
                    "bash", "-c",
                    'set -euo pipefail; source "$1"; '
                    'if test_governed_services; then exit 0; else exit 1; fi',
                    "governed-services-test", str(SCRIPT),
                ], env=env, text=True, capture_output=True, timeout=20)
                scratch = Path((root / "scratch-path").read_text())
                self.assertFalse(scratch.exists(), "Credential-bearing scratch files were retained")
                count = root / "request-count"
                requests = int(count.read_text()) if count.exists() else 0
                if (root / "forward-pid").exists() and mode != "forward-exit":
                    self.assertTrue((root / "forward-stopped").exists(), "Owned forward was not stopped")
            finally:
                pid_file = root / "forward-pid"
                if pid_file.exists() and mode != "forward-exit" and not (root / "forward-stopped").exists():
                    try:
                        os.kill(int(pid_file.read_text()), signal.SIGTERM)
                    except ProcessLookupError:
                        pass
        self.assertNotIn(PRIVATE, result.stdout + result.stderr)
        return result, requests

    def failure(self, mode, stage, category, requests=None):
        result, count = self.run_gate(mode)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        lines = result.stderr.splitlines()
        self.assertEqual(len(lines), 1, result.stderr)
        prefix = "GOVERNED-SERVICES-FAILURE "
        self.assertTrue(lines[0].startswith(prefix), result.stderr)
        fact = json.loads(lines[0][len(prefix):])
        self.assertEqual(fact["stage"], stage)
        self.assertEqual(fact["category"], category)
        self.assertEqual(set(fact), {
            "stage", "category", "expectedHttpStatus", "httpStatus", "operatorTokenPresent",
            "agentTokenPresent", "tokensDistinct", "sandboxUidPresent", "namespaceUidPresent",
            "forwardStarted", "scopeChanged", "sandboxPreserved", "telemetryScopeMatches",
        })
        for key, value in fact.items():
            if key not in {"stage", "category", "expectedHttpStatus", "httpStatus"}:
                self.assertIsInstance(value, bool)
        if requests is not None:
            self.assertEqual(count, requests)
        return fact

    def test_unchanged_positive_sequence_has_no_failure_diagnostic(self):
        result, requests = self.run_gate("success")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout + result.stderr, "")
        self.assertEqual(requests, 11)

    def test_forward_exit_is_reported_before_private_log_cleanup(self):
        fact = self.failure("forward-exit", "port-forward-start", "process-exited", 0)
        self.assertFalse(fact["forwardStarted"])

    def test_unchanged_scope_still_fails_and_cleans_up(self):
        fact = self.failure("same-scope", "reset-scope-change", "assertion", 8)
        self.assertFalse(fact["scopeChanged"])
        self.assertTrue(fact["forwardStarted"])

    def test_identity_and_telemetry_comparisons_keep_their_failures(self):
        self.failure("wrong-sandbox", "reset-sandbox-preservation", "assertion", 8)
        self.failure("wrong-telemetry", "telemetry-scope", "assertion", 11)

    def test_equal_credentials_are_reported_only_as_booleans(self):
        fact = self.failure("equal-tokens", "credential-distinctness", "assertion", 0)
        self.assertTrue(fact["operatorTokenPresent"])
        self.assertTrue(fact["agentTokenPresent"])
        self.assertFalse(fact["tokensDistinct"])

    def test_command_failure_retains_only_its_known_stage(self):
        self.failure("token-read-failure", "operator-token-read", "assertion", 0)

    def test_http_mismatch_retains_only_numeric_status(self):
        fact = self.failure("wrong-status", "anonymous-read-denial", "http-status", 1)
        self.assertEqual((fact["expectedHttpStatus"], fact["httpStatus"]), (401, 403))

    def test_transport_or_non_numeric_status_cannot_leak_private_text(self):
        fact = self.failure("transport-failure", "anonymous-read-denial", "http-transport", 1)
        self.assertEqual(fact["httpStatus"], 0)
        fact = self.failure("invalid-http-status", "anonymous-read-denial", "invalid-http-status", 1)
        self.assertEqual(fact["httpStatus"], 0)

    def test_malformed_response_still_fails_without_publishing_the_body(self):
        self.failure("malformed-json", "scope-identity", "assertion", 3)


if __name__ == "__main__":
    unittest.main()
