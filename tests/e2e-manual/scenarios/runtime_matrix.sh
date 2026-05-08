#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Manual E2E scenario: runtime matrix.
#
# For each first-class runtime, create a minimal ClawSandbox, wait for it
# to become Ready, verify the expected pod surface (init: egress-guard +
# main agent container + inference-router container), then tear down.
#
# Usage:
#   bash tests/e2e-manual/scenarios/runtime_matrix.sh
#   AZURECLAW_E2E_RUNTIMES="openclaw oai-agents" \
#       bash tests/e2e-manual/scenarios/runtime_matrix.sh
#
# Env:
#   AZURECLAW_E2E_RUNTIMES   space-separated subset of all_runtime_aliases
#                            (default: every alias)
#   MANUAL_E2E_TIMEOUT       per-sandbox readiness timeout (default 300)
#   MANUAL_E2E_KEEP_NS       1 to leave namespaces behind on success
#   MANUAL_E2E_VERBOSE       1 to dump describe + pods on failure

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$(cd "$SCRIPT_DIR/../lib" && pwd)"
# shellcheck source=../lib/common.sh
source "$LIB_DIR/common.sh"
# shellcheck source=../lib/cr_factory.sh
source "$LIB_DIR/cr_factory.sh"

scenario_header "Runtime matrix — every first-class runtime reaches Ready"

require_cluster
require_azureclaw_installed

RUNTIMES="${AZURECLAW_E2E_RUNTIMES:-$(all_runtime_aliases | tr '\n' ' ')}"
log_info "exercising runtimes: ${RUNTIMES}"

for runtime in $RUNTIMES; do
    name="manual-${runtime//[._]/-}"
    ns=$(new_ns "${runtime//[._]/-}")
    log_info "${runtime}: namespace=${ns}, sandbox=${name}"

    if ! cr_dispatch "$runtime" "$name" "$ns" | kubectl apply -f - >/dev/null; then
        log_fail "${runtime}: kubectl apply rejected by API server / admission"
        cleanup_ns "$ns"
        continue
    fi
    log_pass "${runtime}: ClawSandbox accepted by admission"

    if wait_for_clawsandbox_ready "$ns" "$name"; then
        # Belt-and-braces: confirm at least one pod is Running in the namespace.
        assert_pod_running "$ns" "azureclaw.azure.com/sandbox=${name}"
        # And confirm the inference-router container is present in the pod
        # spec (the CR factory above does not opt out of it).
        if kubectl -n "$ns" get pods -l "azureclaw.azure.com/sandbox=${name}" \
              -o jsonpath='{.items[0].spec.containers[*].name}' 2>/dev/null \
              | tr ' ' '\n' | grep -q '^inference-router$'; then
            log_pass "${runtime}: inference-router sidecar present"
        else
            log_fail "${runtime}: inference-router sidecar missing from pod spec"
        fi
    fi

    cleanup_ns "$ns"
done

scenario_summary "Runtime matrix"
