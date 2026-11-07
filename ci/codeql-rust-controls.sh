#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -euo pipefail

: "${CODEQL_BIN:?CodeQL executable required}"
: "${CODEQL_SOURCE:?Isolated upstream source directory required}"
: "${CODEQL_EVIDENCE:?Evidence directory required}"
: "${CODEQL_SOURCE_SHA:?Immutable upstream commit required}"

mkdir -p "$CODEQL_EVIDENCE"
test ! -e "$CODEQL_SOURCE"
git init --quiet "$CODEQL_SOURCE"
git -C "$CODEQL_SOURCE" remote add origin https://github.com/github/codeql.git
git -C "$CODEQL_SOURCE" fetch --quiet --depth=1 origin "$CODEQL_SOURCE_SHA"
git -C "$CODEQL_SOURCE" checkout --quiet --detach FETCH_HEAD
test "$(git -C "$CODEQL_SOURCE" rev-parse HEAD)" = "$CODEQL_SOURCE_SHA"
printf '%s\n' "$CODEQL_SOURCE_SHA" > "$CODEQL_EVIDENCE/source-sha.txt"
"$CODEQL_BIN" version > "$CODEQL_EVIDENCE/cli-version.txt"

# Resolve bundled coverage before introducing the corrected source workspace.
"$CODEQL_BIN" resolve queries codeql/rust-queries:codeql-suites/rust-security-and-quality.qls \
  --format=text > "$CODEQL_EVIDENCE/bundled-queries.txt"
"$CODEQL_BIN" pack install "$CODEQL_SOURCE/rust/ql/src"
"$CODEQL_BIN" pack install "$CODEQL_SOURCE/rust/ql/test"
"$CODEQL_BIN" resolve queries ./.github/codeql-rust.qls \
  --search-path="$CODEQL_SOURCE" --format=text > "$CODEQL_EVIDENCE/source-queries.txt"
"$CODEQL_BIN" resolve library-path \
  --query="$CODEQL_SOURCE/rust/ql/src/queries/security/CWE-022/TaintedPath.ql" \
  --search-path="$CODEQL_SOURCE" --format=json > "$CODEQL_EVIDENCE/library-path.json"
python3 - "$CODEQL_EVIDENCE" "$CODEQL_SOURCE" <<'PY'
import json
from pathlib import Path
import re
import sys

evidence, source = map(Path, sys.argv[1:])
def query_ids(name):
    paths = (evidence / name).read_text().splitlines()
    if not paths:
        raise SystemExit('empty resolved query suite')
    ids = set()
    for path in paths:
        path = Path(path.strip())
        if name == 'source-queries.txt' and not path.resolve().is_relative_to(
            (source / 'rust/ql/src').resolve()
        ):
            raise SystemExit('analysis suite resolved a query outside the verified source')
        text = path.read_text()
        found = re.findall(r'^\s*\*\s*@id\s+(\S+)', text, re.MULTILINE)
        if len(found) != 1:
            raise SystemExit('resolved query lacks a unique ID')
        ids.add(found[0])
    return ids
baseline = query_ids('bundled-queries.txt')
corrected = query_ids('source-queries.txt')
if not baseline <= corrected or 'rust/path-injection' not in corrected:
    raise SystemExit('corrected suite drops existing Rust query coverage')
resolution = json.loads((evidence / 'library-path.json').read_text())
def values(value):
    if isinstance(value, dict):
        for item in value.values():
            yield from values(item)
    elif isinstance(value, list):
        for item in value:
            yield from values(item)
    elif isinstance(value, str):
        yield value
if str(source / 'rust/ql/lib') not in values(resolution):
    raise SystemExit('corrected source library was not resolved')
(evidence / 'query-coverage.json').write_text(json.dumps({
    'bundled': sorted(baseline), 'corrected': sorted(corrected), 'missing': []}, indent=2) + '\n')
PY

# Use the toolchains pinned by this upstream revision, not a moving nightly.
bash "$CODEQL_SOURCE/rust/ql/test/setup.sh"
(
  cd "$CODEQL_SOURCE/rust/extractor/src/nightly-toolchain"
  rustc --version
) > "$CODEQL_EVIDENCE/rust-test-version.txt"
# Upstream expectations include request-to-path positives and configured-State negatives.
# Never regenerate expectations or upload these control databases as Kars findings.
"$CODEQL_BIN" test run --search-path="$CODEQL_SOURCE" \
  "$CODEQL_SOURCE/rust/ql/test/query-tests/security/CWE-022" \
  "$CODEQL_SOURCE/rust/ql/test/library-tests/dataflow/lambdas" \
  "$CODEQL_SOURCE/rust/ql/test/library-tests/dataflow/local" \
  "$CODEQL_SOURCE/rust/ql/test/library-tests/dataflow/models" \
  2>&1 | tee "$CODEQL_EVIDENCE/control-tests.log"

# Analyze with the source and library resolution that just passed the controls.
python3 - "$CODEQL_SOURCE" "${GITHUB_OUTPUT:?}" "${GITHUB_ENV:?}" <<'BINDING'
import json
from pathlib import Path
import sys

source, output, environment = map(Path, sys.argv[1:])
source = source.resolve(strict=True)
suite = source / 'rust/ql/src/codeql-suites/rust-security-and-quality.qls'
if not suite.is_file() or not (source / 'rust/ql/lib').is_dir():
    raise SystemExit('verified Rust suite or library is absent')
if any(char in str(source) for char in '\r\n'):
    raise SystemExit('invalid Rust source path')
options = {
    'database': {command: [f'--search-path={source}'] for command in (
        'init', 'run-queries', 'interpret-results')},
    'resolve': {'queries': [f'--search-path={source}']},
}
with environment.open('a') as handle:
    handle.write('CODEQL_ACTION_EXTRA_OPTIONS=' + json.dumps(options) + '\n')
with output.open('a') as handle:
    handle.write('suite=./.github/codeql-rust.qls\n')
BINDING
