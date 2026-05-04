#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# ci/no-null-provider-prod.sh — enforces internal Phase 1 plan §0.2 #9.
#
# Static mirror of the ValidatingAdmissionPolicy shipped in Phase 0:
#   A YAML manifest with spec.*.provider: null|noop|disabled must carry
#   metadata.labels."azureclaw.azure.com/dev-only": "true".
#
# Scope: controller Helm chart + CLI templates + docs/examples.
set -euo pipefail

BASE_REF="${BASE_REF:-origin/main}"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

SCAN_PATHS=(
  'deploy/helm/'
  'cli/templates/'
  'cli/src/commands/'
  'docs/'
  'tests/compat/fixtures/'
)

# Detect suspect manifests.
suspect_files=$(
  for root in "${SCAN_PATHS[@]}"; do
    [ -d "$root" ] || continue
    grep -l -R -E 'provider:[[:space:]]*(null|noop|disabled)' "$root" 2>/dev/null || true
  done | sort -u | grep -v '^docs/internal/security-audits/' || true
)
[ -z "$suspect_files" ] && exit 0

fail=0
while IFS= read -r f; do
  [ -z "$f" ] && continue
  [ -f "$f" ] || continue

  # Check for the dev-only label within the same file.
  if ! grep -q 'azureclaw\.azure\.com/dev-only:[[:space:]]*"\?true"\?' "$f"; then
    echo "fail: $f uses a null/noop/disabled provider without 'azureclaw.azure.com/dev-only: \"true\"' label." >&2
    fail=1
  fi
done <<< "$suspect_files"

exit $fail
