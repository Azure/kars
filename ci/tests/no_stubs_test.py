# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Exercise the actual diff gate in disposable local Git repositories."""

import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest


GATE = Path(__file__).resolve().parents[1] / "no-stubs.sh"


class NoStubsTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="kars-stub-gate-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Gate Fixture")
        self.git("config", "user.email", "gate@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "core.hooksPath", str(self.root / "empty-hooks"))
        self.git("commit", "--allow-empty", "-qm", "base")
        self.base = self.git("rev-parse", "HEAD").strip()

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True,
                                       stderr=subprocess.PIPE)

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def commit(self):
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")

    def gate(self, extra_env=None):
        return subprocess.run(["bash", str(GATE)], cwd=self.root, text=True,
                              capture_output=True, timeout=30,
                              env={**os.environ, "BASE_REF": self.base, **(extra_env or {})})

    def test_rejects_all_canonical_markers_in_added_lines(self):
        markers = ["// TODO work", "// FIXME work", "// XXX work", "// HACK work",
                   "unimplemented!()", "todo!()", 'panic!("not implemented")',
                   "// placeholder", "value.stub()", "value.mock()",
                   "return None; // placeholder", "return Ok(()); // stub"]
        self.write("bridge/bff/src/fixture.rs", "\n".join(markers) + "\n")
        self.commit()
        result = self.gate()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stderr.splitlines(), [
            "fail: bridge/bff/src/fixture.rs: new stub/placeholder introduced: " + marker
            for marker in markers])

    def test_keeps_existing_inline_override_semantics(self):
        self.write("bridge/web/src/fixture.ts", "// TODO reviewed // ci:stub-ok: fixture\n")
        self.commit()
        result = self.gate()
        self.assertEqual((result.returncode, result.stderr), (0, ""))

    def test_only_checks_production_paths_and_not_test_files(self):
        for path in ("docs/example.ts", "bridge/bff/src/tests/example.rs",
                     "bridge/bff/src/test/example.rs", "bridge/bff/src/tests.rs",
                     "bridge/bff/src/example_test.rs", "bridge/web/src/example.test.ts",
                     "bridge/web/src/example.spec.ts"):
            self.write(path, "// TODO test-only fixture\n")
        self.commit()
        result = self.gate()
        self.assertEqual((result.returncode, result.stderr), (0, ""))

    def test_retains_all_original_production_path_prefixes(self):
        paths = ["shared/", "controller/src/", "inference-router/src/", "cli/src/",
                 "runtimes/openclaw/src/", "sandbox-images/", "cli/profiles/",
                 "bridge/bff/src/", "bridge/web/src/", "bridge/teams-gateway/src/"]
        for path in paths:
            self.write(path + "fixture.rs", "// TODO unfinished\n")
        self.commit()
        result = self.gate()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(len(result.stderr.splitlines()), len(paths))

    def test_does_not_report_unchanged_or_removed_lines(self):
        self.write("controller/src/fixture.rs", "// TODO pre-existing\n")
        self.write("controller/src/removed.rs", "// TODO removed\n")
        self.commit()
        self.base = self.git("rev-parse", "HEAD").strip()
        self.write("controller/src/fixture.rs", "// TODO pre-existing\nfn complete() {}\n")
        (self.root / "controller/src/removed.rs").unlink()
        self.commit()
        result = self.gate()
        self.assertEqual((result.returncode, result.stderr), (0, ""))

    def test_filters_once_per_file_instead_of_spawning_processes_per_line(self):
        self.write("bridge/bff/src/fixture.rs", "fn complete() {}\n" * 250 + "// TODO work\n")
        self.commit()
        calls = self.root / "grep-calls"
        binary = self.root / "bin"
        binary.mkdir()
        grep = shutil.which("grep")
        self.assertIsNotNone(grep)
        wrapper = binary / "grep"
        wrapper.write_text("#!/usr/bin/env bash\nprintf . >> \"$GREP_CALLS\"\n"
                           f"exec {shlex.quote(grep)} \"$@\"\n")
        wrapper.chmod(0o700)
        result = self.gate({"PATH": f"{binary}{os.pathsep}{os.environ['PATH']}",
                            "GREP_CALLS": str(calls)})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(len(result.stderr.splitlines()), 1)
        self.assertLessEqual(len(calls.read_text()), 3)


if __name__ == "__main__":
    unittest.main()
