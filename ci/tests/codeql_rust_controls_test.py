# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Exercise the scanner's real coverage guard without a local CodeQL download."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = (ROOT / 'ci/codeql-rust-controls.sh').read_text()
GUARD = SCRIPT.split("<<'PY'\n", 1)[1].split('\nPY\n', 1)[0]
BOUNDED = ROOT / 'ci/codeql/bounded-value'
MANIFEST = json.loads((BOUNDED / 'manifest.json').read_text())
UPSTREAM = MANIFEST['upstreamFile']
CONTROL_DIRECTORY = 'rust/ql/test/query-tests/security/CWE-502-bounded-value-controls'


def workspace_directory():
    # Keep all offline test artifacts in the checkout, never in the system temp dir.
    return tempfile.TemporaryDirectory(prefix='.codeql-controls-test-', dir=ROOT / 'ci/tests')


class CoverageGuardTests(unittest.TestCase):
    language = 'rust'
    required_query = 'rust/path-injection'

    def setUp(self):
        self.tmp = workspace_directory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.evidence = self.root / 'evidence'
        self.evidence.mkdir()
        self.source = self.root / 'source'
        query_root = self.source / self.language / 'ql/src'
        query_root.mkdir(parents=True)
        self.path_query = query_root / 'path.ql'
        self.path_query.write_text(f'/**\n * @id {self.required_query}\n */\n')
        self.other_query = query_root / 'other.ql'
        self.other_query.write_text(f'/**\n * @id {self.language}/other\n */\n')
        self.queries('bundled-queries.txt', [self.path_query, self.other_query])
        self.queries('source-queries.txt', [self.path_query, self.other_query])
        (self.evidence / 'library-path.json').write_text(json.dumps({
            'libraryPath': [str(self.source / self.language / 'ql/lib')],
        }))

    def queries(self, name, paths):
        (self.evidence / name).write_text(''.join(f'{path}\n' for path in paths))

    def run_guard(self):
        return subprocess.run(
            [sys.executable, '-B', '-', str(self.evidence), str(self.source),
             self.language, self.required_query],
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
        self.assertIn(self.required_query, report['corrected'])

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
            'libraryPath': [str(self.source / self.language / 'ql/lib-stale')],
        }))
        self.assert_rejected()


class ActionsCoverageGuardTests(CoverageGuardTests):
    language = 'actions'
    required_query = 'actions/cache-poisoning/poisonable-step'


class AnalysisBindingTests(unittest.TestCase):
    language = 'rust'

    def setUp(self):
        self.tmp = workspace_directory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.source = self.root / 'verified source'
        self.suite = (self.source / self.language / 'ql/src/codeql-suites'
                      / f'{self.language}-security-and-quality.qls')
        self.suite.parent.mkdir(parents=True)
        self.suite.write_text('- description: test suite\n')
        self.library = self.source / self.language / 'ql/lib'
        self.library.mkdir()
        self.output = self.root / 'output'
        self.environment = self.root / 'environment'

    def bind(self):
        binding = SCRIPT.split("<<'BINDING'\n", 1)[1].split('\nBINDING\n', 1)[0]
        return subprocess.run(
            [sys.executable, '-B', '-', str(self.source), str(self.output),
             str(self.environment), self.language],
            input=binding, text=True, capture_output=True, check=False,
        )

    def test_analysis_reuses_the_verified_suite_and_library_without_another_clone(self):
        result = self.bind()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_text(), f'suite=./.github/codeql-{self.language}.qls\n')
        self.assertIn('resolve queries "./.github/codeql-$language.qls"', SCRIPT)
        suite = (ROOT / f'.github/codeql-{self.language}.qls').read_text()
        self.assertIn(f'- import: codeql-suites/{self.language}-security-and-quality.qls', suite)
        self.assertIn(f'  from: codeql/{self.language}-queries', suite)
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
        self.assertIn(f'id: {self.language}-controls', workflow)
        self.assertIn(f'steps.{self.language}-controls.outputs.suite', workflow)
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


