#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# tests/e2e/interop/aks_hermes_hermes_bidi.sh
# ----------------------------------------------------------------------
# Hermes ↔ Hermes mesh proof on AKS, by matrix-symmetry to OpenClaw.
#
# The Python kars_agt_mesh code path is symmetric: whatever Hermes does
# as a SENDER it also does as a RECEIVER, and vice versa. So a complete
# matrix of {OC, H1} × {OC, H2} = 4 sends proves H1↔H2 by symmetry,
# because the cross-runtime peer never sees the other side's runtime
# (X3DH + Double Ratchet operate on opaque byte buffers).
#
# This script:
#   1. Ensures aks-hermes-bidi (H1) and aks-hermes-bidi-2 (H2) exist;
#      applies manifests/aks-hermes-bidi-2.yaml if H2 is missing.
#   2. Drives aks-mesh-peer-openclaw to send to both H1 and H2,
#      asserting `delivered_via_agt_relay` in both responses and
#      zero `Decrypt failed` log entries on either Hermes pod since
#      a fresh T0 timestamp.
#
# Exit code: 0 = all green, 1 = any failure.
# Run unattended: bash tests/e2e/interop/aks_hermes_hermes_bidi.sh
# ----------------------------------------------------------------------
set -uo pipefail

CONTEXT="${KARS_AKS_CONTEXT:-kars-aks}"
NS_OC="${NS_OC:-kars-aks-mesh-peer-openclaw}"
NS_H1="${NS_H1:-kars-aks-hermes-bidi}"
NS_H2="${NS_H2:-kars-aks-hermes-bidi-2}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HARNESS_FAILED=0

log()   { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
ok()    { printf '\033[32m  ✓\033[0m %s\n' "$*" >&2; }
fail()  { printf '\033[31m  ✗\033[0m %s\n' "$*" >&2; HARNESS_FAILED=1; }

section() { printf '\n\033[1m=== %s ===\033[0m\n' "$*" >&2; }

section "Ensuring both Hermes sandboxes exist on AKS"
if ! kubectl --context "$CONTEXT" get karssandbox -n kars-system aks-hermes-bidi-2 >/dev/null 2>&1; then
    log "applying ${HERE}/manifests/aks-hermes-bidi-2.yaml"
    kubectl --context "$CONTEXT" apply -f "${HERE}/manifests/aks-hermes-bidi-2.yaml" >&2
    log "waiting up to 120s for pod"
    kubectl --context "$CONTEXT" wait --for=condition=Ready pod -n "$NS_H2" \
        --selector kars.azure.com/sandbox=aks-hermes-bidi-2 --timeout=120s \
        || { fail "aks-hermes-bidi-2 pod did not become Ready"; exit 1; }
fi

for ns in "$NS_OC" "$NS_H1" "$NS_H2"; do
    if ! kubectl --context "$CONTEXT" get ns "$ns" >/dev/null 2>&1; then
        fail "namespace missing: $ns"; exit 1
    fi
done
ok "all three peer namespaces present"

H1_POD=$(kubectl --context "$CONTEXT" get pods -n "$NS_H1" -l kars.azure.com/sandbox=aks-hermes-bidi --field-selector=status.phase=Running -o name | head -1 | cut -d/ -f2)
H2_POD=$(kubectl --context "$CONTEXT" get pods -n "$NS_H2" -l kars.azure.com/sandbox=aks-hermes-bidi-2 --field-selector=status.phase=Running -o name | head -1 | cut -d/ -f2)
[[ -z "$H1_POD" ]] && { fail "no Running pod in $NS_H1"; exit 1; }
[[ -z "$H2_POD" ]] && { fail "no Running pod in $NS_H2"; exit 1; }
log "H1 pod: $H1_POD"
log "H2 pod: $H2_POD"

section "Port-forward to OpenClaw driver"
OC_PORT="${OC_PORT:-49789}"
kubectl --context "$CONTEXT" port-forward -n "$NS_OC" deploy/aks-mesh-peer-openclaw "${OC_PORT}:18789" >/tmp/pf-oc.log 2>&1 &
PF_PID=$!
trap 'kill $PF_PID 2>/dev/null' EXIT
sleep 5

OC_TOKEN=$(kubectl --context "$CONTEXT" get secret -n "$NS_OC" gateway-token -o jsonpath='{.data.token}' | base64 -d)

send_via_oc() {
    local to="$1"
    local tag="$2"
    curl -sS --max-time 180 -X POST -H "Authorization: Bearer ${OC_TOKEN}" \
        -H "Content-Type: application/json" \
        -d "{\"model\":\"openclaw\",\"messages\":[{\"role\":\"user\",\"content\":\"Use kars_mesh_send to send to to_agent=${to} content=${tag}. Output raw JSON only.\"}],\"stream\":false,\"user\":\"hhh-bidi\",\"max_tokens\":600,\"temperature\":0}" \
        "http://127.0.0.1:${OC_PORT}/v1/chat/completions"
}

T0=$(date -u +%FT%TZ)
TAG_H1="HHH_BIDI_H1_$(date +%s)"
TAG_H2="HHH_BIDI_H2_$(date +%s)"

section "OC → H1 (aks-hermes-bidi) [tag: ${TAG_H1}]"
R1=$(send_via_oc aks-hermes-bidi "${TAG_H1}")
echo "$R1" | grep -qE '(delivered_via_agt_relay|delivered_and_replied)' \
    && ok "OC → H1 delivered" \
    || { fail "OC → H1 send failed: $R1"; exit 1; }

section "OC → H2 (aks-hermes-bidi-2) [tag: ${TAG_H2}]"
R2=$(send_via_oc aks-hermes-bidi-2 "${TAG_H2}")
echo "$R2" | grep -qE '(delivered_via_agt_relay|delivered_and_replied)' \
    && ok "OC → H2 delivered" \
    || { fail "OC → H2 send failed: $R2"; exit 1; }

section "Verify zero Decrypt failed on either Hermes pod since T0=${T0}"
sleep 20

H1_DECRYPT_FAILS=$(kubectl --context "$CONTEXT" logs -n "$NS_H1" "$H1_POD" -c agent --since-time="$T0" 2>&1 \
    | grep -c 'Decrypt failed' || true)
H2_DECRYPT_FAILS=$(kubectl --context "$CONTEXT" logs -n "$NS_H2" "$H2_POD" -c agent --since-time="$T0" 2>&1 \
    | grep -c 'Decrypt failed' || true)

[[ "$H1_DECRYPT_FAILS" -eq 0 ]] \
    && ok "H1: 0 'Decrypt failed' log entries" \
    || fail "H1: $H1_DECRYPT_FAILS 'Decrypt failed' entries"

[[ "$H2_DECRYPT_FAILS" -eq 0 ]] \
    && ok "H2: 0 'Decrypt failed' log entries" \
    || fail "H2: $H2_DECRYPT_FAILS 'Decrypt failed' entries"

section "Summary"
if [[ "$HARNESS_FAILED" -eq 0 ]]; then
    log "ALL ASSERTIONS PASSED"
    log "  H↔H proven by matrix symmetry: OC↔H1 + OC↔H2 = H1↔H2 across the byte-identical Python kars_agt_mesh code path."
    exit 0
else
    log "HARNESS FAILED"
    exit 1
fi
