# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import threading
import unittest


ROOT = Path(__file__).resolve().parents[2]
LOCKS = (ROOT / "mesh-plugin/package-lock.json", ROOT / "runtimes/openclaw/package-lock.json")
REGISTRY = "https://registry.npmjs.org/"
OLD_MIRROR = "https://ms-feed-25.pkgs.visualstudio.com/1es-public/_packaging/npm-public/npm/registry/"


class OpenClawNpmSourceTests(unittest.TestCase):
    def test_both_adapter_locks_use_canonical_integrity_bound_registry_urls(self):
        for lock in LOCKS:
            packages = json.loads(lock.read_text())["packages"]
            with self.subTest(lock=lock):
                self.assertTrue(packages)
                for location, package in packages.items():
                    resolved = package.get("resolved", "")
                    if not resolved or package.get("link"):
                        continue
                    name = package.get("name") or location.rsplit("node_modules/", 1)[-1]
                    if resolved.startswith("file:"):
                        self.assertEqual(name, "@microsoft/agent-governance-sdk")
                        self.assertTrue((lock.parent / resolved[5:]).resolve().is_relative_to(ROOT / "vendor/agt"))
                    else:
                        archive = f'{name}/-/{name.rsplit("/", 1)[-1]}-{package["version"]}.tgz'
                        self.assertEqual(resolved, REGISTRY + archive, location)
                    self.assertRegex(package["integrity"], r"^sha(?:1|512)-[A-Za-z0-9+/]+={0,2}$")


class OpenClawNpmInstallTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cli = os.environ.get("KARS_NPM_CLI")
        cls.npm = ["node", str(Path(cli).resolve())] if cli else ["npm"]
        version = subprocess.check_output([*cls.npm, "--version"], text=True).strip()
        if int(version.split(".")[0]) < 12:
            raise unittest.SkipTest("npm >=12 required; set KARS_NPM_CLI to its bin/npm-cli.js")

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix=".npm-source-test-", dir=ROOT)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.project = self.root / "project"
        self.project.mkdir()
        (self.root / "user.npmrc").write_text("")
        (self.root / "global.npmrc").write_text("")
        self.env = {k: v for k, v in os.environ.items() if not k.lower().startswith("npm_config_")}
        self.env.update({
            "HOME": str(self.root),
            "NPM_CONFIG_USERCONFIG": str(self.root / "user.npmrc"),
            "NPM_CONFIG_GLOBALCONFIG": str(self.root / "global.npmrc"),
            "NPM_CONFIG_CACHE": str(self.root / "cache"),
            "NPM_CONFIG_IGNORE_SCRIPTS": "true",
            "NPM_CONFIG_ALLOW_REMOTE": "none",
            "NPM_CONFIG_UPDATE_NOTIFIER": "false",
            "NPM_CONFIG_FETCH_RETRIES": "0",
            "NPM_CONFIG_FETCH_TIMEOUT": "3000",
        })
        self.archives = {}
        self.requests = []
        owner = self

        class RegistryHandler(BaseHTTPRequestHandler):
            def do_GET(self):
                owner.requests.append(self.path)
                body = owner.archives.get(self.path)
                self.send_response(200 if body else 404)
                self.end_headers()
                if body:
                    self.wfile.write(body)

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), RegistryHandler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.stop_server)
        self.registry = f"http://127.0.0.1:{self.server.server_port}/registry/"
        self.env["NPM_CONFIG_REGISTRY"] = self.registry

    def stop_server(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)

    def fixture(self, name="vitest", version="4.1.12", origin=REGISTRY, direct=False):
        archive = f'{name}/-/{name.rsplit("/", 1)[-1]}-{version}.tgz'
        metadata = {
            "name": name, "version": version,
            "scripts": {"install": "node -e \"require('fs').writeFileSync('lifecycle-ran', 'yes')\""},
        }
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as output:
            data = json.dumps(metadata).encode()
            entry = tarfile.TarInfo("package/package.json")
            entry.size = len(data)
            output.addfile(entry, io.BytesIO(data))
        payload = buffer.getvalue()
        self.archives["/registry/" + archive] = payload
        resolved = origin + archive
        manifest = {
            "name": "install-regression", "version": "1.0.0", "scripts": metadata["scripts"],
            "dependencies": {name: resolved if direct else version},
        }
        lock = {"name": manifest["name"], "version": "1.0.0", "lockfileVersion": 3,
                "requires": True, "packages": {
                    "": manifest,
                    "node_modules/" + name: {
                        "version": version, "resolved": resolved, "hasInstallScript": True,
                        "integrity": "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode(),
                    },
                }}
        (self.project / "package.json").write_text(json.dumps(manifest))
        (self.project / "package-lock.json").write_text(json.dumps(lock))
        return archive

    def install(self):
        before = (self.project / "package-lock.json").read_bytes()
        result = subprocess.run(
            [*self.npm, "ci", "--ignore-scripts", "--no-audit", "--no-fund"],
            cwd=self.project, env=self.env, capture_output=True, text=True, timeout=30,
        )
        self.assertEqual((self.project / "package-lock.json").read_bytes(), before)
        self.assertFalse(list(self.project.rglob("lifecycle-ran")))
        return result

    def test_old_mirror_lock_is_rejected_by_real_npm(self):
        self.fixture(origin=OLD_MIRROR)
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("EALLOWREMOTE", result.stderr)
        self.assertEqual(self.requests, [])

    def test_canonical_lock_uses_configured_registry_and_preserves_scripts_policy(self):
        for name, version in (("vitest", "4.1.12"), ("@noble/ciphers", "2.4.0")):
            with self.subTest(name=name):
                archive = self.fixture(name, version)
                result = self.install()
                self.assertEqual(result.returncode, 0, result.stderr)
                installed = json.loads((self.project / "node_modules" / name / "package.json").read_text())
                self.assertEqual((installed["name"], installed["version"]), (name, version))
                self.assertIn("/registry/" + archive, self.requests)

    def test_unrelated_remote_tarballs_remain_rejected(self):
        for origin, direct in (("https://unrelated.invalid/", False), ("https://unrelated.invalid/", True),
                               (self.registry.replace("/registry/", "/outside/"), False)):
            with self.subTest(origin=origin, direct=direct):
                self.fixture(origin=origin, direct=direct)
                result = self.install()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("EALLOWREMOTE", result.stderr)
                self.assertEqual(self.requests, [])

    def test_integrity_mismatch_is_still_rejected(self):
        self.fixture()
        path = self.project / "package-lock.json"
        lock = json.loads(path.read_text())
        lock["packages"]["node_modules/vitest"]["integrity"] = "sha512-" + base64.b64encode(bytes(64)).decode()
        path.write_text(json.dumps(lock))
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("EINTEGRITY", result.stderr)


if __name__ == "__main__":
    unittest.main()
