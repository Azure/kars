#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# ci/security-audit-required.sh — enforces internal Phase 1 plan §0.2 #9.
#
# If the PR touches a capability-introducing file, require a matching
# docs/security-audits/<YYYY-MM-DD>-<slug>.md with two distinct sign-off
# blocks (two lines matching /^Signed-off-by: .+<.+@.+>/ on different emails).
#
# Note: audit docs live in the TRACKED `docs/security-audits/` folder (not the
# gitignored `docs/internal/`), so they are committed normally as part of the PR.
set -euo pipefail

BASE_REF="${BASE_REF:-origin/main}"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Capability-introducing paths — mirrors §4.4 of the plan.
CAP_RE='^(controller/src/(crd|reconcilers|admission)|inference-router/src/(mcp|a2a|providers|routes)|cli/src/(commands|migrate|adapters)|runtimes/openclaw/src/(core|index\.ts)|sandbox-images/[^/]+/(Dockerfile|entrypoint\.sh)|cli/profiles/|deploy/seccomp/|deploy/helm/kars/files/|shared/.*\.rs$|bridge/(bff/src/|web/src/|teams-gateway/src/|deploy/|[^/]+/Dockerfile|start-bff\.sh))'

if ! changed=$(git diff --no-ext-diff --name-only "${BASE_REF}...HEAD"); then
  echo "fail: cannot determine the reviewed capability diff." >&2
  exit 1
fi
# Exclude test files — they exercise capabilities but don't introduce
# them. Catches *.test.ts / *.test.js / *_test.rs / tests/ directories.
touches_cap=$(printf '%s\n' "$changed" \
  | grep -E "$CAP_RE" \
  | grep -Ev '(\.test\.(ts|js|tsx|jsx)|_test\.rs|/tests?/|/__tests__/)' \
  || true)
if [ -z "$touches_cap" ]; then
  exit 0
fi

# Modifying or renaming an older approval cannot extend its recorded scope.
if ! additions=$(git diff --no-ext-diff --name-only --find-renames=50% --diff-filter=A "${BASE_REF}...HEAD"); then
  echo "fail: cannot determine newly added audit records." >&2
  exit 1
fi
added_audit=$(printf '%s\n' "$additions" | grep -E '^docs/security-audits/[0-9]{4}-[0-9]{2}-[0-9]{2}-.+\.md$' || true)
if [ -z "$added_audit" ]; then
  echo "fail: capability-introducing files touched but no new docs/security-audits/YYYY-MM-DD-<slug>.md added." >&2
  echo "      touched capabilities:" >&2
  printf '        %s\n' $touches_cap >&2
  echo "      Copy docs/security-audits/_template.md and fill it in (see docs/security-audits/README.md)." >&2
  exit 1
fi

# Explicit source-bound maintainer decisions, not environment-configurable waivers.
beta_approval_exception() {
  local audit="$1" approval_text approved_head approved_base approved_pr
  case "$audit" in
    docs/security-audits/2026-11-04-beta-publication.md)
      approved_head=28ff64372ca97e6bdd6db833d68afa90239cde7c
      approved_base=b2ec2094cc663d5b747fec7f3c9d37cce667a24a
      approved_pr=593 ;;
    docs/security-audits/2026-11-10-overlay-package-update.md)
      approved_head=187ada4b45203bc831e7976961ffcb2f92160886
      approved_base=ab78d11d66b626b4a176603643011ebc1cca1dff
      approved_pr=606 ;;
    docs/security-audits/2026-11-10-kind-local-onboarding.md)
      approved_head=a4333b355a03648cf3769faf4eca26e988700aeb
      approved_base=97789571866ca2c181f84455a845b957427cd991
      approved_pr=607 ;;
    docs/security-audits/2026-11-11-headlamp-go-release-repair.md)
      approved_head=11edc2dc6fde46d8bbf8319ebc15ce2e2c0df18e
      approved_base=2068be5faab76afeb1fa51afb7ebd20fd8177530
      approved_pr=608 ;;
    *) return 1 ;;
  esac
  [[ "$(git rev-parse --verify "${BASE_REF}^{commit}")" == "$approved_base" ]] || return 1
  git merge-base --is-ancestor "$approved_base" "$approved_head" || return 1
  git merge-base --is-ancestor "$approved_head" HEAD || return 1
  # The approval commit may change only this record and its policy/test plumbing.
  git diff --no-ext-diff --quiet "$approved_head" HEAD -- . \
    ':(exclude)ci/security-audit-required.sh' \
    ':(exclude)ci/tests/security_audit_gate_test.py' \
    ':(exclude)docs/security-audits/README.md' \
    ":(exclude)$audit" || return 1
  if [[ "${GITHUB_ACTIONS:-}" == true ]]; then
    [[ "${GITHUB_REPOSITORY:-}" == Azure/kars ]] || return 1
    case "${GITHUB_EVENT_NAME:-}:${GITHUB_REF:-}" in
      "pull_request:refs/pull/$approved_pr/merge"|push:refs/heads/kars-bridge) ;;
      *) return 1 ;;
    esac
  fi
  approval_text=$(git show "HEAD:$audit") || return 1
  grep -Fxq "Beta-approval-exception: Azure/kars#$approved_pr; source=$approved_head; base=$approved_base" \
    <<< "$approval_text" || return 1
  grep -Fxq 'Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>' <<< "$approval_text"
}

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
    if beta_approval_exception "$audit"; then
      echo "pass: $audit uses its explicit, source-bound single-maintainer beta exception."
    else
      echo "fail: $audit has ${#signers[@]} distinct Signed-off-by emails; need 2 (author + independent reviewer)." >&2
      fail=1
    fi
  fi
done <<< "$added_audit"

exit $fail
