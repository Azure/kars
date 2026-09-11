# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import importlib.util
import os
from pathlib import Path
import runpy
import subprocess
import unittest

from git_fixture import GitFixture

CI = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("bridge_contracts", CI / "bridge_contracts.py")
scope = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scope)


class ContractScopeTests(GitFixture):
    def invoke(self, event="pull_request", base=None, head=None, extra=()):
        return subprocess.run(
            ["python3", str(CI / "bridge_contracts.py"), "--event", event,
             "--base", self.base if base is None else base,
             "--head", self.git("rev-parse", "HEAD").strip() if head is None else head, *extra],
            cwd=self.root, text=True, capture_output=True, timeout=30,
        )

    def test_unknown_and_every_runtime_surface_require_native_contracts(self):
        for path in (
            "controller/src/lib.rs", "inference-router/src/lib.rs", "bridge/bff/src/main.rs",
            "bridge/web/src/lib/types.ts", "cli/src/commands/credential-grants.ts",
            "cli/package-lock.json", "mesh-plugin/src/index.ts", "shared/protocol.json",
            "runtimes/openclaw/skills/SKILL.md", "sandbox-images/openclaw/entrypoint.sh",
            "deploy/agentmesh-agt.yaml", "tests/e2e/run.sh", "new-package/src/lib.rs",
            "Cargo.toml", "Cargo.lock", "Makefile", ".github/workflows/bridge-native.yml",
            "ci/bridge_contracts.py", "docs/runtime-policy.json", "bridge/docs/compatibility.md",
        ):
            with self.subTest(path=path):
                self.assertTrue(scope.native_required([path]))
        self.assertFalse(scope.native_required(["README.md", "docs/guide.md", "docs/image.svg"]))
        self.assertTrue(scope.native_required(["docs/guide.md", "cli/src/index.ts"]))
        self.assertFalse(scope.native_required(["bridge/web/src/page.tsx"], core_only=True))
        self.assertTrue(scope.native_required(["bridge/web/src/page.tsx", "cli/src/index.ts"], core_only=True))

    def test_actual_diff_keeps_moves_and_deletions_in_scope(self):
        self.write("controller/src/fixture.rs", "fn fixture() {}\n")
        self.commit()
        self.base = self.git("rev-parse", "HEAD").strip()
        (self.root / "docs").mkdir()
        self.git("mv", "controller/src/fixture.rs", "docs/fixture.md")
        self.commit()
        result = self.invoke()
        self.assertEqual((result.returncode, result.stdout), (0, "required=true\n"))

    def test_empty_and_documentation_only_diffs_do_not_claim_native_execution(self):
        self.assertEqual(self.invoke().stdout, "required=false\n")
        self.write("docs/guide with\nnewline.md", "documentation\n")
        self.commit()
        result = self.invoke()
        self.assertEqual((result.returncode, result.stdout), (0, "required=false\n"))

    def test_invalid_revision_diff_or_event_fails_instead_of_skipping(self):
        for arguments in ({"base": ""}, {"head": "HEAD; false"},
                          {"base": "0" * 40}, {"event": "pull_request_target"}, {"event": ""}):
            with self.subTest(arguments=arguments):
                result = self.invoke(**arguments)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
        for event in ("push", "workflow_dispatch", "workflow_call", "release", "schedule"):
            self.assertEqual(self.invoke(event=event).stdout, "required=true\n")

    def test_core_and_pairing_outputs_have_distinct_bridge_only_scope(self):
        self.write("bridge/web/src/page.tsx", "export const page = 1;\n")
        self.commit()
        self.assertEqual(self.invoke(extra=("--output", "code")).stdout, "code=true\n")
        self.assertEqual(self.invoke(extra=("--output", "run", "--core-only")).stdout, "run=false\n")
        self.assertEqual(self.invoke(event="push", extra=("--output", "run", "--core-only")).stdout,
                         "run=true\n")

    def test_sparse_core_checkout_really_removes_only_bridge(self):
        for path in ("Cargo.toml", "cli/src/index.ts", "controller/src/lib.rs",
                     ".github/workflows/bridge-native.yml", "bridge/bff/Cargo.toml"):
            self.write(path, "fixture\n")
        self.commit()
        self.git("sparse-checkout", "set", "--no-cone", "/*", "!/bridge/")
        self.assertFalse((self.root / "bridge").exists())
        for path in ("Cargo.toml", "cli/src/index.ts", "controller/src/lib.rs",
                     ".github/workflows/bridge-native.yml"):
            self.assertTrue((self.root / path).is_file(), path)


class ContractAggregateTests(unittest.TestCase):
    def test_component_aggregate_rejects_missing_failed_or_skipped_jobs(self):
        check = runpy.run_path(str(CI / "bridge_component_results.py"))["require_success"]
        check({"bff": {"result": "success"}, "web": {"result": "success"}})
        for result in ({}, [], None, {"bff": {}}, {"bff": None},
                       {"bff": {"result": "success"}, "web": {"result": "failure"}},
                       {"bff": {"result": "skipped"}}, {"bff": {"result": "cancelled"}}):
            with self.subTest(result=result), self.assertRaises(ValueError):
                check(result)

    def test_only_required_success_or_explicit_docs_skip_passes(self):
        for required, scope_result, api, runtime, expected in (
            ("true", "success", "success", "success", 0),
            ("false", "success", "skipped", "skipped", 0),
            ("true", "success", "success", "skipped", 1),
            ("true", "success", "failure", "success", 1),
            ("true", "success", "cancelled", "success", 1),
            ("false", "failure", "skipped", "skipped", 1),
            ("false", "success", "failure", "skipped", 1),
            ("false", "success", "success", "success", 1),
            ("unknown", "success", "skipped", "skipped", 1),
            ("", "success", "skipped", "skipped", 1),
        ):
            with self.subTest(required=required, scope=scope_result, api=api, runtime=runtime):
                result = subprocess.run(
                    ["bash", str(CI / "bridge-contract-result.sh")],
                    text=True, capture_output=True, timeout=10,
                    env={**os.environ, "NATIVE_REQUIRED": required, "SCOPE_RESULT": scope_result,
                         "API_RESULT": api, "RUNTIME_RESULT": runtime},
                )
                self.assertEqual(result.returncode, expected, result.stderr)


if __name__ == "__main__":
    unittest.main()
