#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -euo pipefail

: "${CODEQL_BIN:?CodeQL executable required}"
: "${CODEQL_SOURCE:?Isolated upstream source directory required}"
: "${CODEQL_EVIDENCE:?Evidence directory required}"
: "${CODEQL_SOURCE_SHA:?Immutable upstream commit required}"
language="${CODEQL_LANGUAGE:-rust}"
case "$language" in
  rust)
    required_query=rust/path-injection
    library_query=queries/security/CWE-022/TaintedPath.ql
    ;;
  actions)
    required_query=actions/cache-poisoning/poisonable-step
    library_query=Security/CWE-349/CachePoisoningViaPoisonableStep.ql
    ;;
  *) echo 'Unsupported scanner control language' >&2; exit 1 ;;
esac

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
"$CODEQL_BIN" resolve queries "codeql/$language-queries:codeql-suites/$language-security-and-quality.qls" \
  --format=text > "$CODEQL_EVIDENCE/bundled-queries.txt"
if [[ "$language" == rust ]]; then
  # Patch only the isolated source that controls and the real analysis both use.
  controls_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/codeql/bounded-value"
  python3 -B "$controls_root/apply.py" "$CODEQL_SOURCE" "$CODEQL_EVIDENCE" "$CODEQL_SOURCE_SHA"
fi
"$CODEQL_BIN" pack install "$CODEQL_SOURCE/$language/ql/src"
"$CODEQL_BIN" pack install "$CODEQL_SOURCE/$language/ql/test"
"$CODEQL_BIN" resolve queries "./.github/codeql-$language.qls" \
  --search-path="$CODEQL_SOURCE" --format=text > "$CODEQL_EVIDENCE/source-queries.txt"
"$CODEQL_BIN" resolve library-path \
  --query="$CODEQL_SOURCE/$language/ql/src/$library_query" \
  --search-path="$CODEQL_SOURCE" --format=json > "$CODEQL_EVIDENCE/library-path.json"
python3 - "$CODEQL_EVIDENCE" "$CODEQL_SOURCE" "$language" "$required_query" <<'PY'
import json
from pathlib import Path
import re
import sys

evidence, source = map(Path, sys.argv[1:3])
language, required_query = sys.argv[3:]
if language not in ('rust', 'actions'):
    raise SystemExit('unsupported scanner control language')
def query_ids(name):
    paths = (evidence / name).read_text().splitlines()
    if not paths:
        raise SystemExit('empty resolved query suite')
    ids = set()
    for path in paths:
        path = Path(path.strip())
        if name == 'source-queries.txt' and not path.resolve().is_relative_to(
            (source / language / 'ql/src').resolve()
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
if not baseline <= corrected or required_query not in corrected:
    raise SystemExit('corrected suite drops existing query coverage')
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
if str(source / language / 'ql/lib') not in values(resolution):
    raise SystemExit('corrected source library was not resolved')
(evidence / 'query-coverage.json').write_text(json.dumps({
    'bundled': sorted(baseline), 'corrected': sorted(corrected), 'missing': []}, indent=2) + '\n')
PY

# Never regenerate expectations or upload these control databases as Kars findings.
if [[ "$language" == rust ]]; then
  # Use the pinned upstream toolchains, not a moving nightly.
  bash "$CODEQL_SOURCE/rust/ql/test/setup.sh"
  (
    cd "$CODEQL_SOURCE/rust/extractor/src/nightly-toolchain"
    rustc --version
  ) > "$CODEQL_EVIDENCE/rust-test-version.txt"
  # Request-to-path positives and configured-State negatives remain mandatory.
  controls=(
    "$CODEQL_SOURCE/rust/ql/test/query-tests/security/CWE-022"
    "$CODEQL_SOURCE/rust/ql/test/library-tests/dataflow/lambdas"
    "$CODEQL_SOURCE/rust/ql/test/library-tests/dataflow/local"
    "$CODEQL_SOURCE/rust/ql/test/library-tests/dataflow/models"
    "$CODEQL_SOURCE/rust/ql/test/query-tests/security/CWE-502-bounded-value-controls"
    "$CODEQL_SOURCE/rust/ql/test/query-tests/security/CWE-502"
    "$CODEQL_SOURCE/rust/ql/test/query-tests/unusedentities"
  )
else
  # Includes writable-cache positives, read-only negatives and reusable caller limits.
  controls=("$CODEQL_SOURCE/actions/ql/test/query-tests/Security/CWE-349")
fi
"$CODEQL_BIN" test run --search-path="$CODEQL_SOURCE" "${controls[@]}" \
  2>&1 | tee "$CODEQL_EVIDENCE/control-tests.log"

# Analyze with the source and library resolution that just passed the controls.
python3 - "$CODEQL_SOURCE" "${GITHUB_OUTPUT:?}" "${GITHUB_ENV:?}" "$language" <<'BINDING'
import json
from pathlib import Path
import sys

source, output, environment = map(Path, sys.argv[1:4])
language = sys.argv[4]
if language not in ('rust', 'actions'):
    raise SystemExit('unsupported scanner control language')
source = source.resolve(strict=True)
suite = source / language / 'ql/src/codeql-suites' / f'{language}-security-and-quality.qls'
if not suite.is_file() or not (source / language / 'ql/lib').is_dir():
    raise SystemExit('verified suite or library is absent')
if any(char in str(source) for char in '\r\n'):
    raise SystemExit('invalid scanner source path')
options = {
    'database': {command: [f'--search-path={source}'] for command in (
        'init', 'run-queries', 'interpret-results')},
    'resolve': {'queries': [f'--search-path={source}']},
}
with environment.open('a') as handle:
    handle.write('CODEQL_ACTION_EXTRA_OPTIONS=' + json.dumps(options) + '\n')
with output.open('a') as handle:
    handle.write(f'suite=./.github/codeql-{language}.qls\n')
BINDING
