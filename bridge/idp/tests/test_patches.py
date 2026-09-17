# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest
import urllib.request
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
MODIFIED = {"./server/oauth2.go", "./connector/saml/saml_test.go"}
ADDED = {"./server/kars_compat_test.go", "./connector/saml/kars_compat_test.go"}


def checksums(path):
    result = {}
    for line in path.read_text().splitlines():
        digest, name = line.split("  ", 1)
        if name in result:
            raise ValueError("duplicate checksum path")
        result[name] = digest
    return result


class PatchContracts(unittest.TestCase):
    def test_all_reviewed_inputs_are_pinned(self):
        patches = ROOT / "patches"
        manifest = checksums(patches / "SHA256SUMS")
        self.assertEqual(set(manifest), {p.name for p in patches.iterdir() if p.name != "SHA256SUMS"})
        for name, digest in manifest.items():
            self.assertEqual(hashlib.sha256((patches / name).read_bytes()).hexdigest(), digest, name)
        self.assertEqual(set(checksums(patches / "upstream.sha256")), MODIFIED)
        self.assertEqual(set(checksums(patches / "patched.sha256")), MODIFIED | ADDED)
        self.assertEqual((patches / "series").read_text().splitlines(), [
            "0001-literal-oauth-error-descriptions.patch",
            "0002-saml-fixture-validation-clock.patch",
        ])

    def test_production_delta_is_exactly_two_literal_format_callers(self):
        lines = (ROOT / "patches/0001-literal-oauth-error-descriptions.patch").read_text().splitlines()
        removed = [line[1:].strip() for line in lines if line.startswith("-") and not line.startswith("---")]
        added = [line[1:].strip() for line in lines if line.startswith("+") and not line.startswith("+++")]
        self.assertEqual(removed, [
            "return nil, newRedirectedErr(errInvalidRequest, description)",
            "return nil, newRedirectedErr(errInvalidRequest, err)",
        ])
        self.assertEqual(added, [
            'return nil, newRedirectedErr(errInvalidRequest, "%s", description)',
            'return nil, newRedirectedErr(errInvalidRequest, "%s", err)',
        ])

    def test_fixture_clock_and_behavior_contracts_remain_test_only(self):
        patch = (ROOT / "patches/0002-saml-fixture-validation-clock.patch").read_text()
        self.assertEqual([line for line in patch.splitlines() if line.startswith("+++ ")],
                         ["+++ b/connector/saml/saml_test.go"])
        self.assertIn("runVerifyWithClock(t, ca, resp, shouldSucceed, nil)", patch)
        self.assertIn("time.Date(2016, time.December, 12, 16, 54, 35, 0, time.UTC)", patch)
        saml = (ROOT / "patches/saml_compat_test.go").read_text()
        self.assertIn("cert.NotBefore.Add(-time.Second)", saml)
        self.assertIn("cert.NotAfter.Add(time.Second)", saml)
        self.assertIn("verifyResponseSig(validator, data)", saml)
        self.assertIn("Cert is not valid at this time", saml)
        oauth = (ROOT / "patches/server_compat_test.go").read_text()
        self.assertIn('method: "%s%[1]s%%"', oauth)
        self.assertIn("server.parseAuthorizationRequest(req)", oauth)
        self.assertIn("redirected.Description != tc.description", oauth)

    def test_integrity_and_hosted_behavior_gates_are_wired(self):
        script = (ROOT / "scripts/apply-source-patches.sh").read_text()
        self.assertLess(script.index('source.upstream.sha256'), script.index("git apply --no-index --check"))
        self.assertIn('git apply --no-index --whitespace=error-all "$patches/$name"', script)
        self.assertNotIn("--unsafe-paths", script)
        self.assertIn('"$patches/patched.sha256" "$packaging/source.upstream.sha256"', script)
        self.assertIn('cmp "$packaging/source.sha256" "$packaging/source.actual.sha256"', script)
        exclusions = (ROOT / "scripts/source-inventory.sh").read_text()
        self.assertEqual(exclusions.count("! -path"), 4)
        self.assertNotIn("oauth2.go", exclusions)
        self.assertNotIn("saml_test.go", exclusions)
        dockerfile = (ROOT / "Dockerfile").read_text()
        self.assertIn("FROM build AS compatibility-tests", dockerfile)
        self.assertIn("FROM compatibility-tests AS upstream-tests", dockerfile)
        self.assertIn("TestKarsAuthorizationErrorDescriptionsLiteral", dockerfile)
        self.assertIn("TestKarsSAMLFixtureCertificateValidity", dockerfile)
        self.assertNotIn("-vet=off", dockerfile)
        self.assertNotIn("-vet=off", (ROOT / "scripts/upstream-tests.sh").read_text())


@unittest.skipUnless(os.environ.get("KARS_DEX_PATCH_NETWORK") == "1",
                     "opt in to bounded verified upstream download; no Go compilation")
class AppliedPatchContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        inputs = dict(line.split("=", 1) for line in (ROOT / "locks/inputs.lock").read_text().splitlines()
                      if line and not line.startswith("#"))
        cls.commit = inputs["DEX_COMMIT"]
        with urllib.request.urlopen(f"https://codeload.github.com/dexidp/dex/tar.gz/{cls.commit}", timeout=30) as response:
            cls.archive = response.read(2 * 1024 * 1024 + 1)
        if len(cls.archive) > 2 * 1024 * 1024:
            raise ValueError("upstream archive exceeds bounded download size")
        if hashlib.sha256(cls.archive).hexdigest() != inputs["DEX_ARCHIVE_SHA256"]:
            raise ValueError("immutable upstream archive hash mismatch")

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix=".patch-contract-", dir=ROOT)
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        with tarfile.open(fileobj=io.BytesIO(self.archive), mode="r:gz") as archive:
            self.assertLess(sum(member.size for member in archive), 16 * 1024 * 1024)
            archive.extractall(self.directory, filter="data")
        self.source = self.directory / ("dex-" + self.commit)
        self.packaging = self.directory / "packaging"
        shutil.copytree(ROOT / "patches", self.packaging / "patches")
        (self.packaging / "scripts").mkdir()
        for name in ("source-inventory.sh", "apply-source-patches.sh"):
            shutil.copyfile(ROOT / "scripts" / name, self.packaging / "scripts" / name)
        inventory = subprocess.run(["sh", str(self.packaging / "scripts/source-inventory.sh")],
                                   cwd=self.source, check=True, capture_output=True, text=True)
        (self.packaging / "source.upstream.sha256").write_text(inventory.stdout)
        self.original = checksums(self.packaging / "source.upstream.sha256")

    def apply(self):
        return subprocess.run(["sh", str(self.packaging / "scripts/apply-source-patches.sh"), str(self.packaging)],
                              cwd=self.source, capture_output=True, text=True)

    def test_exact_patch_application_and_complete_inventory(self):
        original_oauth = (self.source / "server/oauth2.go").read_text()
        self.assertIn("Description string", original_oauth)
        module_paths = ["go.mod", "go.sum", "api/v2/go.mod", "api/v2/go.sum"]
        modules = {name: (self.source / name).read_bytes() for name in module_paths}
        result = self.apply()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        expected = checksums(self.packaging / "source.sha256")
        actual = checksums(self.packaging / "source.actual.sha256")
        self.assertEqual(expected, actual)
        self.assertEqual(set(expected) - set(self.original), ADDED)
        self.assertEqual(set(self.original) - set(expected), set())
        self.assertEqual({name for name in self.original if self.original[name] != expected[name]}, MODIFIED)
        self.assertEqual((self.source / "server/oauth2.go").read_text(),
                         original_oauth.replace("newRedirectedErr(errInvalidRequest, description)",
                                                'newRedirectedErr(errInvalidRequest, "%s", description)')
                         .replace("newRedirectedErr(errInvalidRequest, err)",
                                  'newRedirectedErr(errInvalidRequest, "%s", err)'))
        for name, data in modules.items():
            self.assertEqual((self.source / name).read_bytes(), data)
            self.assertEqual((ROOT / "locks/generated/upstream" / (name + ".snapshot")).read_bytes(), data)
        fixture = ET.parse(self.source / "connector/saml/testdata/oam-resp.xml")
        self.assertEqual(fixture.getroot().attrib["IssueInstant"], "2016-12-12T16:54:35Z")
        self.assertEqual(self.original["./connector/saml/saml.go"], expected["./connector/saml/saml.go"])
        replay = self.apply()
        self.assertNotEqual(replay.returncode, 0, "original-source verification must reject double application")

    def test_patch_tampering_fails_before_source_changes(self):
        patch = self.packaging / "patches/0001-literal-oauth-error-descriptions.patch"
        patch.write_text(patch.read_text() + "\n")
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(hashlib.sha256((self.source / "server/oauth2.go").read_bytes()).hexdigest(),
                         self.original["./server/oauth2.go"])

    def test_original_source_drift_fails_before_patching(self):
        source = self.source / "server/oauth2.go"
        source.write_text(source.read_text() + "\n")
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.packaging / "source.sha256").exists())

    def test_wrong_reviewed_post_hash_is_not_recomputed_from_output(self):
        post = self.packaging / "patches/patched.sha256"
        original_hash = checksums(post)["./server/oauth2.go"]
        post.write_text(post.read_text().replace(original_hash, "0" * 64))
        manifest = self.packaging / "patches/SHA256SUMS"
        old_pin = checksums(manifest)["patched.sha256"]
        manifest.write_text(manifest.read_text().replace(old_pin, hashlib.sha256(post.read_bytes()).hexdigest()))
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.packaging / "source.sha256").exists())

    def test_unreviewed_extra_source_is_rejected(self):
        (self.source / "server/unreviewed.go").write_text("package server\n")
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((self.packaging / "source.actual.sha256").exists())
        self.assertNotEqual((self.packaging / "source.actual.sha256").read_bytes(),
                            (self.packaging / "source.sha256").read_bytes())


if __name__ == "__main__":
    unittest.main()
