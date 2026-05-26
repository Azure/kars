#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Manual E2E scenario: runtime matrix.
#
# For each first-class runtime, create a minimal KarsSandbox, wait for it
# to become Ready, verify the expected pod surface (init: egress-guard +
# main agent container + inference-router container), then tear down.
#
# Usage:
#   bash tests/e2e-manual/scenarios/runtime_matrix.sh
#   KARS_E2E_RUNTIMES="openclaw oai-agents" \
#       bash tests/e2e-manual/scenarios/runtime_matrix.sh
#
# Env:
#   KARS_E2E_RUNTIMES   space-separated subset of all_runtime_aliases
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
require_kars_installed

RUNTIMES="${KARS_E2E_RUNTIMES:-$(all_runtime_aliases | tr '\n' ' ')}"
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
    metric_finish "admit_${name}" runtime admitKarsSandbox "runtime=${runtime}" "sandbox=${name}"
    metric_emit runtime crdAccepted count 1 "runtime=${runtime}" "sandbox=${name}"
    log_pass "${runtime}: KarsSandbox accepted by admission"

    if wait_for_karssandbox_ready "$ns" "$name"; then
        # Belt-and-braces: confirm at least one pod is Running in the
        # controller-managed pod namespace (kars-<name>). Only run
        # the sidecar / restart-count probes if we actually have a pod —
        # otherwise jsonpath '.items[0]…' silently returns "" and we
        # report a false [PASS].
        metric_start "podrun_${name}"
        if assert_pod_running "$pod_ns" "kars.azure.com/sandbox=${name}"; then
            metric_finish "podrun_${name}" runtime ttrPodRunning "runtime=${runtime}" "sandbox=${name}"
            if kubectl -n "$pod_ns" get pods -l "kars.azure.com/sandbox=${name}" \
                  -o jsonpath='{.items[0].spec.containers[*].name}' 2>/dev/null \
                  | tr ' ' '\n' | grep -q '^inference-router$'; then
                log_pass "${runtime}: inference-router sidecar present"
            else
                log_fail "${runtime}: inference-router sidecar missing from pod spec"
            fi

            local_restarts=$(kubectl -n "$pod_ns" get pods -l "kars.azure.com/sandbox=${name}" \
                -o jsonpath='{.items[*].status.containerStatuses[*].restartCount}' 2>/dev/null \
                | tr ' ' '\n' | awk 'BEGIN{s=0}{s+=$1}END{print s+0}')
            metric_emit runtime restartCount count "${local_restarts:-0}" \
                "runtime=${runtime}" "sandbox=${name}"
        else
            # Surface why no pod is there: KarsSandbox was Ready but the
            # controller never produced a deployment, or pods are stuck
            # ImagePullBackOff / CrashLoopBackOff. Dump enough state to
            # triage without re-running.
            log_info "${runtime}: dumping pod-ns state for triage"
            kubectl -n "$pod_ns" get all 2>&1 | sed 's/^/    /' || true
            kubectl -n "$pod_ns" get events --sort-by=.lastTimestamp 2>&1 \
                | tail -10 | sed 's/^/    /' || true
        fi
    fi

    metric_start "cleanup_${name}"
    cleanup_sandbox "$ns" "$name"
    metric_finish "cleanup_${name}" runtime cleanupNs "runtime=${runtime}" "sandbox=${name}"
done

scenario_summary "Runtime matrix"
