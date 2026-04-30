#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# ci/no-stubs.sh — enforces internal Phase 1 plan §0.2 principle #8.
#
# Rejects *newly added* stub/placeholder markers on production code paths.
# Existing matches are baselined (see ci/no-stubs.allowlist); this script
# only fails on additions introduced by the current PR.
#
# Override: append `// ci:stub-ok: <reason>` (or `# ci:stub-ok: <reason>`)
# on the same line. Reviewer must sign off in the security-audit doc.
#
# Scope: production code only.
set -euo pipefail

BASE_REF="${BASE_REF:-origin/main}"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

PROD_PATHS=(
  'controller/src/'
  'inference-router/src/'
  'cli/src/'
  'runtimes/openclaw/src/'
  'sandbox-images/'
  'cli/profiles/'
)

# Patterns that indicate an unfinished production code path.
# Tuned to avoid false positives on legitimate identifiers (e.g., function
# names containing "mock"): each pattern targets the canonical shape.
PATTERNS='TODO\b|FIXME\b|XXX\b|HACK\b|unimplemented!\(|\btodo!\(|panic!\("not[ _-]impl|\bplaceholder\b|\.stub\(\)|\.mock\(\)|return None; // placeholder|return Ok\(\(\)\); // stub'

allow_exists=0
if [ -f ci/no-stubs.allowlist ]; then allow_exists=1; fi

# Scope the diff to prod paths.
mapfile -t changed < <(
  git diff --name-only "${BASE_REF}...HEAD" 2>/dev/null || git diff --name-only HEAD
)

fail=0
for f in "${changed[@]}"; do
  [ -z "$f" ] && continue
  match=0
  for prefix in "${PROD_PATHS[@]}"; do
    case "$f" in "$prefix"*) match=1; break;; esac
  done
  [ "$match" -eq 0 ] && continue
  # skip tests subdirs
  case "$f" in
    */tests/*|*/test/*|*.test.ts|*.spec.ts|*_test.rs|*/tests.rs) continue;;
  esac
  [ -f "$f" ] || continue

  # For each ADDED line in the diff, check pattern.
  while IFS= read -r line; do
    stripped="${line#+}"
    # Override-aware
    if printf '%s' "$stripped" | grep -qE 'ci:stub-ok:'; then
      continue
    fi
    if printf '%s' "$stripped" | grep -qE "$PATTERNS"; then
      echo "fail: $f: new stub/placeholder introduced: ${stripped:0:160}" >&2
      fail=1
    fi
  done < <(git diff "${BASE_REF}...HEAD" -- "$f" 2>/dev/null | grep -E '^\+[^+]')
done

exit $fail
