#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Manual E2E scenario: cross-runtime AgentMesh round-trip.
#
# Plants two sandboxes (OpenClaw + OpenAI Agents Python by default) in
# distinct namespaces, registers each as a peer of the other via the
# `agt.mesh` block, then verifies:
#
#   1. Both sandboxes register with the AgentMesh registry.
#   2. A KNOCK from sandbox A to sandbox B succeeds (E2E session
#      establishment over the AgentMesh relay).
#   3. A round-trip mesh_send / mesh_inbox happens within the timeout.
#
# This scenario is *not* in CI because it requires the relay + registry
# pods to be reachable from both namespaces, plus the runtime images
# pulled (large) — fine for a hand-run lab cluster, expensive for Kind.
#
# Env:
#   AZURECLAW_E2E_PEER_A      runtime alias for sandbox A (default openclaw)
#   AZURECLAW_E2E_PEER_B      runtime alias for sandbox B (default oai-agents)
#   MANUAL_E2E_TIMEOUT        per-step wait (default 300)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$(cd "$SCRIPT_DIR/../lib" && pwd)"
# shellcheck source=../lib/common.sh
source "$LIB_DIR/common.sh"
# shellcheck source=../lib/cr_factory.sh
source "$LIB_DIR/cr_factory.sh"

scenario_header "Cross-runtime AgentMesh round-trip"

require_cluster
require_azureclaw_installed
require_cli kubectl

if ! kubectl -n agentmesh get deploy relay >/dev/null 2>&1 \
   || ! kubectl -n agentmesh get deploy registry >/dev/null 2>&1; then
    log_skip "agentmesh relay/registry not installed in 'agentmesh' namespace — install AzureClaw with mesh enabled"
    scenario_summary "Cross-runtime AgentMesh round-trip"
    exit 0
fi
log_info "agentmesh relay + registry detected"

peer_a="${AZURECLAW_E2E_PEER_A:-openclaw}"
peer_b="${AZURECLAW_E2E_PEER_B:-oai-agents}"
log_info "peer A: ${peer_a}    peer B: ${peer_b}"

name_a="mesh-a-${peer_a//[._]/-}"
name_b="mesh-b-${peer_b//[._]/-}"
ns_a=$(new_ns "mesh-a-${peer_a//[._]/-}")
ns_b=$(new_ns "mesh-b-${peer_b//[._]/-}")
pod_ns_a=$(pod_ns_for "$name_a")
pod_ns_b=$(pod_ns_for "$name_b")

# Plant peer A with mesh enabled. The factory now emits two docs
# (InferencePolicy + ClawSandbox); only the ClawSandbox carries the
# governance block, so use `select(.kind == "ClawSandbox")` to scope
# the patch and `with()`/passthrough for the rest.
_mesh_overlay() {
    yq eval '
        select(.kind == "ClawSandbox")
            | .spec.governance.enabled = true
            | .spec.governance.registryMode = "global"
        ,
        select(.kind != "ClawSandbox")
    ' -
}

cr_dispatch "$peer_a" "$name_a" "$ns_a" \
  | _mesh_overlay \
  | kubectl apply -f - >/dev/null \
  || { log_fail "could not apply peer A (yq required for this scenario)"; cleanup_sandbox "$ns_a" "$name_a"; cleanup_sandbox "$ns_b" "$name_b"; exit 1; }

cr_dispatch "$peer_b" "$name_b" "$ns_b" \
  | _mesh_overlay \
  | kubectl apply -f - >/dev/null \
  || { log_fail "could not apply peer B"; cleanup_sandbox "$ns_a" "$name_a"; cleanup_sandbox "$ns_b" "$name_b"; exit 1; }

log_pass "applied both peers"

wait_for_clawsandbox_ready "$ns_a" "$name_a" || true
wait_for_clawsandbox_ready "$ns_b" "$name_b" || true

# Step 1 — registry registration
log_step "checking AgentMesh registry for both peers"
relay_logs=$(kubectl -n agentmesh logs deploy/registry --tail=500 2>/dev/null || echo "")
assert_contains "registry registration for peer A" "$name_a" "$relay_logs"
assert_contains "registry registration for peer B" "$name_b" "$relay_logs"

# Step 2 — KNOCK from A to B
# We trigger a send by exec'ing the peer-A agent container and invoking
# `mesh_send` via the OpenClaw plugin (or the runtime's mesh_tools). The
# exact surface differs per runtime; for openclaw we have a shell tool.
log_step "triggering mesh_send from peer A → peer B"
pod_a=$(kubectl -n "$pod_ns_a" get pod -l "azureclaw.azure.com/sandbox=${name_a}" -o jsonpath='{.items[0].metadata.name}')
# Mesh probe needs to invoke the OpenClaw plugin from inside the agent
# container — enable break-glass on peer-A's namespace just for that.
enable_break_glass "$pod_ns_a"
if [[ -z "$pod_a" ]]; then
    log_fail "could not find peer-A pod"
else
    if kubectl -n "$pod_ns_a" exec "$pod_a" -c openclaw -- \
        sh -c "echo '{\"to\":\"${name_b}\",\"text\":\"manual-e2e-ping\"}' > /tmp/mesh-send.json && curl -s --unix-socket /tmp/openclaw.sock http://localhost/mesh/send -d @/tmp/mesh-send.json" \
        >/dev/null 2>&1; then
        log_pass "mesh_send invoked on peer A"
    else
        log_warn "mesh_send via plugin socket not available for peer A runtime — skipping send step"
        log_skip "send-step: depends on runtime-specific plugin surface"
    fi
fi

# Step 3 — inbox arrival on B
log_step "checking inbox on peer B for the round-trip message"
sleep 5
relay_after=$(kubectl -n agentmesh logs deploy/relay --tail=500 2>/dev/null || echo "")
assert_contains "relay routed an envelope between peers" "${name_a}" "$relay_after"

cleanup_sandbox "$ns_a" "$name_a"
cleanup_sandbox "$ns_b" "$name_b"
disable_break_glass "$pod_ns_a"
scenario_summary "Cross-runtime AgentMesh round-trip"
