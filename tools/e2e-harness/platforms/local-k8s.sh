# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# platforms/local-k8s.sh — local kind-based platform helper for the e2e-harness.
#
# Inherits the AKS helper for everything past cluster bring-up — once a
# kind cluster has the Kars chart installed, the K8s API surface
# is identical to AKS (same CRDs, same controller, same router image).
# The only differences this helper adds:
#
#   1. Bring-up: invoke `kars dev --target local-k8s` to create
#      the kind cluster + install the chart + load the local controller/
#      router/sandbox images. We detect-and-skip if the cluster is
#      already up.
#   2. KUBECONFIG: kind clusters publish a kubeconfig on stdout; we
#      ensure the caller's current context targets the kind cluster
#      before invoking the AKS helper's apply/wait/post sequence.
#   3. Sub-agent caveats: on a single-node kind cluster the Cilium
#      NetworkPolicy dataplane is NOT enabled by default; ingress
#      isolation falls back to kindnetd, which does not enforce NP.
#      The harness reports this in preflight so verify.py's egress
#      check is interpreted with the right caveats.
#
# Inputs (env, all optional):
#   KIND_CLUSTER_NAME — kind cluster to create/use (default: kars-dev)
#                       — matches the CLI's `--cluster-name` default.
#   SKIP_DEV_BRINGUP  — set to 1 to assume the cluster + chart already
#                       exist and skip the `kars dev` step. Useful
#                       when iterating on the harness itself.

set -euo pipefail

KIND_CLUSTER_NAME="${KIND_CLUSTER_NAME:-kars-dev}"

# Source the AKS helper first so we can reuse its apply/credentials/
# wait/post/collect functions. We override platform_preflight (and
# optionally credentials) below.
# shellcheck disable=SC1091
source "${SCRIPT_DIR}/platforms/aks.sh"

# Override: local-k8s preflight first ensures the kind cluster + chart
# are up, then runs the K8s connectivity / CRD checks inline (we don't
# call the AKS preflight directly because it also performs AKS-only
# kubectl context shape checks).
platform_preflight() {
    command -v docker >/dev/null || { log "ERR docker not on PATH"; exit 1; }
    command -v kind >/dev/null || { log "ERR kind not on PATH (install via 'brew install kind')"; exit 1; }
    command -v kubectl >/dev/null || { log "ERR kubectl not on PATH"; exit 1; }
    command -v kars >/dev/null || { log "ERR kars CLI not on PATH (run 'npm run build' under cli/)"; exit 1; }

    if [ "${SKIP_DEV_BRINGUP:-0}" = "1" ]; then
        log "SKIP_DEV_BRINGUP=1 — assuming kind cluster '${KIND_CLUSTER_NAME}' and chart already present"
    elif kind get clusters 2>/dev/null | grep -qx "${KIND_CLUSTER_NAME}"; then
        log "kind cluster '${KIND_CLUSTER_NAME}' already exists — skipping bring-up"
    else
        log "bringing up local-k8s via 'kars dev --target local-k8s --cluster-name ${KIND_CLUSTER_NAME} --once'"
        kars dev --target local-k8s \
            --cluster-name "${KIND_CLUSTER_NAME}" \
            --once \
            >>"${OUT_DIR}/dev-bringup.log" 2>&1 || {
                log "ERR kars dev bring-up failed; tail of dev-bringup.log:"
                tail -n 80 "${OUT_DIR}/dev-bringup.log" >&2 || true
                exit 1
            }
    fi

    # Ensure the kubectl context points at the kind cluster.
    local desired="kind-${KIND_CLUSTER_NAME}"
    local current
    current=$(kubectl config current-context 2>/dev/null || true)
    if [ "${current}" != "${desired}" ]; then
        log "switching kubectl context: ${current} → ${desired}"
        kubectl config use-context "${desired}" \
            >>"${OUT_DIR}/dev-bringup.log" 2>&1 || {
                log "ERR could not switch to context '${desired}'"; exit 1
            }
    fi

    # Defer to AKS preflight for CRD presence (same chart, same CRDs).
    # We can't call the prior definition directly because we redefined
    # the function — inline the relevant check instead.
    for crd in karssandboxes.kars.azure.com \
               inferencepolicies.kars.azure.com \
               toolpolicies.kars.azure.com \
               karsmemories.kars.azure.com \
               mcpservers.kars.azure.com; do
        kubectl get crd "$crd" >/dev/null 2>&1 || {
            log "ERR CRD ${crd} missing in kind cluster — bring-up did not install the chart"; exit 1
        }
    done

    # Honest caveat: kindnetd ≠ NetworkPolicy enforcement. The harness
    # writes this into the run's notes so verify.py results are read
    # with the right context.
    {
        echo "platform: local-k8s"
        echo "kind cluster: ${KIND_CLUSTER_NAME}"
        echo "kubectl context: ${desired}"
        echo "caveats:"
        echo "  - NetworkPolicy enforcement: kindnetd does NOT enforce NP."
        echo "    For ingress isolation parity with AKS, install Cilium"
        echo "    or another NP-capable CNI in the kind cluster."
        echo "  - Foundry hosted tools: require an Azure connection-string"
        echo "    (no Workload Identity inside kind). If the scenario"
        echo "    invokes foundry_* tools, set the appropriate provider"
        echo "    credentials before running."
    } >"${OUT_DIR}/platform-notes.txt"

    log "local-k8s preflight ok — kubectl context: ${desired}"
}
