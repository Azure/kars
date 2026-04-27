#!/usr/bin/env bash
# ci/vendored-patch-audit.sh — enforces internal Phase 1 plan §0.2 #8, #9.
#
# If the PR changes vendor/** or bumps the AGT SDK pin (Cargo.toml /
# package.json), require a new "Re-audit history" row in
# docs/agt-vendored-patch-audit.md — dated today, signed by a reviewer.
set -euo pipefail

BASE_REF="${BASE_REF:-origin/main}"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

AUDIT_DOC='docs/agt-vendored-patch-audit.md'

changed=$(git diff --name-only "${BASE_REF}...HEAD" 2>/dev/null || git diff --name-only HEAD)

touched_vendor=$(printf '%s\n' "$changed" | grep -E '^vendor/' || true)
touched_pin=$(printf '%s\n' "$changed" | grep -E '(Cargo\.toml|Cargo\.lock|package\.json|package-lock\.json)$' || true)

if [ -z "$touched_vendor" ] && [ -z "$touched_pin" ]; then
  exit 0
fi

# If only pin files are touched, check if an AGT / agentmesh dep changed.
if [ -z "$touched_vendor" ] && [ -n "$touched_pin" ]; then
  if ! git diff "${BASE_REF}...HEAD" -- $touched_pin 2>/dev/null \
      | grep -E '^\+.*(agt-sdk|agentmesh|@agentmesh/sdk)' >/dev/null; then
    exit 0
  fi
fi

# Require the audit doc itself to appear in the diff with a new row.
if ! printf '%s\n' "$changed" | grep -qx "$AUDIT_DOC"; then
  echo "fail: vendor/** or AGT SDK pin changed but $AUDIT_DOC not updated." >&2
  echo "      Add a new 'Re-audit history' row per internal Phase 1 plan §0.2 principle 8." >&2
  exit 1
fi

# Ensure the diff adds a row dated today (YYYY-MM-DD).
today=$(date -u +%Y-%m-%d)
if ! git diff "${BASE_REF}...HEAD" -- "$AUDIT_DOC" 2>/dev/null | grep -q "^\+| $today"; then
  echo "fail: $AUDIT_DOC must gain a new Re-audit history row dated $today." >&2
  exit 1
fi

exit 0
