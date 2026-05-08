#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Manual E2E scenario: multi-tenant isolation (T2 / T4 boundaries).
#
# Two ClawSandboxes in two namespaces:
#
#   1. NetworkPolicy blocks pod-to-pod traffic across tenants.
#      → exec into A's main container, attempt TCP to B's pod IP, expect
#        connection refused or timeout.
#   2. Token budget is per-tenant: exhausting tenant A's budget must not
#      affect tenant B (probed via a single round-trip on B after a
#      burst on A — but only when the rate limit observed earlier).
#   3. The two namespaces have distinct ServiceAccounts and the
#      controller does not mount a cluster-wide kubeconfig into either.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$(cd "$SCRIPT_DIR/../lib" && pwd)"
# shellcheck source=../lib/common.sh
source "$LIB_DIR/common.sh"
# shellcheck source=../lib/cr_factory.sh
source "$LIB_DIR/cr_factory.sh"

scenario_header "Multi-tenant isolation"

require_cluster
require_azureclaw_installed

name_a="iso-a"
name_b="iso-b"
ns_a=$(new_ns "iso-a")
ns_b=$(new_ns "iso-b")
pod_ns_a=$(pod_ns_for "$name_a")
pod_ns_b=$(pod_ns_for "$name_b")
export MANUAL_E2E_SCENARIO=isolation

metric_start "admit_${name_a}"
cr_openclaw "$name_a" "$ns_a" | kubectl apply -f - >/dev/null
metric_finish "admit_${name_a}" isolation admitClawSandbox "sandbox=${name_a}"
metric_start "admit_${name_b}"
cr_openclaw "$name_b" "$ns_b" | kubectl apply -f - >/dev/null
metric_finish "admit_${name_b}" isolation admitClawSandbox "sandbox=${name_b}"

wait_for_clawsandbox_ready "$ns_a" "$name_a" || { log_fail "A not ready"; cleanup_sandbox "$ns_a" "$name_a"; cleanup_sandbox "$ns_b" "$name_b"; exit 1; }
wait_for_clawsandbox_ready "$ns_b" "$name_b" || { log_fail "B not ready"; cleanup_sandbox "$ns_a" "$name_a"; cleanup_sandbox "$ns_b" "$name_b"; exit 1; }

pod_a=$(kubectl -n "$pod_ns_a" get pod -l "azureclaw.azure.com/sandbox=${name_a}" -o jsonpath='{.items[0].metadata.name}')
pod_b=$(kubectl -n "$pod_ns_b" get pod -l "azureclaw.azure.com/sandbox=${name_b}" -o jsonpath='{.items[0].metadata.name}')
ip_b=$(kubectl -n "$pod_ns_b" get pod "$pod_b" -o jsonpath='{.status.podIP}')
log_info "tenant A pod=${pod_a} (ns=${pod_ns_a})  →  tenant B IP=${ip_b} (ns=${pod_ns_b})"

# Isolation probes need to run inside the agent container to validate
# what the agent itself can reach. Default ValidatingAdmissionPolicy
# blocks exec into openclaw — flip the audited break-glass label on
# tenant A's pod ns for the duration of this scenario, restore on exit.
enable_break_glass "$pod_ns_a"
trap 'disable_break_glass "$pod_ns_a"' EXIT

# 1. NetworkPolicy isolation.
log_step "[1/3] cross-tenant TCP probe (A → B) must fail"
# UID 1000 in the sandbox is iptables-restricted to loopback + DNS;
# attempting any TCP to another pod IP must be denied.
out=$(kubectl -n "$pod_ns_a" exec "$pod_a" -c openclaw -- \
    sh -c "timeout 5 sh -c 'cat </dev/tcp/${ip_b}/8443' 2>&1" || true)
if echo "$out" | grep -qiE 'permission denied|connection refused|operation not permitted|timed out|no route'; then
    log_pass "cross-tenant TCP was blocked: ${out:0:80}…"
else
    log_fail "cross-tenant TCP unexpectedly succeeded: ${out:0:200}"
fi

# 2. ServiceAccount + token isolation
log_step "[2/3] confirming distinct ServiceAccounts and no cluster-wide kubeconfig"
sa_a=$(kubectl -n "$pod_ns_a" get pod "$pod_a" -o jsonpath='{.spec.serviceAccountName}')
sa_b=$(kubectl -n "$pod_ns_b" get pod "$pod_b" -o jsonpath='{.spec.serviceAccountName}')
if [[ -n "$sa_a" && -n "$sa_b" && "$sa_a" != "default" && "$sa_b" != "default" ]]; then
    log_pass "tenants run as dedicated SAs: A=${sa_a}, B=${sa_b}"
else
    log_fail "expected dedicated ServiceAccounts; got A='${sa_a}', B='${sa_b}'"
fi
if kubectl -n "$pod_ns_a" exec "$pod_a" -c openclaw -- \
    test -f /root/.kube/config 2>/dev/null \
   || kubectl -n "$pod_ns_a" exec "$pod_a" -c openclaw -- \
    test -f /home/sandbox/.kube/config 2>/dev/null; then
    log_fail "found a kubeconfig inside the sandbox — tenant could escape"
else
    log_pass "no kubeconfig present in sandbox A"
fi

# 3. Controller / API server reach.
log_step "[3/3] sandbox cannot list cluster-wide pods"
out=$(kubectl -n "$pod_ns_a" exec "$pod_a" -c openclaw -- \
    sh -c 'curl -sS -o /dev/null -w "%{http_code}" -k https://kubernetes.default.svc/api/v1/pods 2>&1 || echo CURL_FAIL' || true)
if [[ "$out" == "403" ]] || [[ "$out" == "401" ]] || [[ "$out" == *"CURL_FAIL"* ]] || [[ "$out" == "000" ]]; then
    log_pass "sandbox got HTTP ${out} from kube API (denied / unreachable)"
else
    log_fail "sandbox got unexpected HTTP ${out} from kube API — investigate RBAC / NetworkPolicy"
fi

cleanup_sandbox "$ns_a" "$name_a"
cleanup_sandbox "$ns_b" "$name_b"
scenario_summary "Multi-tenant isolation"