class ActionsAnalysisBindingTests(AnalysisBindingTests):
    language = 'actions'

    def test_actions_runs_pinned_positive_and_negative_controls(self):
        workflow = (ROOT / '.github/workflows/codeql.yml').read_text()
        self.assertIn('CODEQL_SOURCE_SHA: dc2387baefc27bf00e6a8e6ecd2d4c6d3c8e07d7', workflow)
        self.assertIn('CODEQL_LANGUAGE: actions', workflow)
        self.assertIn('$CODEQL_SOURCE/actions/ql/test/query-tests/Security/CWE-349', SCRIPT)
        self.assertNotIn('--learn', SCRIPT)
        self.assertIn('set -euo pipefail', SCRIPT)
        self.assertIn('test "$(git -C "$CODEQL_SOURCE" rev-parse HEAD)" = "$CODEQL_SOURCE_SHA"', SCRIPT)
        self.assertIn('name: codeql-${{ matrix.language }}-controls', workflow)


class BoundedValueExtensionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = workspace_directory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.payload = self.root / 'bounded-value'
        shutil.copytree(BOUNDED, self.payload)
        self.source = self.root / 'isolated source'
        self.extension = self.source / UPSTREAM
        self.extension.parent.mkdir(parents=True)
        self.original = (BOUNDED / 'fixtures/UnsafeDeserializationExtensions.qll').read_bytes()
        self.extension.write_bytes(self.original)
        self.helper = self.extension.with_name('BoundedJsonValue.qll')
        self.controls = self.source / CONTROL_DIRECTORY
        self.controls.parent.mkdir(parents=True)
        self.evidence = self.root / 'evidence'

    def apply(self, commit=None):
        return subprocess.run(
            [sys.executable, '-B', str(self.payload / 'apply.py'), str(self.source),
             str(self.evidence), commit or MANIFEST['upstreamCommit']],
            text=True, capture_output=True, check=False,
        )

    def assert_rejected(self, message):
        before = self.extension.read_bytes()
        result = self.apply()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(message, result.stderr)
        self.assertEqual(self.extension.read_bytes(), before)
        self.assertFalse((self.evidence / 'bounded-value-extension.json').exists())

    def test_frozen_ch2b_inputs_and_expectations_are_unchanged(self):
        self.assertEqual(MANIFEST['validatedSourceManifestSha256'],
                         'e856e935770c7738ea867f0c63b8e48641f5c6b798eceb20573cba768fb94e7d')
        self.assertEqual(MANIFEST['upstreamCommit'], 'fdf5c8481ffef04e04c7376167fb5ce25aae8453')
        self.assertEqual(MANIFEST['codeqlCliVersion'], '2.27.4')
        self.assertEqual(MANIFEST['validatedRun'], 'ch2b')
        self.assertEqual(hashlib.sha256(self.original).hexdigest(), MANIFEST['upstreamSha256'])
        for name, expected in MANIFEST['controls'].items():
            self.assertEqual(hashlib.sha256((BOUNDED / 'controls' / name).read_bytes()).hexdigest(),
                             expected, name)
        for name, count, expected in (
            ('BoundedValueControl.expected', 17,
             'd1e2450021fecf81fd2befad87fffbcfd471333053b8a157154ad153d2685d3b'),
            ('IntegratedFlowControl.expected', 9,
             '7132559f3721746aeafc38b6700edd43d1a2e6fb0e4ea1fe1f6d3449e93d2461'),
        ):
            content = (BOUNDED / 'controls' / name).read_bytes()
            self.assertEqual(hashlib.sha256(content).hexdigest(), expected)
            self.assertEqual(len(content.splitlines()), count)

    def test_exact_proven_patch_and_identical_control_and_analysis_helpers(self):
        result = self.apply()
        self.assertEqual(result.returncode, 0, result.stderr)
        patched = self.extension.read_bytes()
        self.assertEqual(hashlib.sha256(patched).hexdigest(),
                         'c0da0548958ba44252eab00c1a70f99b0afd5ab8a269d85d6ff0dea2f38111eb')
        self.assertEqual(patched.decode().replace(
            'private import codeql.rust.security.BoundedJsonValue\n', '', 1,
        ).replace(' and not boundedStandardValueModeledSink(this)', '', 1).encode(), self.original)
        self.assertEqual(self.helper.read_bytes(), (self.controls / self.helper.name).read_bytes())
        for name in MANIFEST['controls']:
            self.assertEqual((self.controls / name).read_bytes(), (BOUNDED / 'controls' / name).read_bytes())
        report = json.loads((self.evidence / 'bounded-value-extension.json').read_text())
        self.assertEqual(report['source'], str(self.source))
        self.assertEqual(report['installedLibrary'], str(self.helper))
        self.assertEqual(report['installedControls'], str(self.controls))
        for key, value in MANIFEST.items():
            self.assertEqual(report[key], value)

    def test_wrong_revision_is_rejected_before_changes(self):
        result = self.apply('0' * 40)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('validated upstream revision', result.stderr)
        self.assertEqual(self.extension.read_bytes(), self.original)
        self.assertFalse(self.helper.exists())
        self.assertFalse(self.controls.exists())

    def test_upstream_source_drift_is_rejected_before_changes(self):
        self.extension.write_bytes(self.original + b'\n')
        self.assert_rejected('upstream source hash mismatch')
        self.assertFalse(self.helper.exists())
        self.assertFalse(self.controls.exists())

    def test_source_symlink_is_rejected(self):
        external = self.root / 'external.qll'
        external.write_bytes(self.original)
        self.extension.unlink()
        self.extension.symlink_to(external)
        self.assert_rejected('must not be a symlink')
        self.assertEqual(external.read_bytes(), self.original)

    def test_existing_helper_is_not_overwritten(self):
        self.helper.write_text('existing helper\n')
        self.assert_rejected('already exist')
        self.assertEqual(self.helper.read_text(), 'existing helper\n')

    def test_existing_control_directory_is_not_overwritten(self):
        self.controls.mkdir()
        self.assert_rejected('already exist')
        self.assertEqual(list(self.controls.iterdir()), [])

    def test_every_changed_control_is_rejected_before_any_writes(self):
        for name in MANIFEST['controls']:
            with self.subTest(name=name):
                path = self.payload / 'controls' / name
                original = path.read_bytes()
                path.write_bytes(original + b'\n')
                self.assert_rejected(f'control hash mismatch: {name}')
                self.assertFalse(self.helper.exists())
                self.assertFalse(self.controls.exists())
                path.write_bytes(original)

    def test_unexpected_control_is_rejected(self):
        (self.payload / 'controls/Unexpected.ql').write_text('select 1\n')
        self.assert_rejected('control file set mismatch')

    def test_missing_control_is_rejected(self):
        (self.payload / 'controls/IntegratedFlowControl.expected').unlink()
        self.assert_rejected('control file set mismatch')

    def test_patched_digest_mismatch_is_rejected_before_writes(self):
        manifest = dict(MANIFEST, patchedSha256='0' * 64)
        (self.payload / 'manifest.json').write_text(json.dumps(manifest))
        self.assert_rejected('patched source hash mismatch')
        self.assertFalse(self.helper.exists())
        self.assertFalse(self.controls.exists())


