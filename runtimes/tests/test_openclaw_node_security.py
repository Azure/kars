# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
UPDATER = ROOT / "sandbox-images/openclaw/update-node-dependencies.cjs"
PAYLOAD = ROOT / "vendor/sandbox-node"
MANIFEST = PAYLOAD / "security-updates.json"


class NodeSecurityPayloadTests(unittest.TestCase):
    def test_complete_published_payloads_and_checksums(self):
        packages = json.loads(MANIFEST.read_text())["packages"]
        self.assertEqual(
            {(p["name"], p["version"]) for p in packages},
            {("@modelcontextprotocol/sdk", "1.31.0"), ("brace-expansion", "5.0.11"),
             ("undici", "6.28.1"), ("undici", "7.29.1")},
        )
        targets = [t["path"] for p in packages for t in p["targets"]]
        self.assertEqual(len(targets), 6)
        self.assertEqual(len(set(targets)), 6)
        self.assertEqual({p.name for p in PAYLOAD.glob("*.tgz")}, {p["archive"] for p in packages})
        for package in packages:
            with self.subTest(package=package["archive"]):
                archive = PAYLOAD / package["archive"]
                self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), package["sha256"])
                with tarfile.open(archive) as source:
                    entries = source.getmembers()
                    self.assertTrue(all(m.isfile() and m.name.startswith("package/") for m in entries))
                    self.assertEqual(len(entries), package["files"])
                    self.assertEqual(len({m.name for m in entries}), len(entries))
                    self.assertEqual(sum(m.size for m in entries), package["publishedBytes"])
                    self.assertTrue(any(m.name.upper().startswith("PACKAGE/LICENSE") for m in entries))
                    metadata = json.load(source.extractfile("package/package.json"))
                    self.assertEqual(metadata["name"], package["name"])
                    self.assertEqual(metadata["version"], package["version"])

    def test_recipe_patches_final_tree_without_runtime_installation(self):
        recipe = (ROOT / "sandbox-images/openclaw/Dockerfile.base").read_text()
        self.assertIn("npm install -g npm@12.2.0", recipe)
        self.assertIn("clawhub@0.23.3 mcporter@0.14.2 @steipete/oracle@0.21.4", recipe)
        self.assertLess(
            recipe.index("COPY --from=builder /usr/local/lib/node_modules/openclaw "),
            recipe.index("RUN node /tmp/kars-node-updates/update-node-dependencies.cjs"),
        )
        self.assertIn("rm -rf /tmp/kars-node-updates", recipe)
        self.assertNotIn("update-node-dependencies", (ROOT / "sandbox-images/openclaw/entrypoint.sh").read_text())


class NodeSecurityUpdaterTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.npm_root = Path(subprocess.check_output(["npm", "root", "-g"], text=True).strip()) / "npm"
        for name in ("pacote", "semver"):
            if not (cls.npm_root / "node_modules" / name / "package.json").is_file():
                raise RuntimeError(f"Installed npm must provide {name}")

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name).resolve()
        self.modules = self.base / "node_modules"
        self.write_package(self.modules / "npm", "npm", "12.2.0")
        for name in ("pacote", "semver"):
            target = self.modules / "npm/node_modules" / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.symlink_to(self.npm_root / "node_modules" / name, target_is_directory=True)
        self.parent = self.modules / "consumer"
        self.write_package(self.parent, "consumer", "2026.9.8", {"library": "1.0.0"})
        self.target = self.parent / "node_modules/library"
        self.write_package(self.target, "library", "1.0.0")
        (self.target / "old.cjs").write_text("module.exports = 'old';\n")
        self.manifest = {
            "packages": [{
                "name": "library", "version": "1.1.0", "archive": "library.tgz",
                "sha256": "", "targets": [
                    {"path": "consumer/node_modules/library", "parent": "consumer", "from": "1.0.0"}
                ],
            }]
        }
        self.archive()

    def write_package(self, directory, name, version, dependencies=None):
        directory.mkdir(parents=True, exist_ok=True)
        data = {"name": name, "version": version, "main": "index.cjs"}
        if dependencies is not None:
            data["dependencies"] = dependencies
        (directory / "package.json").write_text(json.dumps(data) + "\n")

    def archive(self, **fields):
        metadata = {"name": "library", "version": "1.1.0", "main": "index.cjs", **fields}
        archive = self.base / "library.tgz"
        with tarfile.open(archive, "w:gz") as output:
            for name, data in {
                "package.json": json.dumps(metadata).encode(),
                "index.cjs": b"module.exports = 'new';\n",
            }.items():
                info = tarfile.TarInfo("package/" + name)
                info.size, info.mode = len(data), 0o644
                output.addfile(info, io.BytesIO(data))
        self.manifest["packages"][0]["sha256"] = hashlib.sha256(archive.read_bytes()).hexdigest()

    def execute(self, error=None):
        manifest = self.base / "updates.json"
        manifest.write_text(json.dumps(self.manifest))
        result = subprocess.run(
            ["node", str(UPDATER), str(self.modules), str(manifest)],
            text=True, capture_output=True, timeout=30,
        )
        if error is None:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(error, result.stderr)
            self.assertEqual(json.loads((self.target / "package.json").read_text())["version"], "1.0.0")
        self.assertEqual(list(self.modules.glob(".kars-node-updates-*")), [])
        return result

    def test_actual_offline_extraction_replaces_code_and_updates_exact_parent_pin(self):
        (self.target / "node_modules").mkdir()
        (self.target / "node_modules/retained.txt").write_text("retained dependency")
        self.execute()
        self.assertEqual(json.loads((self.parent / "package.json").read_text())["version"], "2026.9.8")
        self.assertEqual(json.loads((self.parent / "package.json").read_text())["dependencies"]["library"], "1.1.0")
        self.assertFalse((self.target / "old.cjs").exists())
        self.assertEqual((self.target / "node_modules/retained.txt").read_text(), "retained dependency")
        self.assertEqual(self.target.stat().st_mode & 0o777, 0o755)
        value = subprocess.check_output(["node", "-e", "console.log(require(process.argv[1]))", str(self.target)], text=True)
        self.assertEqual(value.strip(), "new")
        self.assertEqual(list(self.target.parent.glob(".kars-node-*")), [])
        self.execute()

    def test_checksum_mismatch_fails_before_modification(self):
        self.manifest["packages"][0]["sha256"] = "0" * 64
        self.execute("checksum mismatch")

    def test_missing_required_dependency_fails_before_modification(self):
        self.archive(dependencies={"missing": "^4.0.0"})
        self.execute("Incompatible dependencies")

    def test_incompatible_installed_dependency_fails(self):
        self.write_package(self.target / "node_modules/required", "required", "3.0.0")
        self.archive(dependencies={"required": "^4.0.0"})
        self.execute("Incompatible dependencies")

    def test_compatible_nested_dependency_is_preserved(self):
        self.write_package(self.target / "node_modules/required", "required", "4.0.4")
        self.archive(dependencies={"required": "^4.0.2"})
        self.execute()
        self.assertEqual(json.loads((self.target / "node_modules/required/package.json").read_text())["version"], "4.0.4")

    def test_missing_optional_peer_is_accepted(self):
        self.archive(peerDependencies={"optional": "^1.0.0"}, peerDependenciesMeta={"optional": {"optional": True}})
        self.execute()

    def test_missing_required_peer_is_rejected(self):
        self.archive(peerDependencies={"required": "^1.0.0"})
        self.execute("Incompatible peerDependencies")

    def test_optional_runtime_dependency_is_not_required(self):
        self.archive(dependencies={"optional": "^1.0.0"}, optionalDependencies={"optional": "^1.0.0"})
        self.execute()

    def test_incompatible_node_engine_is_rejected(self):
        self.archive(engines={"node": ">=999"})
        self.execute("does not support")

    def test_wrong_archive_identity_is_rejected(self):
        self.archive(name="impostor")
        self.execute("Unexpected replacement")

    def test_all_targets_checked_before_first_replacement(self):
        self.manifest["packages"][0]["targets"].append({
            "path": "missing/node_modules/library", "parent": "missing", "from": "1.0.0",
        })
        self.execute("ENOENT")

    def test_existing_compatible_parent_range_remains_byte_identical(self):
        self.write_package(self.parent, "consumer", "2026.9.8", {"library": "^1.0.0"})
        previous = (self.parent / "package.json").read_bytes()
        self.execute()
        self.assertEqual((self.parent / "package.json").read_bytes(), previous)

    def test_incompatible_nonexact_parent_range_is_rejected(self):
        self.write_package(self.parent, "consumer", "2026.9.8", {"library": "<1.1.0"})
        self.execute("Incompatible parent")

    def test_target_symlink_is_rejected(self):
        actual = self.base / "actual"
        self.target.rename(actual)
        self.target.symlink_to(actual, target_is_directory=True)
        self.execute("real directory")

    def test_parent_traversal_is_rejected(self):
        self.manifest["packages"][0]["targets"][0]["path"] = "../consumer"
        self.execute("Invalid dependency path")

    def test_unexpected_installed_version_is_not_downgraded(self):
        self.manifest["packages"][0]["targets"][0]["from"] = "0.9.0"
        self.execute("Unexpected installed dependency")

    def bundled_tree(self):
        destination = self.manifest["packages"][0]["targets"][0]
        destination["owner"] = destination.pop("parent")
        self.write_package(self.parent, "consumer", "2026.9.8", {"intermediate": "^2.0.0"})
        intermediate = self.parent / "node_modules/@scope/intermediate"
        self.write_package(intermediate, "@scope/intermediate", "2.0.0", {"library": "^1.0.0"})
        return intermediate

    def test_bundled_transitive_consumer_not_owner_declares_dependency(self):
        intermediate = self.bundled_tree()
        original = (intermediate / "package.json").read_bytes()
        self.execute()
        self.assertEqual((intermediate / "package.json").read_bytes(), original)
        self.assertNotIn("library", json.loads((self.parent / "package.json").read_text())["dependencies"])

    def test_all_hoisted_consumers_must_accept_replacement(self):
        self.bundled_tree()
        self.write_package(self.parent / "node_modules/other", "other", "1.0.0", {"library": "<1.1.0"})
        self.execute("Incompatible parent dependency range")

    def test_shadowed_dependency_does_not_restrict_hoisted_replacement(self):
        intermediate = self.bundled_tree()
        self.write_package(self.parent / "node_modules/other", "other", "1.0.0", {"library": "0.1.0"})
        shadowed = self.parent / "node_modules/other/node_modules/library"
        self.write_package(shadowed, "library", "0.1.0")
        self.execute()
        self.assertEqual(json.loads((shadowed / "package.json").read_text())["version"], "0.1.0")
        self.assertTrue((intermediate / "package.json").is_file())

    def test_bundled_tree_without_actual_consumer_is_rejected(self):
        intermediate = self.bundled_tree()
        self.write_package(intermediate, "@scope/intermediate", "2.0.0", {})
        self.execute("Missing parent dependency declaration")

    def test_duplicate_target_is_rejected_before_mutation(self):
        self.manifest["packages"][0]["targets"] *= 2
        self.execute("Duplicate dependency target")

    def test_real_undici_payloads_preserve_loopback_http(self):
        packages = json.loads(MANIFEST.read_text())["packages"]
        destination = self.target.with_name("undici")
        self.target.rename(destination)
        self.target = destination
        for package in (p for p in packages if p["name"] == "undici"):
            with self.subTest(version=package["version"]):
                previous = package["targets"][0]["from"]
                self.write_package(self.parent, "consumer", "2026.9.8", {"undici": previous})
                self.write_package(self.target, "undici", previous)
                shutil.copyfile(PAYLOAD / package["archive"], self.base / package["archive"])
                self.manifest = {"packages": [{
                    **package,
                    "targets": [{"path": "consumer/node_modules/undici", "parent": "consumer", "from": previous}],
                }]}
                self.execute()
                script = """
const assert = require('node:assert/strict');
const http = require('node:http');
const { once } = require('node:events');
const { fetch, request, Agent } = require(process.argv[1]);
(async () => {
  const server = http.createServer((req, res) => {
    if (req.url === '/redirect') { res.writeHead(302, { Location: '/' }); res.end(); return; }
    const parts = [];
    req.on('data', data => parts.push(data));
    req.on('end', () => {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ method: req.method, body: Buffer.concat(parts).toString() }));
    });
  });
  const agent = new Agent();
  try {
    server.listen(0, '127.0.0.1'); await once(server, 'listening');
    const url = `http://127.0.0.1:${server.address().port}`;
    const options = { dispatcher: agent, signal: AbortSignal.timeout(5000) };
    assert.deepEqual(await (await fetch(url, options)).json(), { method: 'GET', body: '' });
    assert.deepEqual(await (await fetch(url + '/redirect', options)).json(), { method: 'GET', body: '' });
    assert.deepEqual(await (await fetch(url, { ...options, method: 'POST', body: 'payload' })).json(),
      { method: 'POST', body: 'payload' });
    const result = await request(url, { ...options, method: 'POST', body: 'request-api' });
    assert.equal(result.statusCode, 200);
    assert.deepEqual(await result.body.json(), { method: 'POST', body: 'request-api' });
  } finally {
    await agent.close(); server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
"""
                result = subprocess.run(
                    ["node", "-e", script, str(self.target)], capture_output=True, text=True, timeout=15,
                )
                self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
