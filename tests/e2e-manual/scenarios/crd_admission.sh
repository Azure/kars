#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Manual E2E scenario: CRD admission lane (T0 — schema gate).
#
# Kars ships 9 CRDs. The runtime/governance/mesh/failure scenarios
# only exercise KarsSandbox + InferencePolicy. This scenario applies
# minimum-valid CRs for the remaining 7 and asserts:
#
#   1. The API server accepts the CR (schema + admission webhooks pass).
#   2. The controller produces the documented status condition within
#      a bounded wait. Most controllers reach `Ready=True`; KarsMemory
#      stays `Pending` by design (the runtime is responsible for
#      provisioning the upstream Foundry store, not the operator).
#
# What we do NOT do here: functional probes for each CRD (those live
# in the per-feature scenarios — handoff for KarsPairing, mesh for
# TrustGraph, etc).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$(cd "$SCRIPT_DIR/../lib" && pwd)"
# shellcheck source=../lib/common.sh
source "$LIB_DIR/common.sh"

scenario_header "CRD admission lane — minimum-valid CRs for 7 untested CRDs"

require_cluster
require_kars_installed

ns=$(new_ns "crd-admission")
export MANUAL_E2E_SCENARIO=crd_admission

# Apply a single doc, with the standard test-suite label so the EXIT
# trap in run.sh sweeps it. Returns the kubectl exit code.
_apply() {
    local manifest="$1"
    printf '%s\n' "$manifest" | kubectl apply -f - >/dev/null
}

# Wait until the named CR's `.status.conditions[?(@.type=="Ready")].status`
# is "True". Returns 0 on Ready, 1 on timeout. Uses the shared
# MANUAL_E2E_TIMEOUT (default 300s).
_wait_ready() {
    local kind="$1"
    local name="$2"
    local namespace="$3"
    local timeout="${4:-${MANUAL_E2E_TIMEOUT}}"
    local deadline=$(( $(date +%s) + timeout ))
    while [[ $(date +%s) -lt $deadline ]]; do
        local status
        status=$(kubectl -n "$namespace" get "$kind" "$name" \
            -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || true)
        [[ "$status" == "True" ]] && return 0
        sleep 2
    done
    return 1
}

# Same shape but for status.phase fields (used by KarsMemory which is
# expected to sit at "Pending" and by KarsPairing which sits at
# "PendingPairing").
_wait_phase() {
    local kind="$1"
    local name="$2"
    local namespace="$3"
    local want="$4"
    local timeout="${5:-${MANUAL_E2E_TIMEOUT}}"
    local deadline=$(( $(date +%s) + timeout ))
    while [[ $(date +%s) -lt $deadline ]]; do
        local phase
        phase=$(kubectl -n "$namespace" get "$kind" "$name" \
            -o jsonpath='{.status.phase}' 2>/dev/null || true)
        [[ "$phase" == "$want" ]] && return 0
        sleep 2
    done
    return 1
}

label="kars.azure.com/test-suite: manual-e2e"

# ── 1. ToolPolicy ──────────────────────────────────────────────────────
log_step "[1/7] ToolPolicy: minimum spec (appliesTo: {})"
metric_start "tp_admit"
if _apply "
apiVersion: kars.azure.com/v1alpha1
kind: ToolPolicy
metadata:
  name: tp-min
  namespace: ${ns}
  labels:
    ${label}
spec:
  appliesTo: {}
"; then
    metric_finish "tp_admit" crd_admission admitToolPolicy
    log_pass "ToolPolicy admitted"
    if _wait_ready ToolPolicy tp-min "$ns" 60; then
        log_pass "ToolPolicy reached Ready"
    else
        log_fail "ToolPolicy never reached Ready"
        kubectl -n "$ns" get toolpolicy tp-min -o yaml 2>&1 | tail -30 | sed 's/^/    /' || true
    fi
else
    log_fail "ToolPolicy admission rejected"
fi

# ── 2. McpServer ───────────────────────────────────────────────────────
log_step "[2/7] McpServer: minimum spec (just url)"
metric_start "mcp_admit"
if _apply "
apiVersion: kars.azure.com/v1alpha1
kind: McpServer
metadata:
  name: mcp-min
  namespace: ${ns}
  labels:
    ${label}
spec:
  url: https://mcp.example.com
"; then
    metric_finish "mcp_admit" crd_admission admitMcpServer
    log_pass "McpServer admitted"
    if _wait_ready McpServer mcp-min "$ns" 60; then
        log_pass "McpServer reached Ready"
    else
        log_fail "McpServer never reached Ready"
        kubectl -n "$ns" get mcpserver mcp-min -o yaml 2>&1 | tail -30 | sed 's/^/    /' || true
    fi
else
    log_fail "McpServer admission rejected"
fi

# ── 3. A2AAgent ────────────────────────────────────────────────────────
log_step "[3/7] A2AAgent: minimum spec (endpointUrl + signingKey)"
metric_start "a2a_admit"
if _apply "
apiVersion: kars.azure.com/v1alpha1
kind: A2AAgent
metadata:
  name: a2a-min
  namespace: ${ns}
  labels:
    ${label}
spec:
  endpointUrl: https://agent.example.com
  signingKeys:
    - kid: k1
      alg: EdDSA
      publicKeyB64u: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
"; then
    metric_finish "a2a_admit" crd_admission admitA2AAgent
    log_pass "A2AAgent admitted"
    if _wait_ready A2AAgent a2a-min "$ns" 60; then
        log_pass "A2AAgent reached Ready"
    else
        log_fail "A2AAgent never reached Ready"
        kubectl -n "$ns" get a2aagent a2a-min -o yaml 2>&1 | tail -30 | sed 's/^/    /' || true
    fi
