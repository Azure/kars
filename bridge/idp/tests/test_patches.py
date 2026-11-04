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
MODIFIED = {"./server/grants/refresh.go", "./server/connectors/password.go"}
ADDED = {"./server/authflow/kars_compat_test.go", "./connector/saml/kars_compat_test.go",
         "./server/grants/kars_refresh_test.go"}
AUTH_REQUEST = "server/authflow/request.go"
PATCH = "0001-refresh-storage-deadlock.patch"


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
        self.assertEqual(set(checksums(patches / "patched.sha256")), MODIFIED | ADDED)
        self.assertEqual(set(checksums(patches / "upstream.sha256")), MODIFIED)
        self.assertEqual((patches / "series").read_text(), PATCH + "\n")
        self.assertEqual([p.name for p in patches.glob("*.patch")], [PATCH])

    def test_upstream_literal_fix_is_not_patched_again(self):
        self.assertFalse((ROOT / "patches/0001-literal-oauth-error-descriptions.patch").exists())
        regression = (ROOT / "patches/server_compat_test.go").read_text()
        self.assertIn("package authflow", regression)
        self.assertIn("newTestHandler(t, func(c *testFlowConfig)", regression)
        self.assertIn("_, _, err := server.parseAuthorizationRequest(req)", regression)

    def test_fixture_clock_and_behavior_contracts_remain_test_only(self):
        self.assertFalse((ROOT / "patches/0002-saml-fixture-validation-clock.patch").exists())
        saml = (ROOT / "patches/saml_compat_test.go").read_text()
        self.assertIn("cert.NotBefore.Add(time.Second)", saml)
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
        self.assertLess(script.index('source.upstream.sha256'), script.index('cp "$patches/server_compat_test.go"'))
        self.assertNotIn("git apply", script)
        self.assertIn('sha256sum --check --strict "$patches/upstream.sha256"', script)
        self.assertIn('patch --batch --forward --fuzz=0 -p1', script)
        self.assertLess(script.index('"$patches/upstream.sha256"'), script.index('patch --batch'))
        self.assertIn('"$patches/patched.sha256" "$packaging/source.upstream.sha256"', script)
        self.assertIn('!($2 in replaced)', script)
        self.assertIn('cp "$patches/refresh_compat_test.go" server/grants/kars_refresh_test.go', script)
        self.assertIn('cmp "$packaging/source.sha256" "$packaging/source.actual.sha256"', script)
        exclusions = (ROOT / "scripts/source-inventory.sh").read_text()
        self.assertEqual(exclusions.count("! -path"), 4)
        self.assertNotIn("oauth2.go", exclusions)
        self.assertNotIn("saml_test.go", exclusions)
        dockerfile = (ROOT / "Dockerfile").read_text()
        self.assertIn("FROM build AS compatibility-tests", dockerfile)
        self.assertIn("FROM compatibility-tests AS session-tests", dockerfile)
        self.assertIn("go test -race -count=1 -v ./server/session ./server/authflow", dockerfile)
        self.assertIn("FROM session-tests AS upstream-tests", dockerfile)
        self.assertIn("TestKarsAuthorizationErrorDescriptionsLiteral", dockerfile)
        self.assertIn("TestKarsSAMLFixtureCertificateValidity", dockerfile)
        self.assertIn("TestKarsRefreshStorageDeadlock", dockerfile)
        self.assertIn("./server/authflow ./connector/saml ./server/grants", dockerfile)
        self.assertIn("go test -race -count=1 -timeout=120s -v", dockerfile)
        self.assertNotIn("-vet=off", dockerfile)
        self.assertNotIn("-vet=off", (ROOT / "scripts/upstream-tests.sh").read_text())


    def test_refresh_regressions_use_real_stores_and_bounded_workers(self):
        regression = (ROOT / "patches/refresh_compat_test.go").read_text()
        for contract in ('//go:build cgo', 'TestKarsRefreshStorageDeadlock',
                         'context.WithTimeout(t.Context(), 30*time.Second)',
                         'exec.CommandContext', '-test.timeout=25s', 'cmd.WaitDelay',
                         'memory.New(logger)', 'dexsql.SQLite3{File: ":memory:"}',
                         'connectors.NewPasswordDB(f.store)', '2 * time.Minute',
                         'concurrent_once', 'deferred_errors', 'legacy=',
                         'stale_claim_and_scope_rejected_before_external_refresh',
                         'wrong_client', 'invalid_secret', 'absolute_expiry', 'idle_expiry',
                         'sessions_use_cached_identity_without_password_refresh',
                         'token.Verify(keys[0].Key)', 'f.store.inside.Load() != 1'):
            self.assertIn(contract, regression)
        self.assertNotIn('time.Sleep', regression)
        patch = (ROOT / "patches" / PATCH).read_text()
        touched = {"./" + line.removeprefix("+++ b/") for line in patch.splitlines()
                   if line.startswith("+++ b/")}
        self.assertEqual(touched, MODIFIED)
        self.assertNotIn('refresh_store.go', patch)
        self.assertIn('PreparePasswordDBRefresh', patch)
        self.assertIn('conn.(passwordDB)', patch)


