# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Exercise the actual diff gate in disposable local Git repositories."""

import os
from pathlib import Path
import shlex
import shutil
import subprocess
import unittest

from git_fixture import GitFixture

GATE = Path(__file__).resolve().parents[1] / "no-stubs.sh"


class NoStubsTests(GitFixture):
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

    def test_javascript_placeholder_identifiers_are_not_unfinished_implementations(self):
        self.write("bridge/web/src/fixture.tsx", """
type Props = { placeholder?: string };
export function Field({ placeholder }: Props) {
  return <input placeholder={placeholder} />;
}
export const card = { placeholder: "Describe the requested changes" };
export const quoted = { "placeholder": "Search" };
export const styled = <input className="placeholder:text-muted focus:placeholder:text-white" />;
""")
        self.commit()
        result = self.gate()
        self.assertEqual((result.returncode, result.stderr), (0, ""))

    def test_javascript_identifiers_do_not_hide_real_markers_on_the_same_line(self):
        lines = [
            'export const a = { placeholder: "TODO implement" };',
            'export const b = { placeholder: "Search" }; // FIXME validation',
            'export const c = { placeholder: "placeholder" };',
            'export const d = "placeholder";',
            'export function placeholder() { return null; }',
            'export const placeholder = null;',
            'export const e = { placeholder: function placeholder() { return null; } };',
            '// placeholder implementation',
        ]
        self.write("bridge/web/src/fixture.ts", "\n".join(lines) + "\n")
        self.commit()
        result = self.gate()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stderr.splitlines(), [
            "fail: bridge/web/src/fixture.ts: new stub/placeholder introduced: " + line
            for line in lines])

    def test_javascript_diff_positions_do_not_scan_unchanged_markers(self):
        self.write("bridge/web/src/fixture.ts", '// TODO pre-existing\nexport const old = 1;\n')
        self.commit()
        self.base = self.git("rev-parse", "HEAD").strip()
        self.write("bridge/web/src/fixture.ts",
                   '// TODO pre-existing\nexport const replacement = { placeholder: "Search" };\n')
        self.commit()
        result = self.gate()
        self.assertEqual((result.returncode, result.stderr), (0, ""))

    def test_javascript_parser_failure_never_becomes_a_clean_scan(self):
        self.write("bridge/web/src/fixture.ts", "const placeholder = ;\n")
        self.commit()
        result = self.gate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("fail:", result.stderr)

    def test_css_variant_recognition_keeps_other_markers_and_non_css_strings(self):
        lines = [
            'export const a = <input className="placeholder:text-muted TODO" />;',
            'export const b = <input className="placeholder" />;',
            'export const c = "placeholder:text-muted";',
        ]
        self.write("bridge/web/src/fixture.tsx", "\n".join(lines) + "\n")
        self.commit()
        result = self.gate()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stderr.splitlines(), [
            "fail: bridge/web/src/fixture.tsx: new stub/placeholder introduced: " + line
            for line in lines])

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
