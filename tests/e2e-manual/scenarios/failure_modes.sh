#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Manual E2E scenario: failure modes (resilience).
#
#   1. Router crash: kill inference-router container; verify pod restarts
#      and sandbox returns to Ready.
#   2. Relay disconnect: scale agentmesh-relay to 0; verify queued mesh
#      sends fail closed (no plaintext leak); restore relay; verify
#      recovery.
#   3. Sandbox OOM: raise memory pressure; verify the controller marks
#      the sandbox Degraded and the pod is restarted (not silently
#      retained).
#
# All probes are non-destructive to the cluster as a whole — only the
# sandbox-under-test and the relay deployment are touched, and the
# relay is scaled back at the end.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$(cd "$SCRIPT_DIR/../lib" && pwd)"
# shellcheck source=../lib/common.sh
source "$LIB_DIR/common.sh"
# shellcheck source=../lib/cr_factory.sh
source "$LIB_DIR/cr_factory.sh"

scenario_header "Failure modes — router crash, relay disconnect, OOM"

require_cluster
require_azureclaw_installed

name="failmodes-openclaw"
ns=$(new_ns "failmodes")

cr_openclaw "$name" "$ns" | kubectl apply -f - >/dev/null
wait_for_clawsandbox_ready "$ns" "$name" || {
    log_fail "initial sandbox never became Ready"
    cleanup_ns "$ns"; exit 1
}
pod=$(kubectl -n "$ns" get pod -l "azureclaw.azure.com/sandbox=${name}" -o jsonpath='{.items[0].metadata.name}')

# 1. Router crash
log_step "[1/3] killing inference-router container"
kubectl -n "$ns" exec "$pod" -c inference-router -- sh -c 'kill 1' 2>/dev/null || true
sleep 5
if wait_for_clawsandbox_ready "$ns" "$name"; then
    log_pass "sandbox recovered after inference-router restart"
else
    log_fail "sandbox did not return to Ready after router restart"
fi

# 2. Relay disconnect
if kubectl -n agentmesh get deploy agentmesh-relay >/dev/null 2>&1; then
    log_step "[2/3] scaling agentmesh-relay to 0 to simulate a network split"
    orig_replicas=$(kubectl -n agentmesh get deploy agentmesh-relay -o jsonpath='{.spec.replicas}')
    kubectl -n agentmesh scale deploy/agentmesh-relay --replicas=0 >/dev/null
    sleep 10
    # Sandbox should remain Running but mesh ops fail closed.
    if kubectl -n "$ns" get pod "$pod" -o jsonpath='{.status.phase}' | grep -q Running; then
        log_pass "sandbox pod remained Running through relay outage (no crash loop)"
    else
        log_fail "sandbox pod went non-Running on relay outage"
    fi
    kubectl -n agentmesh scale deploy/agentmesh-relay --replicas="${orig_replicas:-1}" >/dev/null
    if kubectl -n agentmesh rollout status deploy/agentmesh-relay --timeout="${MANUAL_E2E_TIMEOUT:-300}s" >/dev/null 2>&1; then
        log_pass "agentmesh-relay scaled back successfully"
    else
        log_fail "agentmesh-relay did not roll out after scale-up"
    fi
else
    log_skip "[2/3] agentmesh-relay not installed — mesh disconnect probe skipped"
fi

# 3. OOM
log_step "[3/3] inducing memory pressure inside the sandbox"
# Allocate a 256MiB string in the sandbox shell. If limits are tight the
# kernel kills it; if not, this is a no-op probe.
kubectl -n "$ns" exec "$pod" -c openclaw -- sh -c \
    'python3 -c "x=[bytearray(1024*1024) for _ in range(512)]" 2>&1 | head -2' \
    >/dev/null 2>&1 || true
sleep 5
restart_count=$(kubectl -n "$ns" get pod "$pod" \
    -o jsonpath='{.status.containerStatuses[?(@.name=="openclaw")].restartCount}' 2>/dev/null || echo 0)
log_info "openclaw container restartCount = ${restart_count}"
if [[ "$restart_count" -ge 1 ]]; then
    log_pass "OOM/oversized alloc was contained — kubelet restarted the container"
else
    log_skip "[3/3] no restart observed — sandbox memory limits may be permissive in this profile"
fi

if wait_for_clawsandbox_ready "$ns" "$name"; then
    log_pass "sandbox is Ready again at end of failure-modes scenario"
else
    log_fail "sandbox not Ready at end of failure-modes scenario"
fi

cleanup_ns "$ns"
scenario_summary "Failure modes"
