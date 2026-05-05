#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "═══ Audit chain (azureclaw-claw) ═══"
if command -v azureclaw >/dev/null 2>&1; then
  azureclaw audit verify --namespace azureclaw-claw --agent realestate-agent || true
else
  echo "(azureclaw CLI not on PATH — falling back to raw logs)"
  kubectl logs -n azureclaw-claw deploy/realestate-agent -c inference-router \
    | grep -E 'audit|deny|safety|budget|quarantin' | tail -20 || true
fi

echo
echo "═══ Naked claw audit chain ═══"
echo "(no audit pipeline — this is the point)"

echo
echo "═══ Router policy decisions (azureclaw-claw) ═══"
kubectl logs -n azureclaw-claw deploy/realestate-agent -c inference-router \
  | grep -E 'tool_policy|prompt_shields|token_budget|behavior' | tail -20 || true

echo
echo "═══ Egress-guard verification ═══"
echo "▸ trying direct curl from agent UID 1000 in azureclaw-claw:"
kubectl exec -n azureclaw-claw deploy/realestate-agent -c openclaw -- \
  sh -c 'curl -sS --max-time 5 https://api.openai.com/v1/files -o /dev/null -w "%{http_code}\n"' \
  2>&1 | tail -5 || echo "  ✅ refused as expected"

echo
echo "═══ Naked-claw exfil result ═══"
echo "▸ same curl from naked-claw:"
kubectl exec -n naked-claw deploy/realestate-agent -- \
  sh -c 'curl -sS --max-time 5 https://api.openai.com/v1/files -o /dev/null -w "%{http_code}\n"' \
  2>&1 | tail -5 || true