@unittest.skipUnless(os.environ.get("KARS_DEX_PATCH_NETWORK") == "1" or os.environ.get("KARS_DEX_PATCH_SOURCE"),
                     "opt in to a local source copy or bounded verified upstream download; no Go compilation")
class AppliedPatchContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        inputs = dict(line.split("=", 1) for line in (ROOT / "locks/inputs.lock").read_text().splitlines()
                      if line and not line.startswith("#"))
        cls.commit = inputs["DEX_COMMIT"]
        cls.local_source = os.environ.get("KARS_DEX_PATCH_SOURCE")
        if cls.local_source:
            cls.local_source = Path(cls.local_source).resolve(strict=True)
            return
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
        self.source = self.directory / ("dex-" + self.commit)
        if self.local_source:
            shutil.copytree(self.local_source, self.source)
            # A qualified snapshot may already contain the two older test-only
            # additions. Strip only exact reviewed copies in this private clone;
            # never modify the supplied source or accept patched production code.
            for source, addition in (("server_compat_test.go", "server/authflow/kars_compat_test.go"),
                                     ("saml_compat_test.go", "connector/saml/kars_compat_test.go")):
                path = self.source / addition
                if path.exists():
                    self.assertEqual(path.read_bytes(), (ROOT / "patches" / source).read_bytes())
                    path.unlink()
            for name, digest in checksums(ROOT / "patches/upstream.sha256").items():
                self.assertEqual(hashlib.sha256((self.source / name).read_bytes()).hexdigest(), digest)
            # Qualified build snapshots have already had the reviewed dependency
            # graph applied. Reconstruct the fetched-source inputs from its
            # checked-in original module snapshots, only in this private clone.
            for name in ("go.mod", "go.sum", "api/v2/go.mod", "api/v2/go.sum"):
                shutil.copyfile(ROOT / "locks/generated/upstream" / (name + ".snapshot"), self.source / name)
        else:
            with tarfile.open(fileobj=io.BytesIO(self.archive), mode="r:gz") as archive:
                self.assertLess(sum(member.size for member in archive), 16 * 1024 * 1024)
                archive.extractall(self.directory, filter="data")
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
        original_oauth = (self.source / AUTH_REQUEST).read_text()
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
        self.assertEqual((self.source / AUTH_REQUEST).read_text(), original_oauth)
        self.assertIn('"Unsupported PKCE challenge method (%q).", codeChallengeMethod)', original_oauth)
        self.assertIn('"Cannot use response type \'token\' with redirect_uri \'%s\'.", oauth2.RedirectURIOOB)', original_oauth)
        for name, data in modules.items():
            self.assertEqual((self.source / name).read_bytes(), data)
        fixture = ET.parse(self.source / "connector/saml/testdata/oam-resp.xml")
        self.assertEqual(fixture.getroot().attrib["IssueInstant"], "2026-07-02T00:00:00Z")
        certificate = subprocess.run(["openssl", "x509", "-in", "connector/saml/testdata/oam-ca.pem",
                                      "-noout", "-dates"], cwd=self.source, check=True,
                                     capture_output=True, text=True)
        self.assertIn("notBefore=Jul  2 12:04:52 2026 GMT", certificate.stdout)
        self.assertIn("notAfter=Jul  2 12:04:52 2125 GMT", certificate.stdout)
        self.assertEqual(self.original["./connector/saml/saml.go"], expected["./connector/saml/saml.go"])
        replay = self.apply()
        self.assertNotEqual(replay.returncode, 0, "original-source verification must reject double application")

    def test_refresh_preparation_keeps_rotation_and_session_semantics(self):
        result = self.apply()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        refresh = (self.source / "server/grants/refresh.go").read_text()
        prepare = refresh.index('connectorData, connectorDataErr = g.refreshConnectorData')
        callback = refresh.index('freshIdentity := func')
        rotation = refresh.index('g.issuer.Refresh.Rotate')
        self.assertLess(prepare, callback)
        self.assertLess(callback, rotation)
        self.assertNotIn('g.refreshConnectorData(', refresh[callback:rotation])
        self.assertIn('if passwordRefresh != nil {\n\t\t\treturn passwordRefresh()', refresh[callback:rotation])
        self.assertIn('return g.refreshWithConnector(ctx, conn, connectorData, scopes,', refresh[callback:rotation])
        self.assertIn('if userIdent != nil {\n\t\t\treturn tokens.IdentityFromClaims(userIdent.Claims), nil',
                      refresh[callback:rotation])
        expected = checksums(self.packaging / "source.sha256")
        for path in ('./server/tokens/refresh_store.go', './server/tokens/refresh_lookup.go',
                     './server/tokens/refresh_strategy.go', './storage/sql/sqlite.go',
                     './storage/memory/memory.go'):
            self.assertEqual(self.original[path], expected[path], path)

    def test_production_source_drift_rejects_even_a_recomputed_inventory(self):
        source = self.source / 'server/grants/refresh.go'
        source.write_text(source.read_text() + '\n')
        baseline = self.packaging / 'source.upstream.sha256'
        baseline.write_text(baseline.read_text().replace(self.original['./server/grants/refresh.go'],
                            hashlib.sha256(source.read_bytes()).hexdigest()))
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.source / 'server/grants/kars_refresh_test.go').exists())

    def test_production_patch_tampering_fails_before_source_changes(self):
        patch = self.packaging / 'patches' / PATCH
        patch.write_text(patch.read_text() + '\n')
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        for name in MODIFIED:
            self.assertEqual(hashlib.sha256((self.source / name).read_bytes()).hexdigest(), self.original[name])

    def test_released_session_implementation_and_guards_are_retained(self):
        session = (self.source / "server/session/session.go").read_text()
        for guard in ("ClientSharesWith", "SSOSharedWith", "http.SameSiteLaxMode", "HttpOnly: true",
                      "session.AbsoluteExpiry", "session.IdleExpiry"):
            self.assertIn(guard, session)
        login = (self.source / "server/authflow/sessionlogin.go").read_text()
        self.assertIn("h.Sessions.FindSSO(ctx, session, authReq.ClientID)", login)
        self.assertIn("authReq.MaxAge >= 0", login)
        tests = (self.source / "server/authflow/sessionlogin_test.go").read_text()
        for name in ("TestTrySessionLogin_SSO", "TestClientSharesSessionWith", "TestFindSSOSession",
                     "TestTrySessionLogin_MaxAge", "TestSSO_ConsentAndMFA", "TestGetValidAuthSession"):
            self.assertIn(f"func {name}(t *testing.T)", tests)
        result = self.apply()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        expected = checksums(self.packaging / "source.sha256")
        for path in ("./server/session/session.go", "./server/authflow/sessionlogin.go",
                     "./server/authflow/sessionlogin_test.go", "./storage/sql/crud.go"):
            self.assertEqual(self.original[path], expected[path])

    def test_requested_security_versions_match_reviewed_graph_without_downgrades(self):
        manifest = (self.source / "go.mod").read_text()
        reviewed = (ROOT / "locks/generated/go.mod").read_text()
        for request in (ROOT / "locks/requests.txt").read_text().splitlines():
            module, version = request.split("@")
            selected = next(line.split()[1] for line in reviewed.splitlines()
                            if line.startswith(f"\t{module} "))
            self.assertEqual(selected, version, module)
            original = next(line.split()[1] for line in manifest.splitlines()
                            if line.startswith(f"\t{module} "))
            self.assertGreaterEqual(tuple(map(int, version.removeprefix('v').split('.'))),
                                    tuple(map(int, original.removeprefix('v').split('.'))))
        self.assertIn("\ngo 1.27.0\n", manifest)
        result = subprocess.run(["sh", str(ROOT / "scripts/verify-source-toolchain.sh"),
                                 "go1.26.8", str(self.source / "go.mod")],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires Go 1.27.0; pinned compiler is go1.26.8", result.stderr)
        compiler = next(line.split("=", 1)[1] for line in (ROOT / "locks/inputs.lock").read_text().splitlines()
                        if line.startswith("GO_VERSION="))
        result = subprocess.run(["sh", str(ROOT / "scripts/verify-source-toolchain.sh"),
                                 compiler, str(self.source / "go.mod"), str(self.source / "api/v2/go.mod")],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_current_lock_baselines_match_released_source(self):
        for name in ("go.mod", "go.sum", "api/v2/go.mod", "api/v2/go.sum"):
            self.assertEqual((ROOT / "locks/generated/upstream" / (name + ".snapshot")).read_bytes(),
                             (self.source / name).read_bytes(), name)

    def test_patch_tampering_fails_before_source_changes(self):
        patch = self.packaging / "patches/saml_compat_test.go"
        patch.write_text(patch.read_text() + "\n")
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(hashlib.sha256((self.source / AUTH_REQUEST).read_bytes()).hexdigest(),
                         self.original["./" + AUTH_REQUEST])

    def test_original_source_drift_fails_before_patching(self):
        source = self.source / AUTH_REQUEST
        source.write_text(source.read_text() + "\n")
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.packaging / "source.sha256").exists())

    def test_wrong_reviewed_post_hash_is_not_recomputed_from_output(self):
        post = self.packaging / "patches/patched.sha256"
        original_hash = checksums(post)["./connector/saml/kars_compat_test.go"]
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
