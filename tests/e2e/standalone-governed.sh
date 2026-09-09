#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# Independent API/protocol/broker qualification, not active-SRE or CNI proof.
# Manual CI selects e2e_suite=standalone-governed under a separate check name.
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/run.sh"

STANDALONE_CLUSTER_UID=""

standalone_diagnostics() {
    PYTHONDONTWRITEBYTECODE=1 PYTHONPATH="$SCRIPT_DIR" \
        python3 -m standalone_diagnostics "$1"
}

standalone_exit() {
    local result="$1"
    trap - EXIT
    if ! standalone_diagnostics final; then
        printf 'Standalone diagnostics incomplete; original failure is retained\n' >&2
        [ "$result" -ne 0 ] || result=1
    fi
    if ! standalone_cleanup; then
        [ "$result" -ne 0 ] || result=1
    fi
    exit "$result"
}

standalone_check() {
    local check="$1" stage="$2" success="${3:-}" before="$FAIL"
    if "$check"; then
        if [ "$FAIL" -eq "$before" ] && [ -n "$success" ]; then pass "$success"; fi
    elif [ "$FAIL" -eq "$before" ]; then
        fail "Standalone check failed: $check"
    fi
    if [ "$FAIL" -gt "$before" ]; then
        standalone_diagnostics "$stage" || fail "Standalone diagnostics incomplete: $stage"
    fi
}

standalone_budget() {
    node "$SCRIPT_DIR/inference-budget-enforcement.mjs"
}

prepare_standalone_namespace() {
    # The chart owns kars-system. Create that exact manifest with release
    # ownership before Helm needs its release-storage namespace; never adopt.
    helm template kars "$ROOT_DIR/deploy/helm/kars" --namespace kars-system \
        --show-only templates/namespace.yaml \
        | kubectl --context kind-kars-e2e --request-timeout=15s \
            create --dry-run=client --validate=strict -f - -o json \
        | python3 -c '
import json, sys
namespace = json.load(sys.stdin)
assert namespace.get("kind") == "Namespace"
metadata = namespace["metadata"]
assert metadata["name"] == "kars-system"
metadata.setdefault("labels", {})["app.kubernetes.io/managed-by"] = "Helm"
metadata.setdefault("annotations", {}).update({
    "meta.helm.sh/release-name": "kars",
    "meta.helm.sh/release-namespace": "kars-system",
})
json.dump(namespace, sys.stdout)
' \
        | kubectl --context kind-kars-e2e --request-timeout=15s create -f -
}

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
    export KARS_E2E_SUITE=standalone-governed
    export KARS_STANDALONE_CLUSTER_UID="$STANDALONE_CLUSTER_UID"
    trap 'standalone_exit "$?"' EXIT
    build_images
    prepare_managed_mcp
    prepare_standalone_namespace
    install_crds standalone-governed

    info "Standalone governed services: no active SRE, no CNI-enforcement claim"
    for check in test_crd_installed test_controller_running \
        test_controller_metrics_endpoint test_admission_policies_installed \
        test_operator_default_deny_np test_create_sandbox \
        test_sandbox_deployment_exists test_sandbox_pod_starts; do
        standalone_check "$check" "$check"
    done
    standalone_check test_governed_services services "Standalone private service credentials and scope lifecycle"
    standalone_check test_managed_mcp mcp
    standalone_check test_credential_sources credentials
    standalone_check standalone_budget budget "Standalone durable governed-inference broker enforcement"
    standalone_check test_cleanup_sandbox sandbox-cleanup
    printf 'Standalone governed services: %s passed, %s failed\n' "$PASS" "$FAIL"
    [ "$FAIL" -eq 0 ]
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    standalone_governed_main "$@"
fi
