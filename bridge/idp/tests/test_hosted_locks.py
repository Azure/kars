# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import importlib.util
import json
import os
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest import mock

from contracts import LOCK_FILES, check_modules
from test_baselines import artifact_log

ROOT = Path(__file__).resolve().parents[1]


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


hosted = load("hosted_locks_test", "hosted-locks.py")
importer = load("hosted_import_test", "import-locks.py")
IDENTITY = {"GITHUB_REPOSITORY": "owner/repo", "SOURCE_SHA": "a" * 40,
            "GITHUB_SHA": "b" * 40, "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1",
            "GITHUB_EVENT_NAME": "pull_request"}


class HostedLockContracts(unittest.TestCase):
    def test_prepare_changes_only_isolated_exact_crypto_request(self):
        before = {name: (ROOT / "locks/generated" / name).read_bytes() for name in LOCK_FILES}
        original = (ROOT / "locks/requests.txt").read_bytes()
        with tempfile.TemporaryDirectory(prefix=".hosted-contract-", dir=ROOT) as directory:
            context, evidence = Path(directory) / "context", Path(directory) / "evidence"
            with mock.patch.dict(os.environ, IDENTITY):
                hosted.prepare(context, evidence)
            self.assertEqual((context / "locks/requests.txt").read_bytes(), original.replace(
                b"golang.org/x/crypto@v0.58.0", hosted.FIXED_CRYPTO.encode()))
            self.assertIn("v0.58.1-0.20261009181718-2f1f834330b2", hosted.FIXED_CRYPTO)
            provenance = json.loads((evidence / "provenance.json").read_text())
            self.assertEqual(provenance["inputs"], hosted.reviewed_inputs())
            self.assertEqual(provenance["github"], IDENTITY)
            self.assertIn(hosted.WORKFLOW, provenance["inputs"]["source_sha256"])
            self.assertEqual((context / "Dockerfile.locks").read_bytes(), (ROOT / "Dockerfile.locks").read_bytes())
            for name in LOCK_FILES:
                self.assertEqual((context / "locks/generated" / name).read_bytes(), before[name])
                self.assertEqual((ROOT / "locks/generated" / name).read_bytes(), before[name])
            with self.assertRaisesRegex(ValueError, "already exists"):
                hosted.prepare(context, evidence)
        self.assertEqual((ROOT / "locks/requests.txt").read_bytes(), original)
        check_modules(ROOT)

    def test_verify_real_existing_fixture_and_reject_tampering_or_failed_attempt(self):
        # Exercise the full validator with the promoted hosted resolver bytes,
        # without substituting a version or fabricating a fixed-version inventory.
        with tempfile.TemporaryDirectory(prefix=".hosted-contract-", dir=ROOT) as directory, \
                mock.patch.dict(os.environ, IDENTITY):
            context, evidence = Path(directory) / "context", Path(directory) / "evidence"
            hosted.prepare(context, evidence)
            shutil.copytree(ROOT / "locks/generated", evidence / "resolved")
            hosted.verify(evidence, seal=True)
            (evidence / "job-status.txt").write_text("success\n")
            (evidence / "source-diff.log").write_text("")
            self.assertEqual(hosted.verify(evidence),
                             {name: (ROOT / "locks/generated" / name).read_bytes() for name in LOCK_FILES})
            with self.assertRaisesRegex(ValueError, "already recorded"):
                hosted.verify(evidence, seal=True)
            for filename, corrupt, message in (
                ("job-status.txt", b"failure\n", "failed hosted attempts"),
                ("source-diff.log", b"changed", "failed hosted attempts"),
                ("verified.json", b"{}", "digest mismatch"),
                ("resolved/go.sum", b"invalid", "artifact digest mismatch"),
            ):
                path = evidence / filename
                original = path.read_bytes()
                path.write_bytes(corrupt)
                with self.assertRaisesRegex(ValueError, message):
                    hosted.verify(evidence)
                path.write_bytes(original)
            provenance = json.loads((evidence / "provenance.json").read_text())
            provenance["inputs"]["crypto_request"] += ";echo unsafe"
            (evidence / "provenance.json").write_text(json.dumps(provenance))
            with self.assertRaisesRegex(ValueError, "source/version input drift"):
                hosted.verify(evidence)

    def test_stale_crypto_request_cannot_satisfy_fixed_candidate(self):
        with tempfile.TemporaryDirectory(prefix=".hosted-contract-", dir=ROOT) as directory, \
                mock.patch.dict(os.environ, IDENTITY):
            context, evidence = Path(directory) / "context", Path(directory) / "evidence"
            hosted.prepare(context, evidence)
            shutil.copytree(ROOT / "locks/generated", evidence / "resolved")
            requests = evidence / "resolved/requests.txt"
            requests.write_bytes(requests.read_bytes().replace(
                hosted.FIXED_CRYPTO.encode(), b"golang.org/x/crypto@v0.58.0"))
            with self.assertRaisesRegex(ValueError, "resolver input drift: requests.txt"):
                hosted.verify(evidence, seal=True)
            self.assertFalse((evidence / "verified.json").exists())

    def test_import_review_directory_and_legacy_transport_remain_supported(self):
        artifacts = hosted.CHECKOUT / "artifacts"
        artifacts.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="dex-import-contract-", dir=artifacts) as directory:
            directory = Path(directory)
            log = directory / "resolver.log"
            files = {name: (ROOT / "locks/generated" / name).read_bytes() for name in LOCK_FILES}
            log.write_text(artifact_log(files).replace("/tmp/dex-locks.tar.gz", "/work/dex-locks.tar.gz"))
            output = directory / "reviewed"
            importer.import_artifact(log, output)
            self.assertEqual({name: (output / name).read_bytes() for name in LOCK_FILES}, files)
            with self.assertRaisesRegex(ValueError, "already exists"):
                importer.import_artifact(log, output)
            with self.assertRaises(ValueError):
                importer.import_artifact(None, ROOT / "locks/generated", directory)

    def test_workflow_is_path_scoped_readonly_hosted_and_preserves_failures(self):
        workflow = (hosted.CHECKOUT / hosted.WORKFLOW).read_text()
        for expected in ("pull_request:", "'bridge/idp/**'", "workflow_dispatch:", "contents: read",
                         "runs-on: ubuntu-24.04", "persist-credentials: false", "--target lock-artifact",
                         "--platform linux/amd64", "--no-cache", "--progress=plain", "set -euo pipefail",
                         "github.event.pull_request.head.sha || github.sha", "tee \"$EVIDENCE/resolver.log\"",
                         "git diff --exit-code -- bridge/idp", "github.run_id", "github.run_attempt"):
            self.assertIn(expected, workflow)
        self.assertEqual(workflow.count("if: always()"), 2)
        for forbidden in ("pull_request_target:", "secrets.", "--push", "--load", "go get", "go install",
                          "continue-on-error", "inputs.crypto", "registry", "self-hosted"):
            self.assertNotIn(forbidden, workflow)
        generator = (ROOT / "scripts/generate-locks.sh").read_text()
        self.assertIn('crypto_request=$(grep', generator)
        self.assertIn('"$crypto_request"', generator)
        self.assertNotIn("golang.org/x/crypto@v0.58.0", generator)
        self.assertNotIn("generate-locks", (ROOT / "Dockerfile").read_text())


if __name__ == "__main__":
    unittest.main()
