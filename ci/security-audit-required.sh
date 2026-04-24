#!/usr/bin/env bash
# ci/security-audit-required.sh — enforces docs/implementation-plan.md §0.2 #9.
#
# If the PR touches a capability-introducing file, require a matching
# docs/security-audits/<YYYY-MM-DD>-<slug>.md with two distinct sign-off
# blocks (two lines matching /^Signed-off-by: .+<.+@.+>/ on different emails).
set -euo pipefail

BASE_REF="${BASE_REF:-origin/main}"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Capability-introducing paths — mirrors §4.4 of the plan.
CAP_RE='^(controller/src/(crd|reconcilers|admission)|inference-router/src/(mcp|a2a|providers|routes)|cli/src/(commands|migrate|adapters)|sandbox-images/[^/]+/(Dockerfile|entrypoint\.sh)|policy-engine/)'

changed=$(git diff --name-only "${BASE_REF}...HEAD" 2>/dev/null || git diff --name-only HEAD)
touches_cap=$(printf '%s\n' "$changed" | grep -E "$CAP_RE" || true)
if [ -z "$touches_cap" ]; then
  exit 0
fi

# Is at least one docs/security-audits/*.md added in this PR?
added_audit=$(printf '%s\n' "$changed" | grep -E '^docs/security-audits/[0-9]{4}-[0-9]{2}-[0-9]{2}-.+\.md$' || true)
if [ -z "$added_audit" ]; then
  echo "fail: capability-introducing files touched but no docs/security-audits/YYYY-MM-DD-<slug>.md added." >&2
  echo "      touched capabilities:" >&2
  printf '        %s\n' $touches_cap >&2
  echo "      Copy docs/security-audits/_template.md and fill it in (see implementation-plan §5.5)." >&2
  exit 1
fi

fail=0
while IFS= read -r audit; do
  [ -z "$audit" ] && continue
  [ -f "$audit" ] || continue
  # Two distinct Signed-off-by lines required.
  mapfile -t signers < <(
    grep -E '^Signed-off-by: .+<[^>]+@[^>]+>' "$audit" \
      | sed -E 's/.*<([^>]+)>.*/\1/' | sort -u
  )
  if [ "${#signers[@]}" -lt 2 ]; then
    echo "fail: $audit has ${#signers[@]} distinct Signed-off-by emails; need 2 (author + independent reviewer)." >&2
    fail=1
  fi
done <<< "$added_audit"

exit $fail
