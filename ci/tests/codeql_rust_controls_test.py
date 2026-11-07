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
        query_root = self.source / 'rust/ql/src'
        query_root.mkdir(parents=True)
        self.path_query = query_root / 'path.ql'
        self.path_query.write_text('/**\n * @id rust/path-injection\n */\n')
        self.other_query = query_root / 'other.ql'
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

    def test_matching_query_ids_from_another_pack_are_rejected(self):
        stale = self.root / 'bundled-path.ql'
        stale.write_bytes(self.path_query.read_bytes())
        self.queries('source-queries.txt', [stale, self.other_query])
        self.assert_rejected()

    def test_source_symlink_cannot_select_a_bundled_query(self):
        stale = self.root / 'bundled-path.ql'
        stale.write_bytes(self.path_query.read_bytes())
        self.path_query.unlink()
        self.path_query.symlink_to(stale)
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


class AnalysisBindingTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.source = self.root / 'verified source'
        self.suite = self.source / 'rust/ql/src/codeql-suites/rust-security-and-quality.qls'
        self.suite.parent.mkdir(parents=True)
        self.suite.write_text('- description: test suite\n')
        self.library = self.source / 'rust/ql/lib'
        self.library.mkdir()
        self.output = self.root / 'output'
        self.environment = self.root / 'environment'

    def bind(self):
        binding = SCRIPT.split("<<'BINDING'\n", 1)[1].split('\nBINDING\n', 1)[0]
        return subprocess.run(
            [sys.executable, '-B', '-', str(self.source), str(self.output), str(self.environment)],
            input=binding, text=True, capture_output=True, check=False,
        )

    def test_analysis_reuses_the_verified_suite_and_library_without_another_clone(self):
        result = self.bind()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text(), 'suite=./.github/codeql-rust.qls\n')
        self.assertIn('resolve queries ./.github/codeql-rust.qls', SCRIPT)
        suite = (ROOT / '.github/codeql-rust.qls').read_text()
        self.assertIn('- import: codeql-suites/rust-security-and-quality.qls', suite)
        self.assertIn('  from: codeql/rust-queries', suite)
        name, value = self.environment.read_text().strip().split('=', 1)
        self.assertEqual(name, 'CODEQL_ACTION_EXTRA_OPTIONS')
        self.assertEqual(json.loads(value), {
            'database': {
                command: [f'--search-path={self.source.resolve()}']
                for command in ('init', 'run-queries', 'interpret-results')
            },
            'resolve': {'queries': [f'--search-path={self.source.resolve()}']},
        })
        workflow = (ROOT / '.github/workflows/codeql.yml').read_text()
        self.assertIn('id: rust-controls', workflow)
        self.assertIn('steps.rust-controls.outputs.suite', workflow)
        self.assertNotIn('github/codeql/rust/ql/src/', workflow)
        self.assertLess(SCRIPT.index('"$CODEQL_BIN" test run'), SCRIPT.index("<<'BINDING'"))

    def test_missing_suite_cannot_publish_analysis_inputs(self):
        self.suite.unlink()
        self.assertNotEqual(self.bind().returncode, 0)
        self.assertFalse(self.output.exists())
        self.assertFalse(self.environment.exists())

    def test_missing_source_library_cannot_publish_analysis_inputs(self):
        self.library.rmdir()
        self.assertNotEqual(self.bind().returncode, 0)
        self.assertFalse(self.output.exists())
        self.assertFalse(self.environment.exists())

    def test_outputs_preserve_existing_step_environment(self):
        self.environment.write_text('EXISTING=value\n')
        self.output.write_text('existing=value\n')
        self.assertEqual(self.bind().returncode, 0)
        self.assertTrue(self.environment.read_text().startswith('EXISTING=value\n'))
        self.assertTrue(self.output.read_text().startswith('existing=value\n'))


if __name__ == '__main__':
    unittest.main()
