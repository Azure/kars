#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# tools/demo/act2/break.sh — induce the Act II infrastructure incident.
#
# Scenario (per docs/blueprints/07-kars-sre-proposal.md §7.2 +
# tools/demo/act2/platform-hardening-quota.yaml header):
#
#   The "platform hardening" GitOps refactor lands a tight
#   ResourceQuota in the kars-research namespace. The quota's
#   requests.memory ceiling (50Mi) is lower than the agent pod
#   actually requests. The running pod keeps running, but the moment
#   anything triggers a fresh pod (rollout, eviction, restart) the
#   new pod cannot be admitted to the namespace.
#
# This script:
#   1. Applies the ResourceQuota (the operator's "mistake")
#   2. Force-deletes the running research pod (surfaces the failure
#      immediately rather than waiting for natural restart)
#   3. Confirms the new pod is stuck Pending with the expected
#      quota-violation reason on the ReplicaSet
#
# Idempotent: re-running is safe; the quota is `kubectl apply`-ed.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NS="kars-research"
SANDBOX="research"

echo "▸ verifying agent-a is running (must be present before we break it)..."
if ! kubectl -n "${NS}" get deploy "${SANDBOX}" >/dev/null 2>&1; then
  echo "✗ deploy/${SANDBOX} not found in ns ${NS}." >&2
  echo "  Apply tools/demo/act2/agent-a-research.yaml first and wait for Running 2/2." >&2
  exit 1
fi
kubectl -n "${NS}" rollout status "deploy/${SANDBOX}" --timeout=60s

echo ""
echo "▸ applying platform-hardening ResourceQuota..."
kubectl apply -f "${SCRIPT_DIR}/platform-hardening-quota.yaml"

echo ""
echo "▸ force-deleting the running pod to surface the failure..."
POD=$(kubectl -n "${NS}" get pod -l kars.azure.com/component=sandbox \
  -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
if [[ -z "${POD}" ]]; then
  echo "⚠ no sandbox pod found to evict; quota will only manifest on next natural restart" >&2
else
  kubectl -n "${NS}" delete pod "${POD}" --grace-period=1
fi

echo ""
echo "▸ waiting for the failure to surface in the ReplicaSet events (up to 60s)..."
for i in $(seq 1 60); do
  # Look for the quota-violation event on any ReplicaSet in the ns
  REASON=$(kubectl -n "${NS}" get events \
    --field-selector reason=FailedCreate \
    -o jsonpath='{.items[*].message}' 2>/dev/null || echo "")
  if echo "${REASON}" | grep -qE "exceeded quota|forbidden.*quota"; then
    echo "✓ quota violation observed after ${i}s"
    echo ""
    echo "─── current state ─────────────────────────────────────"
    kubectl -n "${NS}" get pod
    echo ""
    echo "─── ResourceQuota in ${NS} ────────────────────────────"
    kubectl -n "${NS}" get resourcequota
    echo ""
    echo "─── most-recent FailedCreate events ──────────────────"
    kubectl -n "${NS}" get events --field-selector reason=FailedCreate --sort-by=.lastTimestamp | tail -3
    echo "───────────────────────────────────────────────────────"
    echo ""
    echo "✓ Act II incident induced. kars-sre agent's turn."
    exit 0
  fi
  sleep 1
done

echo "⚠ timeout: quota-violation event did not appear within 60s" >&2
kubectl -n "${NS}" get pod >&2 || true
kubectl -n "${NS}" get events --field-selector reason=FailedCreate >&2 || true
exit 1
