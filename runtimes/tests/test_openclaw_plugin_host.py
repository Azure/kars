# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
DIRECTORY = ROOT / "sandbox-images/openclaw"
spec = importlib.util.spec_from_file_location("plugin_host", DIRECTORY / "link-plugin-host.py")
plugin_host = importlib.util.module_from_spec(spec)
spec.loader.exec_module(plugin_host)


class PluginHostTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.host = self.root / "host/openclaw"
        self.host.mkdir(parents=True)
        (self.host / "package.json").write_text('{"name":"openclaw","version":"2026.9.8"}')
        self.projects = self.root / "projects"
        self.modules = self.projects / "whatsapp/node_modules"
        self.native = self.modules / "@img/sharp-linux-arm64/lib/native-host.cjs"
        self.native.parent.mkdir(parents=True)
        self.native.write_text("")
        plugin_modules = self.modules / "@openclaw/whatsapp/node_modules"
        plugin_modules.mkdir(parents=True)
        (plugin_modules / "openclaw").symlink_to(self.host, target_is_directory=True)

    def resolve_native_host(self):
        script = """
const fs = require('node:fs'), path = require('node:path'), {createRequire} = require('node:module');
const paths = createRequire(process.argv[1]).resolve.paths('openclaw');
const peer = paths.map(p => path.join(p, 'openclaw')).find(p => fs.existsSync(p));
if (!peer) process.exit(2);
console.log(fs.realpathSync(peer));
"""
        return subprocess.run(["node", "-e", script, str(self.native)],
                              capture_output=True, text=True, timeout=15)

    def test_hoisted_native_dependency_resolves_host_after_binding(self):
        self.assertEqual(self.resolve_native_host().returncode, 2)
        plugin_host.link_host(self.projects, self.host)
        result = self.resolve_native_host()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(Path(result.stdout.strip()), self.host.resolve())
        self.assertEqual(self.native.read_text(), "")

    def test_correct_binding_is_idempotent(self):
        plugin_host.link_host(self.projects, self.host)
        peer = self.modules / "openclaw"
        inode = peer.lstat().st_ino
        plugin_host.link_host(self.projects, self.host)
        self.assertEqual(peer.lstat().st_ino, inode)

    def test_conflicting_directory_or_symlink_is_preserved(self):
        peer = self.modules / "openclaw"
        for kind in ("directory", "wrong-link", "dangling-link"):
            with self.subTest(kind=kind):
                if kind == "directory":
                    peer.mkdir()
                else:
                    peer.symlink_to(self.root if kind == "wrong-link" else self.root / "missing")
                with self.assertRaisesRegex(RuntimeError, "Conflicting"):
                    plugin_host.link_host(self.projects, self.host)
                self.assertTrue(peer.is_dir() if kind == "directory" else peer.is_symlink())
                peer.rmdir() if kind == "directory" else peer.unlink()

    def test_invalid_project_is_rejected_before_creating_links(self):
        (self.projects / "z-invalid").mkdir()
        with self.assertRaisesRegex(RuntimeError, "Invalid installed"):
            plugin_host.link_host(self.projects, self.host)
        self.assertFalse((self.modules / "openclaw").is_symlink())

    def test_symlinked_modules_are_rejected(self):
        project = self.projects / "linked"
        project.mkdir()
        (project / "node_modules").symlink_to(self.modules, target_is_directory=True)
        with self.assertRaisesRegex(RuntimeError, "Invalid installed"):
            plugin_host.link_host(self.projects, self.host)

    def test_missing_host_and_empty_projects_fail(self):
        with self.assertRaises(FileNotFoundError):
            plugin_host.link_host(self.projects, self.root / "missing")
        empty = self.root / "empty"
        empty.mkdir()
        with self.assertRaisesRegex(RuntimeError, "No installed"):
            plugin_host.link_host(empty, self.host)

    def test_binding_runs_only_in_final_build_stage_before_readonly(self):
        recipe = (DIRECTORY / "Dockerfile.base").read_text()
        command = "python3 /usr/local/lib/kars-link-plugin-host.py"
        self.assertEqual(recipe.count(command), 1)
        self.assertLess(recipe.index("COPY --from=builder /opt/openclaw-managed"), recipe.index(command))
        self.assertLess(recipe.index(command), recipe.index("chmod -R a-w /opt/openclaw-managed"))
        self.assertNotIn("kars-link-plugin-host", (DIRECTORY / "entrypoint.sh").read_text())
        workflow = (ROOT / ".github/workflows/image-cache-publish.yml").read_text()
        self.assertIn("'sandbox-images/openclaw/link-plugin-host.py'", workflow)


if __name__ == "__main__":
    unittest.main()
