#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -euo pipefail

cd "$(dirname "$0")/.."

# The ClawSandbox CR is declared in `azureclaw-claw`, but the controller
# materialises the actual sandbox Deployment in
# `azureclaw-realestate-agent` per the standard per-sandbox-namespace
# pattern. All `kubectl logs` / `kubectl exec` against the live pod
# target that namespace; `azureclaw audit verify` targets the CR
# namespace (`azureclaw-claw`).
SANDBOX_CR_NS="azureclaw-claw"
SANDBOX_POD_NS="azureclaw-realestate-agent"

echo "═══ Audit chain ($SANDBOX_CR_NS) ═══"
if command -v azureclaw >/dev/null 2>&1; then
  azureclaw audit verify --namespace "$SANDBOX_CR_NS" --agent realestate-agent || true
else
  echo "(azureclaw CLI not on PATH — falling back to raw logs)"
  kubectl logs -n "$SANDBOX_POD_NS" deploy/realestate-agent -c inference-router \
    | grep -E 'audit|deny|safety|budget|quarantin' | tail -20 || true
fi

echo
echo "═══ Naked claw audit chain ═══"
echo "(no audit pipeline — this is the point)"

echo
echo "═══ Router policy decisions ($SANDBOX_POD_NS) ═══"
kubectl logs -n "$SANDBOX_POD_NS" deploy/realestate-agent -c inference-router \
  | grep -E 'tool_policy|prompt_shields|token_budget|behavior' | tail -20 || true

echo
echo "═══ Egress-guard verification ═══"
echo "▸ trying direct curl from agent UID 1000 in $SANDBOX_POD_NS:"
kubectl exec -n "$SANDBOX_POD_NS" deploy/realestate-agent -c openclaw -- \
  sh -c 'curl -sS --max-time 5 https://api.openai.com/v1/files -o /dev/null -w "%{http_code}\n"' \
  2>&1 | tail -5 || echo "  ✅ refused as expected"

echo
echo "═══ Naked-claw exfil result ═══"
echo "▸ same curl from naked-claw:"
kubectl exec -n naked-claw deploy/realestate-agent -- \
  sh -c 'curl -sS --max-time 5 https://api.openai.com/v1/files -o /dev/null -w "%{http_code}\n"' \
  2>&1 | tail -5 || true
