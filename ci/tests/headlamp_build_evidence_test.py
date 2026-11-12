# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import re
import stat
import subprocess


class HeadlampEvidencePackagingTests(unittest.TestCase):
    def test_read_only_inputs_keep_traversable_directory_under_restrictive_umask(self):
        root = Path(__file__).resolve().parents[2]
        recipe = (root / "sandbox-images/headlamp/Dockerfile").read_text()
        command = re.search(
            r"^RUN install -d -m0755 ../frontend-build[^\n]*",
            recipe.replace("\\\n", ""), re.M,
        )
        self.assertIsNotNone(command, "Stage evidence with separate directory and file modes")
        self.assertIn(
            "COPY --from=frontend /build/frontend-build /usr/share/kars/headlamp/frontend-build/",
            recipe,
        )
        self.assertNotIn("COPY --from=frontend --chmod=0444", recipe)
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            source = parent / "frontend"
            source.mkdir()
            contents = {name: (name + "\n").encode() for name in MODULE.INPUTS}
            for name, data in contents.items():
                (source / name).write_bytes(data)
                (source / name).chmod(0o400)
            subprocess.run(
                ["sh", "-eu", "-c", "umask 077\n" + command.group()[4:]],
                cwd=source, check=True, capture_output=True, text=True,
            )
            exported = parent / "frontend-build"
            self.assertEqual(stat.S_IMODE(exported.stat().st_mode), 0o755)
            self.assertEqual(set(p.name for p in exported.iterdir()), set(contents))
            for name, data in contents.items():
                self.assertEqual((exported / name).read_bytes(), data)
                self.assertEqual(stat.S_IMODE((exported / name).stat().st_mode), 0o444)
                self.assertEqual((source / name).read_bytes(), data)
                self.assertEqual(stat.S_IMODE((source / name).stat().st_mode), 0o400)
            (parent / "frontend-evidence.json").write_text(json.dumps({
                "buildInputs": {name: hashlib.sha256(data).hexdigest() for name, data in contents.items()}
            }))
            MODULE.verify(parent)


SPEC = importlib.util.spec_from_file_location(
    "headlamp_build_evidence", Path(__file__).resolve().parents[1] / "headlamp_build_evidence.py"
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class HeadlampBuildEvidenceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.inputs = self.root / "frontend-build"
        self.inputs.mkdir()
        self.hashes = {}
        for name in MODULE.INPUTS:
            data = (name + "\n").encode()
            (self.inputs / name).write_bytes(data)
            self.hashes[name] = hashlib.sha256(data).hexdigest()
        self.write_evidence()

    def write_evidence(self):
        (self.root / "frontend-evidence.json").write_text(json.dumps({"buildInputs": self.hashes}))

    def test_exact_exported_inputs_pass(self):
        MODULE.verify(self.root)

    def test_changed_input_fails(self):
        (self.inputs / "package-lock.json").write_text("changed")
        with self.assertRaisesRegex(ValueError, "digest mismatch"):
            MODULE.verify(self.root)

    def test_missing_input_fails(self):
        (self.inputs / "vite.config.ts").unlink()
        with self.assertRaisesRegex(ValueError, "Missing regular"):
            MODULE.verify(self.root)

    def test_missing_binding_fails(self):
        self.hashes.pop("package.json")
        self.write_evidence()
        with self.assertRaisesRegex(ValueError, "all three"):
            MODULE.verify(self.root)

    def test_unexpected_path_fails(self):
        self.hashes["../outside"] = "0" * 64
        self.write_evidence()
        with self.assertRaisesRegex(ValueError, "all three"):
            MODULE.verify(self.root)

    def test_invalid_digest_fails(self):
        self.hashes["package.json"] = "not-a-digest"
        self.write_evidence()
        with self.assertRaisesRegex(ValueError, "Invalid"):
            MODULE.verify(self.root)

    def test_symlink_input_fails(self):
        path = self.inputs / "package.json"
        external = self.root / "outside.json"
        external.write_bytes(path.read_bytes())
        path.unlink()
        path.symlink_to(external)
        with self.assertRaisesRegex(ValueError, "Missing regular"):
            MODULE.verify(self.root)


class HeadlampScanInventoryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.report = Path(temporary.name) / "scan.json"
        self.result = {
            "Target": "package-lock.json", "Type": "npm",
            "Packages": [{"Name": "vite", "Version": "6.4.5"}],
        }

    def verify(self, results):
        self.report.write_text(json.dumps({"Results": results}))
        MODULE.verify_npm_inventory(self.report)

    def test_nonempty_npm_lock_inventory_passes(self):
        self.verify([self.result])

    def test_absolute_lock_target_passes(self):
        self.result["Target"] = "/work/frontend-build/package-lock.json"
        self.verify([self.result])

    def test_empty_or_absent_results_fail(self):
        for results in (None, [], {}):
            with self.subTest(results=results), self.assertRaisesRegex(ValueError, "nonempty npm"):
                self.verify(results)

    def test_go_only_report_does_not_qualify_the_plugin_lock(self):
        self.result.update(Target="node_modules/tool/example/go.mod", Type="gomod")
        with self.assertRaisesRegex(ValueError, "nonempty npm"):
            self.verify([self.result])

    def test_non_lock_npm_inventory_does_not_qualify_the_lock(self):
        self.result["Target"] = "package.json"
        with self.assertRaisesRegex(ValueError, "nonempty npm"):
            self.verify([self.result])

    def test_absent_empty_or_malformed_packages_fail(self):
        for packages in (None, [], [{}], [{"Name": "vite"}], [{"Name": "vite", "Version": ""}]):
            with self.subTest(packages=packages), self.assertRaisesRegex(ValueError, "nonempty npm"):
                self.result["Packages"] = packages
                self.verify([self.result])

    def test_unrelated_result_cannot_hide_missing_npm_inventory(self):
        self.result.pop("Packages")
        with self.assertRaisesRegex(ValueError, "nonempty npm"):
            self.verify([{"Type": "gomod", "Packages": [{"Name": "example", "Version": "1.0"}]}, self.result])


if __name__ == "__main__":
    unittest.main()
