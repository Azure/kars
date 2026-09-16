# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import importlib.util
import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import tempfile
import textwrap
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
    components = (
        "addon", "bff", "dependencies", "idp", "lockfiles",
        "rust-dependencies", "secrets", "security", "web",
    )

    def component_success(self):
        return {name: {"result": "success"} for name in self.components}

    def workflow_job(self, name):
        workflow = (CI.parent / ".github/workflows/bridge-ci.yml").read_text()
        match = re.search(rf"(?ms)^  {re.escape(name)}:\n(.*?)(?=^  \S|\Z)", workflow)
        self.assertIsNotNone(match, name)
        return match.group(1)

    def test_idp_is_required_by_the_exact_component_graph(self):
        aggregate = self.workflow_job("bridge-required-gates")
        needs = re.search(r"(?m)^    needs: \[([^\]]+)\]$", aggregate)
        self.assertIsNotNone(needs)
        self.assertEqual({name.strip() for name in needs.group(1).split(",")}, set(self.components))
        self.assertIn("idp", self.components)
        self.assertIn("    if: always()", aggregate)
        self.assertEqual(runpy.run_path(str(CI / "bridge_component_results.py"))["REQUIRED_JOBS"],
                         frozenset(self.components))

    def test_idp_job_requires_real_hosted_images_reports_runtime_and_scans(self):
        job = self.workflow_job("idp")
        for required in (
            "runs-on: ubuntu-24.04", "permissions:\n      contents: read",
            'test "$(uname -m)" = x86_64', "persist-credentials: false",
            "KARS_DEX_PATCH_NETWORK=1", 'check_modules("bridge/idp")',
            "--target upstream-tests", "--target test-tools", "--target runtime",
            "--platform linux/amd64", "--file bridge/idp/Dockerfile bridge/idp",
            "docker create kars-bridge-idp-upstream-tests:latest",
            'trap \'docker rm "$container"\' EXIT',
            "$container:/out/doc/upstream-tests.json", "$container:/out/doc/api-tests.json",
            "aquasecurity/trivy-action@ed142fd0673e97e23eac54620cfb913e5ce36c25",
            "version: v0.70.0", "ignore-unfixed: false", "exit-code: '1'",
            'trivy_path="$(command -v trivy)"', 'test -x "$trivy_path"',
            "python3 bridge/idp/tests/qualify.py", '--trivy "$trivy_path"',
            '--evidence "$IDP_EVIDENCE_DIR/runtime"',
            "if: ${{ !cancelled() && steps.idp_runtime.outcome == 'success' }}",
            "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
            "if-no-files-found: error", "runtime-qualification.log", "step-outcomes.json",
        ):
            self.assertIn(required, job, required)
        self.assertNotIn("continue-on-error", job)
        self.assertNotIn("|| true", job)
        self.assertNotIn(": write", job)
        self.assertNotRegex(job, r"\b(docker push|docker login|az acr|kubectl)\b")
        self.assertNotIn("secrets.", job)
        self.assertGreaterEqual(job.count("if: always()"), 2)
        self.assertIn("set -euo pipefail", job)

    def report_summary(self, root_events, api_events):
        block = self.workflow_job("idp").split("python3 - <<'PY'\n", 1)[1]
        script = textwrap.dedent(block.split("\n          PY", 1)[0])
        with tempfile.TemporaryDirectory(prefix="idp-report-contract-") as temporary:
            evidence = Path(temporary)
            for name, events in (("upstream-tests.json", root_events), ("api-tests.json", api_events)):
                (evidence / name).write_text("".join(json.dumps(event) + "\n" for event in events))
            result = subprocess.run(
                ["python3", "-c", script], text=True, capture_output=True, timeout=10,
                env={**os.environ, "IDP_EVIDENCE_DIR": temporary,
                     "GITHUB_STEP_SUMMARY": str(evidence / "step-summary.md")},
            )
            summary_file = evidence / "upstream-summary.json"
            summary = json.loads(summary_file.read_text()) if summary_file.exists() else None
            return result, summary

    def upstream_report_fixtures(self):
        root = [
            {"Action": "start", "Package": "example/root"},
            {"Action": "pass", "Package": "example/root", "Test": "TestUnit"},
            {"Action": "skip", "Package": "example/root", "Test": "TestUnconfiguredLDAP"},
            {"Action": "pass", "Package": "example/root"},
        ]
        api = [
            {"Action": "start", "Package": "example/api"},
            {"Action": "output", "Package": "example/api", "Output": "[no test files]\n"},
            {"Action": "skip", "Package": "example/api"},
        ]
        return root, api

    def test_upstream_summary_discloses_skips_without_inventing_api_tests(self):
        result, summary = self.report_summary(*self.upstream_report_fixtures())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(summary["root"]["testActions"], {"pass": 1, "skip": 1})
        self.assertEqual(summary["root"]["skipped"],
                         [{"package": "example/root", "test": "TestUnconfiguredLDAP"}])
        self.assertEqual(summary["api"]["testActions"], {})
        self.assertEqual(summary["api"]["packageActions"], {"skip": 1})
        self.assertIn("not runtime-qualified", summary["coverageLimit"])

    def test_upstream_summary_rejects_failure_or_incomplete_proof(self):
        root, api = self.upstream_report_fixtures()
        for root_events, api_events in (
            (root[:-1], api), (root, []),
            (root + [{"Action": "fail", "Package": "example/root", "Test": "TestFailure"}], api),
            (root, api + [{"Action": "build-fail", "ImportPath": "example/api"}]),
            ([{"Action": "start", "Package": "example/root"},
              {"Action": "skip", "Package": "example/root"}], api),
            (root + [None], api),
        ):
            with self.subTest(root=root_events, api=api_events):
                result, summary = self.report_summary(root_events, api_events)
                self.assertNotEqual(result.returncode, 0)
                self.assertIsNone(summary)

    def test_component_aggregate_rejects_missing_failed_or_skipped_jobs(self):
        check = runpy.run_path(str(CI / "bridge_component_results.py"))["require_success"]
        check(self.component_success())
        for result in ({}, [], None, {"bff": {}}, {"bff": None},
                       {"bff": {"result": "success"}, "web": {"result": "failure"}},
                       {"bff": {"result": "skipped"}}, {"bff": {"result": "cancelled"}}):
            with self.subTest(result=result), self.assertRaises(ValueError):
                check(result)
        for name in self.components:
            missing = self.component_success()
            del missing[name]
            with self.subTest(missing=name), self.assertRaises(ValueError):
                check(missing)
            for outcome in (None, {}, {"result": "failure"}, {"result": "cancelled"},
                            {"result": "skipped"}, {"result": "neutral"}):
                result = self.component_success()
                result[name] = outcome
                with self.subTest(job=name, outcome=outcome), self.assertRaises(ValueError):
                    check(result)
        unexpected = {**self.component_success(), "unexpected": {"result": "success"}}
        with self.assertRaises(ValueError):
            check(unexpected)

    def test_component_workflow_entrypoint_requires_complete_results(self):
        partial = self.component_success()
        del partial["security"]
        without_idp = self.component_success()
        del without_idp["idp"]
        environment = {key: value for key, value in os.environ.items()
                       if key != "COMPONENT_RESULTS"}
        for payload, expected in ((json.dumps(self.component_success()), 0),
                                  (json.dumps(partial), 1), (json.dumps(without_idp), 1), ("not-json", 1),
                                  ("null", 1), (None, 1)):
            with self.subTest(payload=payload):
                result = subprocess.run(
                    ["python3", str(CI / "bridge_component_results.py")],
                    text=True, capture_output=True, timeout=10,
                    env={**environment, **({} if payload is None else
                                          {"COMPONENT_RESULTS": payload})},
                )
                self.assertEqual(result.returncode, expected, result.stderr)

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
