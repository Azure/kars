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

export MANUAL_E2E_SCENARIO=runtime

for runtime in $RUNTIMES; do
    name="manual-${runtime//[._]/-}"
    ns=$(new_ns "${runtime//[._]/-}")
    pod_ns=$(pod_ns_for "$name")
    log_info "${runtime}: cr_ns=${ns}, pod_ns=${pod_ns}, sandbox=${name}"

    metric_start "admit_${name}"
    if ! cr_dispatch "$runtime" "$name" "$ns" | kubectl apply -f - >/dev/null; then
        log_fail "${runtime}: kubectl apply rejected by API server / admission"
        metric_emit runtime crdRejected count 1 "runtime=${runtime}" "sandbox=${name}"
        cleanup_sandbox "$ns" "$name"
        continue
    fi
    metric_finish "admit_${name}" runtime admitClawSandbox "runtime=${runtime}" "sandbox=${name}"
    metric_emit runtime crdAccepted count 1 "runtime=${runtime}" "sandbox=${name}"
    log_pass "${runtime}: ClawSandbox accepted by admission"

    if wait_for_clawsandbox_ready "$ns" "$name"; then
        # Belt-and-braces: confirm at least one pod is Running in the
        # controller-managed pod namespace (azureclaw-<name>).
        metric_start "podrun_${name}"
        if assert_pod_running "$pod_ns" "azureclaw.azure.com/sandbox=${name}"; then
            metric_finish "podrun_${name}" runtime ttrPodRunning "runtime=${runtime}" "sandbox=${name}"
        fi
        # And confirm the inference-router container is present in the pod
        # spec (the CR factory above does not opt out of it).
        if kubectl -n "$pod_ns" get pods -l "azureclaw.azure.com/sandbox=${name}" \
              -o jsonpath='{.items[0].spec.containers[*].name}' 2>/dev/null \
              | tr ' ' '\n' | grep -q '^inference-router$'; then
            log_pass "${runtime}: inference-router sidecar present"
        else
            log_fail "${runtime}: inference-router sidecar missing from pod spec"
        fi

        # Restart-count snapshot (post-readiness) — useful for spotting
        # crashloops that recovered just in time to flip Ready true.
        local_restarts=$(kubectl -n "$pod_ns" get pods -l "azureclaw.azure.com/sandbox=${name}" \
            -o jsonpath='{.items[*].status.containerStatuses[*].restartCount}' 2>/dev/null \
            | tr ' ' '\n' | awk 'BEGIN{s=0}{s+=$1}END{print s+0}')
        metric_emit runtime restartCount count "${local_restarts:-0}" \
            "runtime=${runtime}" "sandbox=${name}"
    fi

    metric_start "cleanup_${name}"
    cleanup_sandbox "$ns" "$name"
    metric_finish "cleanup_${name}" runtime cleanupNs "runtime=${runtime}" "sandbox=${name}"
done

scenario_summary "Runtime matrix"
