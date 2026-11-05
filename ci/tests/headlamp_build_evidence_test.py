# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


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


if __name__ == "__main__":
    unittest.main()
