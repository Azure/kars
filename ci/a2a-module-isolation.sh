#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# ci/a2a-module-isolation.sh — enforces ADR-0001 D4.
#
# The A2A inbound code path (inference-router/src/a2a/ and any future
# inference-router/src/routes/a2a/) MUST NOT import the concrete
# credential-bearing types defined in crate::auth. This raises the
# bar against memory-disclosure exploits in the JWS / JSON-RPC parser
# from "find a UAF" to "find a UAF AND heap-scan for IMDS bytes
# without any type information about where they live."
#
# Allowed: trait-mediated calls (PolicyDecisionProvider, SigningProvider,
# AuditSink). Forbidden: direct imports of ImdsToken, FoundryCredentials,
# WorkloadIdentityToken, or anything matching `auth::*Credential*` /
# `auth::*Token*`.
#
# Scope: any *.rs file under inference-router/src/a2a/ or
# inference-router/src/routes/a2a/.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

A2A_PATHS=(
  'inference-router/src/a2a'
  'inference-router/src/routes/a2a'
)

# Forbidden import patterns.
# We deliberately exclude the trait-only re-exports if any are added later
# — they would import via crate::providers::*, not crate::auth::*.
FORBID_RE='^[[:space:]]*use[[:space:]]+(crate::|super::|self::)?auth::(.*(Credential|Token|ImdsToken|FoundryCredentials|WorkloadIdentityToken).*|\*)'

fail=0
for dir in "${A2A_PATHS[@]}"; do
  [ -d "$dir" ] || continue
  while IFS= read -r -d '' f; do
    hits=$(grep -nE "$FORBID_RE" "$f" || true)
    if [ -n "$hits" ]; then
      echo "fail: $f imports a forbidden auth type (ADR-0001 D4)." >&2
      printf '  %s\n' "$hits" >&2
      fail=1
    fi
    # Also forbid `crate::auth::*` glob at any other position.
    if grep -nE 'crate::auth::\*' "$f" >/dev/null; then
      echo "fail: $f wildcards crate::auth (ADR-0001 D4)." >&2
      fail=1
    fi
  done < <(find "$dir" -type f -name '*.rs' -print0)
done

exit $fail