class ScannerPipelineTests(unittest.TestCase):
    """Run the real shell/patch/guards with offline Git, CodeQL and rustc doubles."""

    language = 'rust'
    commit = MANIFEST['upstreamCommit']
    required_query = 'rust/path-injection'
    library_query = 'queries/security/CWE-022/TaintedPath.ql'
    control_paths = [
        'rust/ql/test/query-tests/security/CWE-022',
        'rust/ql/test/library-tests/dataflow/lambdas',
        'rust/ql/test/library-tests/dataflow/local',
        'rust/ql/test/library-tests/dataflow/models',
        CONTROL_DIRECTORY,
        'rust/ql/test/query-tests/security/CWE-502',
        'rust/ql/test/query-tests/unusedentities',
    ]

    def setUp(self):
        self.tmp = workspace_directory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.template = self.root / 'template'
        self.source = self.root / 'source with spaces'
        self.evidence = self.root / 'evidence'
        self.output = self.root / 'output'
        self.environment = self.root / 'environment'
        self.events = self.root / 'events.jsonl'
        self.query_root = self.template / self.language / 'ql/src'
        suite = self.query_root / f'codeql-suites/{self.language}-security-and-quality.qls'
        suite.parent.mkdir(parents=True)
        suite.write_text('- description: offline pipeline fixture\n')
        library = self.template / self.language / 'ql/lib'
        library.mkdir()
        for relative, query_id in ((self.library_query, self.required_query),
                                   ('other.ql', f'{self.language}/other')):
            query = self.query_root / relative
            query.parent.mkdir(parents=True, exist_ok=True)
            query.write_text(f'/**\n * @id {query_id}\n */\n')
        for relative in self.control_paths:
            if relative != CONTROL_DIRECTORY:
                directory = self.template / relative
                directory.mkdir(parents=True)
                (directory / 'retained.expected').write_text('unchanged upstream expectation\n')
        if self.language == 'rust':
            extension = self.template / UPSTREAM
            extension.parent.mkdir(parents=True)
            shutil.copyfile(BOUNDED / 'fixtures/UnsafeDeserializationExtensions.qll', extension)
            (self.template / 'rust/ql/test/setup.sh').write_text('exit 0\n')
            (self.template / 'rust/extractor/src/nightly-toolchain').mkdir(parents=True)
        bundled = self.root / 'bundled'
        bundled.mkdir()
        shutil.copyfile(self.query_root / self.library_query, bundled / 'required.ql')
        shutil.copyfile(self.query_root / 'other.ql', bundled / 'other.ql')
        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        stub = '#!' + sys.executable + '\n' + textwrap.dedent('''\
            import hashlib
            import json
            import os
            from pathlib import Path
            import shutil
            import sys

            name = Path(sys.argv[0]).name
            args = sys.argv[1:]
            source = Path(os.environ['CODEQL_SOURCE'])
            extension = source / os.environ['TEST_UPSTREAM_FILE']
            event = {'tool': name, 'args': args}
            if extension.exists():
                event['extensionSha256'] = hashlib.sha256(extension.read_bytes()).hexdigest()
            with Path(os.environ['TEST_EVENTS']).open('a') as handle:
                handle.write(json.dumps(event) + '\\n')
            if name == 'git':
                if args[0] == 'init':
                    shutil.copytree(os.environ['TEST_TEMPLATE'], source)
                elif 'rev-parse' in args:
                    print(os.environ['CODEQL_SOURCE_SHA'])
            elif name == 'rustc':
                print('offline pinned rustc fixture')
            elif name == 'codeql':
                if args == ['version']:
                    print('CodeQL command-line toolchain release 2.27.4.')
                elif args[:2] == ['resolve', 'queries']:
                    if args[2].startswith('codeql/'):
                        root = Path(os.environ['TEST_BUNDLED'])
                        paths = [root / 'required.ql', root / 'other.ql']
                    else:
                        root = source / os.environ['CODEQL_LANGUAGE'] / 'ql/src'
                        paths = [root / os.environ['TEST_LIBRARY_QUERY'], root / 'other.ql']
                    print('\\n'.join(map(str, paths)))
                elif args[:2] == ['resolve', 'library-path']:
                    path = source / os.environ['CODEQL_LANGUAGE'] / 'ql/lib'
                    print(json.dumps({'libraryPath': [str(path)]}))
                elif args[:2] == ['pack', 'install']:
                    pass
                elif args[:2] == ['test', 'run']:
                    print('offline control invocation; not a CodeQL behavior result', flush=True)
                    sys.exit(int(os.environ.get('TEST_CONTROL_EXIT', '0')))
                else:
                    raise SystemExit('unexpected CodeQL command: ' + repr(args))
            else:
                raise SystemExit('unexpected executable')
        ''')
        for executable in ('git', 'codeql', 'rustc'):
            path = bin_dir / executable
            path.write_text(stub)
            path.chmod(0o755)
        self.env = {
            **os.environ,
            'PATH': str(bin_dir) + os.pathsep + os.environ['PATH'],
            'PYTHONDONTWRITEBYTECODE': '1',
            'CODEQL_BIN': str(bin_dir / 'codeql'),
            'CODEQL_SOURCE': str(self.source),
            'CODEQL_EVIDENCE': str(self.evidence),
            'CODEQL_SOURCE_SHA': self.commit,
            'CODEQL_LANGUAGE': self.language,
            'GITHUB_OUTPUT': str(self.output),
            'GITHUB_ENV': str(self.environment),
            'TEST_TEMPLATE': str(self.template),
            'TEST_BUNDLED': str(bundled),
            'TEST_EVENTS': str(self.events),
            'TEST_UPSTREAM_FILE': UPSTREAM,
            'TEST_LIBRARY_QUERY': self.library_query,
        }

    def run_pipeline(self):
        return subprocess.run(['bash', str(ROOT / 'ci/codeql-rust-controls.sh')], cwd=ROOT,
                              env=self.env, text=True, capture_output=True, check=False)

    def read_events(self):
        return [json.loads(line) for line in self.events.read_text().splitlines()]

    def test_pipeline_preserves_all_controls_coverage_and_source_binding(self):
        result = self.run_pipeline()
        self.assertEqual(result.returncode, 0, result.stderr)
        events = self.read_events()
        tests = [event for event in events if event['args'][:2] == ['test', 'run']]
        self.assertEqual(len(tests), 1)
        self.assertEqual(tests[0]['args'], [
            'test', 'run', f'--search-path={self.source}',
            *(str(self.source / path) for path in self.control_paths),
        ])
        for path in self.control_paths:
            if path != CONTROL_DIRECTORY:
                self.assertEqual((self.source / path / 'retained.expected').read_text(),
                                 'unchanged upstream expectation\n')
        self.assertEqual(self.output.read_text(), f'suite=./.github/codeql-{self.language}.qls\n')
        options = json.loads(self.environment.read_text().strip().split('=', 1)[1])
        for command in ('init', 'run-queries', 'interpret-results'):
            self.assertEqual(options['database'][command], [f'--search-path={self.source}'])
        self.assertEqual(options['resolve']['queries'], [f'--search-path={self.source}'])
        report = json.loads((self.evidence / 'query-coverage.json').read_text())
        self.assertEqual(report['bundled'], report['corrected'])
        self.assertIn(self.required_query, report['corrected'])
        self.assertEqual((self.evidence / 'source-sha.txt').read_text().strip(), self.commit)
        self.assertIn('offline control invocation', (self.evidence / 'control-tests.log').read_text())
        self.assertNotIn('--learn', SCRIPT)
        if self.language == 'rust':
            self.assertEqual(tests[0]['extensionSha256'], MANIFEST['patchedSha256'])
            for event in events:
                if event['tool'] == 'codeql' and (
                    event['args'][:2] in (['pack', 'install'], ['resolve', 'library-path'])
                    or event['args'][2:3] == ['./.github/codeql-rust.qls']
                ):
                    self.assertEqual(event['extensionSha256'], MANIFEST['patchedSha256'])
            installed = json.loads((self.evidence / 'bounded-value-extension.json').read_text())
            self.assertEqual(installed['installedLibrary'],
                             str((self.source / UPSTREAM).with_name('BoundedJsonValue.qll')))
            for name, expected in MANIFEST['controls'].items():
                self.assertEqual(hashlib.sha256(
                    (self.source / CONTROL_DIRECTORY / name).read_bytes()).hexdigest(), expected)
        else:
            self.assertFalse((self.evidence / 'bounded-value-extension.json').exists())
            self.assertFalse((self.source / 'rust').exists())
            self.assertFalse(any(event['tool'] == 'rustc' for event in events))

    def test_failed_controls_cannot_export_analysis_inputs(self):
        self.env['TEST_CONTROL_EXIT'] = '2'
        result = self.run_pipeline()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.output.exists())
        self.assertFalse(self.environment.exists())
        self.assertTrue((self.evidence / 'query-coverage.json').exists())
        self.assertIn('offline control invocation', (self.evidence / 'control-tests.log').read_text())


