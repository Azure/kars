# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import hashlib
from pathlib import Path
import tempfile
import unittest

from contracts import LOCK_FILES
from test_baselines import artifact_log, import_locks

ROOT = Path(__file__).resolve().parents[1]
NTLM_REQUEST = "github.com/Azure/go-ntlmssp@v0.1.1"


class NTLMSecurityUpdateContracts(unittest.TestCase):
    def test_exact_ntlm_update_does_not_change_crypto_or_other_pins(self):
        requested = (ROOT / "locks/requests.txt").read_text().splitlines()
        self.assertEqual(requested.count(NTLM_REQUEST), 1)
        self.assertIn("golang.org/x/crypto@v0.56.0", requested)
        self.assertIn("GO_VERSION=go1.26.8", (ROOT / "locks/inputs.lock").read_text())
        self.assertEqual(len(requested), len(set(requested)))

    def test_runtime_inventory_is_retained_and_checked_without_filtering_packages(self):
        build = (ROOT / "scripts/build.sh").read_text()
        self.assertIn("go list -deps -json=ImportPath,Module ./cmd/dex > /out/doc/runtime-packages.json", build)
        self.assertIn("go run /packaging/scripts/notices.go /out/doc/runtime-packages.json /out/doc/third-party", build)
        notice = (ROOT / "scripts/notices.go").read_text()
        self.assertIn('pkg.ImportPath == "golang.org/x/crypto/openpgp"', notice)
        self.assertIn('strings.HasPrefix(pkg.ImportPath, "golang.org/x/crypto/openpgp/")', notice)
        self.assertIn('forbidden compiled runtime package: %s (GO-2026-5932)', notice)
        self.assertIn('runtime package inventory does not include the Dex command', notice)
        self.assertEqual(notice.count('strings.HasPrefix(name, "LICENCE")'), 2)
        dockerfile = (ROOT / "Dockerfile").read_text()
        self.assertIn("go test -count=1 /packaging/scripts/notices.go /packaging/scripts/notices_test.go", dockerfile)
        self.assertIn("COPY --from=build /out/doc/ /usr/share/doc/dex/", dockerfile)

    def test_real_upstream_ntlm_overflow_regression_and_package_suite_are_required(self):
        script = (ROOT / "scripts/upstream-tests.sh").read_text()
        test = "TestNewAuthenticateMessage_ChallengeTargetInfoOffsetOverflowNoPanics"
        self.assertIn(f"go test -list '^{test}$'", script)
        self.assertIn(f"grep -Fx '{test}' /out/doc/ntlm-test-list.txt", script)
        self.assertIn("go test -json -count=1 -race github.com/Azure/go-ntlmssp/...", script)
        self.assertIn("cat /out/doc/ntlm-tests.json", script)
        self.assertIn("go test -json -count=1 -race ./...", script)
        self.assertNotIn("-vet=off", script)

    def test_review_import_never_overwrites_an_existing_output(self):
        with tempfile.TemporaryDirectory(prefix="kars-ntlm-review-contract-") as temporary:
            target = Path(temporary) / "reviewed"
            target.mkdir()
            marker = target / "keep"
            marker.write_bytes(b"existing reviewed evidence")
            with self.assertRaisesRegex(ValueError, "already exists"):
                import_locks.import_artifact(Path(temporary) / "unused.log", target)
            self.assertEqual(marker.read_bytes(), b"existing reviewed evidence")

    def test_review_import_rejects_stale_artifacts_without_touching_qualified_locks(self):
        generated = ROOT / "locks/generated"
        before = {name: hashlib.sha256((generated / name).read_bytes()).hexdigest() for name in LOCK_FILES}
        files = {name: (generated / name).read_bytes() for name in LOCK_FILES}
        files["requests.txt"] = "\n".join(
            line for line in files["requests.txt"].decode().splitlines() if line != NTLM_REQUEST
        ).encode() + b"\n"
        with tempfile.TemporaryDirectory(prefix="kars-ntlm-staging-contract-") as temporary:
            directory = Path(temporary)
            log = directory / "stale.log"
            log.write_text(artifact_log(files))
            target = directory / "reviewed"
            with self.assertRaisesRegex(ValueError, "resolver input drift: requests.txt"):
                import_locks.import_artifact(log, target)
            self.assertFalse(target.exists())
            self.assertEqual(sorted(path.name for path in directory.iterdir()), ["stale.log"])
        after = {name: hashlib.sha256((generated / name).read_bytes()).hexdigest() for name in LOCK_FILES}
        self.assertEqual(after, before)

    def test_alternate_review_output_cannot_create_more_active_manifests_in_checkout(self):
        with self.assertRaisesRegex(ValueError, "outside the source checkout"):
            import_locks.import_artifact("unused.log", ROOT / "locks/proposed-ntlm")


if __name__ == "__main__":
    unittest.main()
