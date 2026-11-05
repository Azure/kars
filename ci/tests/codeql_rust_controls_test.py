# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Exercise the scanner's real coverage guard without a local CodeQL download."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = (ROOT / 'ci/codeql-rust-controls.sh').read_text()
GUARD = SCRIPT.split("<<'PY'\n", 1)[1].split('\nPY\n', 1)[0]


class CoverageGuardTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.evidence = self.root / 'evidence'
        self.evidence.mkdir()
        self.source = self.root / 'source'
        self.path_query = self.root / 'path.ql'
        self.path_query.write_text('/**\n * @id rust/path-injection\n */\n')
        self.other_query = self.root / 'other.ql'
        self.other_query.write_text('/**\n * @id rust/other\n */\n')
        self.queries('bundled-queries.txt', [self.path_query, self.other_query])
        self.queries('source-queries.txt', [self.path_query, self.other_query])
        (self.evidence / 'library-path.json').write_text(json.dumps({
            'libraryPath': [str(self.source / 'rust/ql/lib')],
        }))

    def queries(self, name, paths):
        (self.evidence / name).write_text(''.join(f'{path}\n' for path in paths))

    def run_guard(self):
        return subprocess.run(
            [sys.executable, '-B', '-', str(self.evidence), str(self.source)],
            input=GUARD, text=True, capture_output=True, check=False,
        )

    def assert_rejected(self):
        result = self.run_guard()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertFalse((self.evidence / 'query-coverage.json').exists())

    def test_complete_suite_records_coverage(self):
        result = self.run_guard()
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads((self.evidence / 'query-coverage.json').read_text())
        self.assertEqual(report['bundled'], report['corrected'])
        self.assertEqual(report['missing'], [])
        self.assertIn('rust/path-injection', report['corrected'])

    def test_dropped_bundled_query_is_rejected(self):
        self.queries('source-queries.txt', [self.path_query])
        self.assert_rejected()

    def test_path_injection_is_required_even_when_baseline_lacks_it(self):
        for name in ('bundled-queries.txt', 'source-queries.txt'):
            self.queries(name, [self.other_query])
        self.assert_rejected()

    def test_empty_suite_is_rejected(self):
        self.queries('source-queries.txt', [])
        self.assert_rejected()

    def test_ambiguous_query_metadata_is_rejected(self):
        self.other_query.write_text('/**\n * @id rust/one\n * @id rust/two\n */\n')
        self.assert_rejected()

    def test_stale_library_is_rejected(self):
        (self.evidence / 'library-path.json').write_text(json.dumps({
            'libraryPath': ['/bundled/rust/ql/lib'],
        }))
        self.assert_rejected()

    def test_library_prefix_is_not_accepted_as_exact_resolution(self):
        (self.evidence / 'library-path.json').write_text(json.dumps({
            'libraryPath': [str(self.source / 'rust/ql/lib-stale')],
        }))
        self.assert_rejected()


if __name__ == '__main__':
    unittest.main()
