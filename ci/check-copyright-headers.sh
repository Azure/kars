#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
# ci/check-copyright-headers.sh — enforces OSPO finding CODE-COPYRIGHT-HDRS.
#
# Every AzureClaw-authored source file (.rs, .ts, .tsx, .js, .sh) must carry
# the two-line Microsoft + MIT copyright header at the top of the file.
#
# Excludes:
#   - vendor/          (upstream code; own licenses in THIRD_PARTY_NOTICES.txt)
#   - node_modules/    (installed deps)
#   - dist/            (compiled output)
#   - build/           (compiled output)
#   - target/          (Rust build artifacts)
#   - .turbo/          (turbo cache)
#   - coverage/        (test coverage reports)
#   - *.d.ts           (generated TypeScript declarations)
#
# Exit codes:
#   0 — all files have the header
#   1 — one or more files are missing the header (list printed to stderr)
set -euo pipefail

MISSING=()

while IFS= read -r file; do
  # Check first 5 lines for the copyright marker
  if ! head -5 "$file" | grep -qE '^(//|#) *Copyright \(c\) Microsoft Corporation'; then
    MISSING+=("$file")
  fi
done < <(
  git ls-files \
    | grep -E '\.(rs|ts|tsx|js|sh)$' \
    | grep -v '^vendor/' \
    | grep -v 'node_modules/' \
    | grep -v '/dist/' \
    | grep -v '^target/' \
    | grep -v '/build/' \
    | grep -v '\.d\.ts$' \
    | grep -v '\.turbo/' \
    | grep -v '/coverage/'
)

if [ "${#MISSING[@]}" -gt 0 ]; then
  echo "❌ Missing Microsoft + MIT copyright header in ${#MISSING[@]} file(s):" >&2
  for f in "${MISSING[@]}"; do
    echo "  $f" >&2
  done
  echo "" >&2
  echo "Every AzureClaw-authored source file must begin with:" >&2
  echo "  // Copyright (c) Microsoft Corporation."  >&2
  echo "  // Licensed under the MIT License."        >&2
  echo "(or # … for shell/Python files)" >&2
  exit 1
fi

echo "✅ All $(git ls-files | grep -E '\.(rs|ts|tsx|js|sh)$' | grep -v '^vendor/' | grep -v 'node_modules/' | grep -v '/dist/' | grep -v '^target/' | grep -v '/build/' | grep -v '\.d\.ts$' | grep -v '\.turbo/' | grep -v '/coverage/' | wc -l | tr -d ' ') source files carry the Microsoft + MIT copyright header."
