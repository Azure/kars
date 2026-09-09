#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# Independent API/protocol/broker qualification, not active-SRE or CNI proof.
# Manual CI selects e2e_suite=standalone-governed under a separate check name.
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/run.sh"

STANDALONE_CLUSTER_UID=""

standalone_cleanup() {
    local current_uid
    current_uid=$(kubectl --context kind-kars-e2e --request-timeout=15s \
        get namespace kube-system -o jsonpath='{.metadata.uid}') || {
        printf 'Cannot verify standalone cluster ownership; refusing teardown\n' >&2
        return 1
    }
    if [ -z "$STANDALONE_CLUSTER_UID" ] || [ "$current_uid" != "$STANDALONE_CLUSTER_UID" ]; then
        printf 'Standalone cluster identity changed; refusing teardown\n' >&2
        return 1
    fi
    kind delete cluster --name "$CLUSTER_NAME" || return 1
    rm -f "$E2E_KUBECONFIG"
}

standalone_governed_main() {
    umask 077
    local clusters check
    clusters=$(kind get clusters) || return 1
    if printf '%s\n' "$clusters" | grep -qx "$CLUSTER_NAME"; then
        printf 'Standalone qualification requires a fresh Kind cluster; refusing reuse\n' >&2
        return 1
    fi
    if [ -e "$E2E_KUBECONFIG" ] || [ -L "$E2E_KUBECONFIG" ]; then
        printf 'Standalone qualification refuses an existing kubeconfig file\n' >&2
        return 1
    fi
    setup_cluster
    STANDALONE_CLUSTER_UID=$(kubectl --context kind-kars-e2e --request-timeout=15s \
        get namespace kube-system -o jsonpath='{.metadata.uid}')
    [ -n "$STANDALONE_CLUSTER_UID" ] || {
        printf 'Standalone cluster identity is unavailable\n' >&2
        return 1
    }
    trap standalone_cleanup EXIT
    build_images
    prepare_managed_mcp
    install_crds standalone-governed

    info "Standalone governed services: no active SRE, no CNI-enforcement claim"
    for check in test_crd_installed test_controller_running \
        test_controller_metrics_endpoint test_admission_policies_installed \
        test_operator_default_deny_np test_create_sandbox \
        test_sandbox_deployment_exists test_sandbox_pod_starts; do
        "$check" || fail "Standalone prerequisite failed: $check"
    done
    if test_governed_services; then
        pass "Standalone private service credentials and scope lifecycle"
    else
        fail "Standalone private service credentials and scope lifecycle"
    fi
    test_managed_mcp || fail "Standalone managed MCP lifecycle/protocol"
    test_credential_sources || fail "Standalone legacy credential-source lifecycle"
    if node "$SCRIPT_DIR/inference-budget-enforcement.mjs"; then
        pass "Standalone durable governed-inference broker enforcement"
    else
        fail "Standalone durable governed-inference broker enforcement"
    fi
    test_cleanup_sandbox || fail "Standalone sandbox cleanup"
    printf 'Standalone governed services: %s passed, %s failed\n' "$PASS" "$FAIL"
    [ "$FAIL" -eq 0 ]
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    standalone_governed_main "$@"
fi