class RustPatchPipelineFailureTests(ScannerPipelineTests):
    def test_upstream_mismatch_stops_before_pack_install_controls_or_binding(self):
        with (self.template / UPSTREAM).open('a') as handle:
            handle.write('\n')
        result = self.run_pipeline()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('upstream source hash mismatch', result.stderr)
        self.assertFalse(self.output.exists())
        self.assertFalse(self.environment.exists())
        self.assertFalse((self.evidence / 'bounded-value-extension.json').exists())
        self.assertFalse(any(event['args'][:2] in (['pack', 'install'], ['test', 'run'])
                             for event in self.read_events()))

    def test_revision_mismatch_stops_before_controls_or_binding(self):
        self.env['CODEQL_SOURCE_SHA'] = '0' * 40
        result = self.run_pipeline()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('validated upstream revision', result.stderr)
        self.assertFalse(self.output.exists())
        self.assertFalse(self.environment.exists())
        self.assertFalse(any(event['args'][:2] == ['test', 'run'] for event in self.read_events()))


class ActionsScannerPipelineTests(ScannerPipelineTests):
    language = 'actions'
    commit = 'dc2387baefc27bf00e6a8e6ecd2d4c6d3c8e07d7'
    required_query = 'actions/cache-poisoning/poisonable-step'
    library_query = 'Security/CWE-349/CachePoisoningViaPoisonableStep.ql'
    control_paths = ['actions/ql/test/query-tests/Security/CWE-349']


if __name__ == '__main__':
    unittest.main()