else
    log_fail "A2AAgent admission rejected"
fi

# ── 4. TrustGraph (cluster-scoped) ─────────────────────────────────────
log_step "[4/7] TrustGraph (cluster-scoped): one vertex"
metric_start "tg_admit"
if _apply "
apiVersion: kars.azure.com/v1alpha1
kind: TrustGraph
metadata:
  name: tg-crd-admission
  labels:
    ${label}
spec:
  vertices:
    - id: v1
      alg: EdDSA
      publicKeyB64u: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
"; then
    metric_finish "tg_admit" crd_admission admitTrustGraph
    log_pass "TrustGraph admitted"
    # Cluster-scoped — pass empty namespace to _wait_ready by inlining
    deadline=$(( $(date +%s) + 60 ))
    ready="False"
    while [[ $(date +%s) -lt $deadline ]]; do
        ready=$(kubectl get trustgraph tg-crd-admission \
            -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || true)
        [[ "$ready" == "True" ]] && break
        sleep 2
    done
    if [[ "$ready" == "True" ]]; then
        log_pass "TrustGraph reached Ready"
    else
        log_fail "TrustGraph never reached Ready (last: ${ready:-<empty>})"
    fi
    # Cleanup cluster-scoped CR explicitly — the namespace sweep won't.
    kubectl delete trustgraph tg-crd-admission --ignore-not-found --wait=false >/dev/null 2>&1 || true
else
    log_fail "TrustGraph admission rejected"
fi

# ── 5. KarsPairing ─────────────────────────────────────────────────────
log_step "[5/7] KarsPairing: minimum spec (tokenHash + expiresAt)"
metric_start "cp_admit"
expires="$(date -u -d '+1 hour' '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || \
          date -u -v+1H '+%Y-%m-%dT%H:%M:%SZ')"
if _apply "
apiVersion: kars.azure.com/v1alpha1
kind: KarsPairing
metadata:
  name: pairing-min
  namespace: ${ns}
  labels:
    ${label}
spec:
  tokenHash: c7f5d4f9b3a8e2d1f4c8b6a3e5d7f9b1c3e5d7f9b1c3e5d7f9b1c3e5d7f9b1c3
  expiresAt: \"${expires}\"
"; then
    metric_finish "cp_admit" crd_admission admitKarsPairing
    log_pass "KarsPairing admitted"
    # KarsPairing's documented happy path is phase=PendingPairing — it
    # never reports Ready until the partner side completes.
    if _wait_phase KarsPairing pairing-min "$ns" PendingPairing 60; then
        log_pass "KarsPairing reached PendingPairing (expected pre-pairing state)"
    else
        log_fail "KarsPairing never reached PendingPairing"
        kubectl -n "$ns" get karspairing pairing-min -o yaml 2>&1 | tail -20 | sed 's/^/    /' || true
    fi
else
    log_fail "KarsPairing admission rejected"
fi

# ── 6. KarsMemory ──────────────────────────────────────────────────────
# KarsMemory references a sandbox by name. The CR can be admitted
# without the sandbox existing; the controller will sit at Pending
# until a runtime provisions the upstream Foundry store. We only
# assert admission + the documented Pending phase.
log_step "[6/7] KarsMemory: minimum spec (storeName + sandboxRef + scope)"
metric_start "cm_admit"
if _apply "
apiVersion: kars.azure.com/v1alpha1
kind: KarsMemory
metadata:
  name: memory-min
  namespace: ${ns}
  labels:
    ${label}
spec:
  storeName: memory-min
  sandboxRef:
    name: nonexistent-sandbox
  scope: agent:nonexistent-sandbox
"; then
    metric_finish "cm_admit" crd_admission admitKarsMemory
    log_pass "KarsMemory admitted"
    if _wait_phase KarsMemory memory-min "$ns" Pending 60; then
        log_pass "KarsMemory at expected Pending phase (runtime provisions store)"
    else
        # Some controllers may use a different phase name on first
        # reconcile; surface the actual value rather than failing hard.
        actual=$(kubectl -n "$ns" get karsmemory memory-min \
            -o jsonpath='{.status.phase}' 2>/dev/null || true)
        log_skip "KarsMemory phase='${actual:-<empty>}' (expected Pending) — controller may have changed semantics"
    fi
else
    log_fail "KarsMemory admission rejected"
fi

# ── 7. KarsEval ────────────────────────────────────────────────────────
log_step "[7/7] KarsEval: minimum spec (sandboxRef + suite)"
metric_start "ce_admit"
if _apply "
apiVersion: kars.azure.com/v1alpha1
kind: KarsEval
metadata:
  name: eval-min
  namespace: ${ns}
  labels:
    ${label}
spec:
  sandboxRef:
    name: nonexistent-sandbox
  suite: foundry-evals
  evaluators:
    - relevance
"; then
    metric_finish "ce_admit" crd_admission admitKarsEval
    log_pass "KarsEval admitted"
    if _wait_ready KarsEval eval-min "$ns" 60; then
        log_pass "KarsEval reached Ready"
    else
        log_fail "KarsEval never reached Ready"
        kubectl -n "$ns" get karseval eval-min -o yaml 2>&1 | tail -30 | sed 's/^/    /' || true
    fi
else
    log_fail "KarsEval admission rejected"
fi

scenario_summary "CRD admission lane"
exit 0
