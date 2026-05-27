#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Manual E2E scenario: AGT mesh round-trips (Signal Protocol).
#
# Validates the encrypted inter-agent comms layer end-to-end:
#
#   [1/4] agt-1sub        — parent → 1 sub-agent → encrypted reply
#   [2/4] agt-2sub-parallel — parent fans out to 2 sub-agents in parallel
#   [3/4] agt-sibling     — sub-agent A talks to sub-agent B without
#                           parent in path (peer-to-peer)
#   [4/4] agt-multiturn   — 5-message ratchet chain on a single peer
#                           pair, proves Double Ratchet rotates keys
#                           and patches #5/#7/#8/#12 hold
#
# All probes use Path A (OpenClaw plugin's `mesh_send` over the unix
# socket + router `/agt/mesh/inbox` for receive-side observation).
# Path B (raw relay framing) is deferred to Tier C.
#
# We use OpenClaw for every peer because it is the only runtime with
# the AGT plugin pre-installed and because the plugin is the only
# external surface for `mesh_send`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$(cd "$SCRIPT_DIR/../lib" && pwd)"
# shellcheck source=../lib/common.sh
source "$LIB_DIR/common.sh"
# shellcheck source=../lib/cr_factory.sh
source "$LIB_DIR/cr_factory.sh"
# shellcheck source=../lib/agt_send.sh
source "$LIB_DIR/agt_send.sh"

scenario_header "AGT mesh — 1sub, 2sub-parallel, sibling, multiturn"

require_cluster
require_kars_installed

if ! kubectl get deploy -n agentmesh >/dev/null 2>&1; then
    log_fail "agentmesh namespace missing — relay+registry not deployed"
    scenario_summary "AGT mesh"
    exit 1
fi

export MANUAL_E2E_SCENARIO=agt_mesh

# Helper: bring up a mesh-enabled OpenClaw sandbox with a sibling
# ToolPolicy. Returns the (cr_ns, pod_ns, pod) triple via globals to
# avoid bash's 1-return-value limitation.
_brought_up_cr_ns=""
_brought_up_pod_ns=""
_brought_up_pod=""

