#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# tools/demo/act2/reset.sh — undo the Act II break.
#
# Removes the platform-hardening ResourceQuota and waits for the
# agent pod to come back Running 2/2. This is what the kars-sre
# agent's typed `DeleteResourceQuota` action does in the demo; the
# script exists so the presenter can recover the cluster manually
# (during rehearsal, or after a failed Act II run).

set -euo pipefail

NS="kars-research"
SANDBOX="research"

echo "▸ deleting platform-hardening ResourceQuota..."
kubectl -n "${NS}" delete resourcequota platform-hardening-quota --ignore-not-found

echo ""
echo "▸ waiting for the agent pod to come back Running (up to 120s)..."
kubectl -n "${NS}" rollout status "deploy/${SANDBOX}" --timeout=120s

echo ""
echo "✓ ${SANDBOX} is healthy"
kubectl -n "${NS}" get pod
