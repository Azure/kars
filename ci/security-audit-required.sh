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
CAP_RE='^(controller/src/(crd|reconcilers|admission)|inference-router/src/(mcp|a2a|providers|routes)|cli/src/(commands|migrate|adapters)|runtimes/openclaw/src/(core|index\.ts)|(mesh-plugin|runtimes/openclaw)/package-lock\.json$|sandbox-images/[^/]+/(Dockerfile|entrypoint\.sh)|cli/profiles/|deploy/seccomp/|deploy/helm/kars/files/|shared/.*\.rs$|bridge/(bff/src/|web/src/|teams-gateway/src/|deploy/|[^/]+/Dockerfile|start-bff\.sh))'

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
# Modifying or renaming an older approval cannot extend its recorded scope.
if ! additions=$(git diff --no-ext-diff --name-only --find-renames=50% --diff-filter=A "${BASE_REF}...HEAD"); then
  echo "fail: cannot determine newly added audit records." >&2
  exit 1
fi
added_audit=$(printf '%s\n' "$additions" | grep -E '^docs/security-audits/[0-9]{4}-[0-9]{2}-[0-9]{2}-.+\.md$' || true)
# PR621 is workflow/test-only; its newly added voluntary approval must still
# exercise the source-bound checks. Historical record edits do not reopen it.
if [ -z "$touches_cap" ] && ! grep -Fxq 'docs/security-audits/2026-11-12-playwright-native-release.md' <<< "$added_audit"; then
  exit 0
fi
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
      approved_head=bec8d459b360ee6c9d5416b313df8a356195bbd7
      approved_base=2068be5faab76afeb1fa51afb7ebd20fd8177530
      approved_pr=608 ;;
    docs/security-audits/2026-11-12-openclaw-sdk-compare.md)
      approved_head=234b63e60f231f1c80286efaf9d2d44c91854f79
      approved_base=e5db80c4e761cb5a92eb097b98713ba43d3d9394
      approved_pr=617 ;;
    docs/security-audits/2026-11-12-playwright-native-release.md)
      approved_head=efadcd4932c08844a94e62080e45b6256c29a58a
      approved_base=01548f2c365a94ca2016987f03a496a237ea46ff
      approved_pr=621 ;;
    docs/security-audits/2026-11-12-openclaw-npm-registry.md)
      approved_head=28ca292c01bbd4310af90563fc2496281fce7141
      approved_base=af5ff64796898060f8f9d4a9dc853013f1d51981
      approved_pr=622 ;;
    docs/security-audits/2026-11-12-headlamp-evidence-permissions.md)
      approved_head=15281ad70925895f3f7c646b6503a2b1f6bd90d1
      approved_base=af5ff64796898060f8f9d4a9dc853013f1d51981
      approved_pr=623 ;;
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

# PR623 retains PR622's record, still newly added against their common base.
# Only the validated successor scope can carry it; that scope's source diff
# includes the historical record, so modifying it invalidates the successor.
carried_beta_approval() {
  local audit="$1" successor='docs/security-audits/2026-11-12-headlamp-evidence-permissions.md'
  [[ "$audit" == 'docs/security-audits/2026-11-12-openclaw-npm-registry.md' ]] || return 1
  grep -Fxq "$successor" <<< "$added_audit" || return 1
  beta_approval_exception "$successor"
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
    elif carried_beta_approval "$audit"; then
      echo "pass: $audit is unchanged within the explicit, source-bound PR623 successor approval."
    else
      echo "fail: $audit has ${#signers[@]} distinct Signed-off-by emails; need 2 (author + independent reviewer)." >&2
      fail=1
    fi
  fi
done <<< "$added_audit"

exit $fail