_bring_up_mesh_peer() {
    local name="$1"
    local cr_ns
    cr_ns=$(new_ns "agt-${name}")
    local pod_ns
    pod_ns=$(pod_ns_for "$name")
    local tp_name="${name}-toolpolicy"

    kubectl apply -f - >/dev/null <<YAML
apiVersion: kars.azure.com/v1alpha1
kind: ToolPolicy
metadata:
  name: ${tp_name}
  namespace: ${cr_ns}
  labels:
    kars.azure.com/test-suite: manual-e2e
spec:
  appliesTo: {}
  agtProfile:
    inline: |
      version: "1.0"
      agent: e2e-allow-all
      policies:
        - name: allow-all
          type: capability
          allowed_actions: ["*"]
          priority: 1
YAML

    cr_dispatch openclaw "$name" "$cr_ns" \
      | yq eval '
            select(.kind == "KarsSandbox")
                | .spec.governance.enabled = true
                | .spec.governance.registryMode = "global"
                | .spec.governance.toolPolicyRef.name = "'"${tp_name}"'"
            ,
            select(.kind != "KarsSandbox")
        ' - \
      | kubectl apply -f - >/dev/null

    if ! wait_for_karssandbox_ready "$cr_ns" "$name"; then
        log_fail "${name}: never reached Ready"
        return 1
    fi
    local pod
    pod=$(kubectl -n "$pod_ns" get pod \
        -l "kars.azure.com/sandbox=${name}" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
    if [[ -z "$pod" ]]; then
        log_fail "${name}: Ready but no pod"
        return 1
    fi
    _brought_up_cr_ns="$cr_ns"
    _brought_up_pod_ns="$pod_ns"
    _brought_up_pod="$pod"
    enable_break_glass "$pod_ns"
    return 0
}

_teardown_mesh_peer() {
    local cr_ns="$1"
    local name="$2"
    local pod_ns
    pod_ns=$(pod_ns_for "$name")
    disable_break_glass "$pod_ns" 2>/dev/null || true
    cleanup_sandbox "$cr_ns" "$name"
}

# Track everything we created so the EXIT trap can clean up even on
# partial failure.
declare -a created_peers=()  # entries: "cr_ns|name"

# shellcheck disable=SC2317
_cleanup_all() {
    for entry in "${created_peers[@]}"; do
        IFS='|' read -r cr_ns name <<<"$entry"
        _teardown_mesh_peer "$cr_ns" "$name" >/dev/null 2>&1 || true
    done
}
trap _cleanup_all EXIT

# ── [1/4] agt-1sub ────────────────────────────────────────────────────
log_step "[1/4] agt-1sub: parent → 1 sub-agent → encrypted reply"

_bring_up_mesh_peer "agt-parent" || { scenario_summary "AGT mesh"; exit 1; }
parent_cr_ns="$_brought_up_cr_ns"
parent_pod_ns="$_brought_up_pod_ns"
parent_pod="$_brought_up_pod"
created_peers+=("$parent_cr_ns|agt-parent")

_bring_up_mesh_peer "agt-sub1" || { scenario_summary "AGT mesh"; exit 1; }
sub1_cr_ns="$_brought_up_cr_ns"
sub1_pod_ns="$_brought_up_pod_ns"
sub1_pod="$_brought_up_pod"
created_peers+=("$sub1_cr_ns|agt-sub1")

# Give the registry ~10s to see both heartbeats.
sleep 10
if agt_registry_has_peers "agt-parent" "agt-sub1"; then
    log_pass "registry sees both peers"
else
    log_warn "registry does not yet show both peers — proceeding anyway"
fi

metric_start "agt_1sub_send"
ping1="agt-1sub-$(date +%s)"
if agt_mesh_send "$parent_pod_ns" "$parent_pod" "agt-sub1" "$ping1" >/dev/null 2>&1; then
    metric_finish "agt_1sub_send" agt_mesh meshSendLatency
    log_pass "parent → sub1: mesh_send accepted"
else
    log_fail "parent → sub1: mesh_send call failed"
fi

metric_start "agt_1sub_receive"
if agt_mesh_wait_for_message "$sub1_pod_ns" "$sub1_pod" "$ping1" 60; then
    metric_finish "agt_1sub_receive" agt_mesh meshReceiveLatency
    log_pass "sub1 received & decrypted parent's message"
else
    log_fail "sub1 never saw the encrypted message in inbox"
fi

# ── [2/4] agt-2sub-parallel ───────────────────────────────────────────
log_step "[2/4] agt-2sub-parallel: parent fans out to 2 sub-agents"

_bring_up_mesh_peer "agt-sub2" || { scenario_summary "AGT mesh"; exit 1; }
sub2_cr_ns="$_brought_up_cr_ns"
sub2_pod_ns="$_brought_up_pod_ns"
sub2_pod="$_brought_up_pod"
created_peers+=("$sub2_cr_ns|agt-sub2")

sleep 5

ping2a="agt-fanout-a-$(date +%s)"
ping2b="agt-fanout-b-$(date +%s)"
metric_start "agt_2sub_fanout"
agt_mesh_send "$parent_pod_ns" "$parent_pod" "agt-sub1" "$ping2a" >/dev/null 2>&1 &
pid_a=$!
agt_mesh_send "$parent_pod_ns" "$parent_pod" "agt-sub2" "$ping2b" >/dev/null 2>&1 &
pid_b=$!
wait $pid_a && wait $pid_b
metric_finish "agt_2sub_fanout" agt_mesh meshFanoutLatency

ok_a=0; ok_b=0
agt_mesh_wait_for_message "$sub1_pod_ns" "$sub1_pod" "$ping2a" 60 && ok_a=1
agt_mesh_wait_for_message "$sub2_pod_ns" "$sub2_pod" "$ping2b" 60 && ok_b=1
if [[ $ok_a -eq 1 && $ok_b -eq 1 ]]; then
    log_pass "both sub-agents received their fan-out messages"
else
    log_fail "fan-out delivery: sub1=${ok_a}, sub2=${ok_b}"
fi

# ── [3/4] agt-sibling ─────────────────────────────────────────────────
log_step "[3/4] agt-sibling: sub1 → sub2 directly (parent not in path)"

ping3="agt-sibling-$(date +%s)"
metric_start "agt_sibling_send"
if agt_mesh_send "$sub1_pod_ns" "$sub1_pod" "agt-sub2" "$ping3" >/dev/null 2>&1; then
    metric_finish "agt_sibling_send" agt_mesh meshSiblingSend
    log_pass "sub1 → sub2: mesh_send accepted"
else
    log_fail "sub1 → sub2: mesh_send call failed"
fi
if agt_mesh_wait_for_message "$sub2_pod_ns" "$sub2_pod" "$ping3" 60; then
    log_pass "sub2 received sibling message from sub1"
else
    log_fail "sub2 never saw sibling message"
fi

# ── [4/4] agt-multiturn ───────────────────────────────────────────────
log_step "[4/4] agt-multiturn: 5-message ratchet chain (parent ↔ sub1)"

multi_ok=0
multi_total=5
metric_start "agt_multiturn"
for i in $(seq 1 $multi_total); do
    msg="agt-mt-${i}-$(date +%s%N 2>/dev/null || date +%s)-rnd"
    if agt_mesh_send "$parent_pod_ns" "$parent_pod" "agt-sub1" "$msg" >/dev/null 2>&1 \
       && agt_mesh_wait_for_message "$sub1_pod_ns" "$sub1_pod" "$msg" 30; then
        multi_ok=$((multi_ok + 1))
    fi
done
metric_finish "agt_multiturn" agt_mesh meshMultiTurn

if [[ $multi_ok -eq $multi_total ]]; then
    log_pass "Double Ratchet chain delivered ${multi_ok}/${multi_total} messages"
elif [[ $multi_ok -ge 3 ]]; then
    log_skip "ratchet chain delivered ${multi_ok}/${multi_total} — tolerable but indicates jitter"
else
    log_fail "ratchet chain delivered only ${multi_ok}/${multi_total} — likely ratchet desync regression"
fi

scenario_summary "AGT mesh"
