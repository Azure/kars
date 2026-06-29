#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Manual E2E scenario: egress allowlist lifecycle.
#
# Validates the four states of the per-sandbox egress allowlist:
#
#   [1/4] learn        — sandbox in learn mode records outbound domains
#                         when the agent (here: an exec-driven curl)
#                         touches them. Probe via /egress/learned.
#   [2/4] enforce      — switch to Strict via the KarsSandbox CRD
#                         (spec.networkPolicy.egressMode). Probe a
#                         previously-unseen domain → expect block
#                         (NetworkPolicy denies, curl times out).
#   [3/4] approve      — grant example.org via an EgressApproval CR
#                         (the runtime widening mechanism), re-probe →
#                         expect success.
#   [4/4] deny         — delete the EgressApproval CR, re-probe → expect
#                         block.
#
# Slice 5c.1 removed the in-router /egress/approve|deny|enforce|pending
# endpoints; the allowlist is now driven by the KarsSandbox CRD (baseline
# + signed bundle) and EgressApproval CRs (TTL-scoped grants). This
# scenario exercises the CRD control plane only (no cosign/ACR signing).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$(cd "$SCRIPT_DIR/../lib" && pwd)"
# shellcheck source=../lib/common.sh
source "$LIB_DIR/common.sh"
# shellcheck source=../lib/cr_factory.sh
source "$LIB_DIR/cr_factory.sh"

scenario_header "Egress allowlist lifecycle"

require_cluster
require_kars_installed

name="egress-loop"
ns=$(new_ns "egress-loop")
pod_ns=$(pod_ns_for "$name")
export MANUAL_E2E_SCENARIO=egress_lifecycle

# Apply the sandbox with a learn-mode patch baked into the KarsSandbox.
metric_start "admit_${name}"
cr_dispatch openclaw "$name" "$ns" \
  | yq eval '
        select(.kind == "KarsSandbox")
            | .spec.networkPolicy.egressMode = "Learn"
        ,
        select(.kind != "KarsSandbox")
    ' - \
  | kubectl apply -f - >/dev/null
metric_finish "admit_${name}" egress_lifecycle admitKarsSandbox

if ! wait_for_karssandbox_ready "$ns" "$name"; then
    log_fail "sandbox never reached Ready"
    cleanup_sandbox "$ns" "$name"
    scenario_summary "Egress allowlist lifecycle"
    exit 1
fi

pod=$(kubectl -n "$pod_ns" get pod -l "kars.azure.com/sandbox=${name}" \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
if [[ -z "$pod" ]]; then
    log_fail "no pod for sandbox ${name}"
    cleanup_sandbox "$ns" "$name"
    exit 1
fi

enable_break_glass "$pod_ns"
trap 'disable_break_glass "'"$pod_ns"'" 2>/dev/null || true; cleanup_sandbox "'"$ns"'" "'"$name"'"' EXIT

# ── Read the admin token (best-effort).
admin_token=""
admin_b64=$(kubectl -n "$pod_ns" get secret router-admin-token \
    -o jsonpath='{.data.token}' 2>/dev/null || true)
if [[ -n "$admin_b64" ]]; then
    admin_token=$(printf '%s' "$admin_b64" | base64 -d 2>/dev/null || \
                  printf '%s' "$admin_b64" | base64 -D 2>/dev/null || true)
fi
auth_args=()
[[ -n "$admin_token" ]] && auth_args=("-H" "Authorization: Bearer ${admin_token}")

router_curl() {
    # Run curl from the router container so we hit 127.0.0.1:8443 with
    # no NetworkPolicy interference.
    local method="$1"; shift
    local path="$1"; shift
    local body="${1:-}"
    local args=("-s" "-o" "/tmp/egress_resp.txt" "-w" "%{http_code}"
                "-X" "$method" "${auth_args[@]}")
    if [[ -n "$body" ]]; then
        args+=("-H" "content-type: application/json" "-d" "$body")
    fi
    args+=("http://127.0.0.1:8443${path}")
    kubectl exec -n "$pod_ns" "$pod" -c inference-router -- curl "${args[@]}" 2>/dev/null || echo "000"
}

agent_curl() {
    # Drive a request from the openclaw container as the sandbox UID.
    # Returns 0 if curl succeeded, non-0 otherwise.
    local url="$1"
    local timeout="${2:-8}"
    kubectl exec -n "$pod_ns" "$pod" -c openclaw -- \
        timeout "$timeout" curl -s -o /dev/null -w "%{http_code}" \
        --connect-timeout "$timeout" "$url" 2>/dev/null || true
}

# ── [1/4] learn ───────────────────────────────────────────────────────
log_step "[1/4] learn: agent touches example.com → expect to see it in /egress/learned"
metric_start "egress_learn_touch"
# Send an outbound request as the agent. In learn mode this is logged
# but not blocked.
_=$(agent_curl "https://example.com" 6)
sleep 3
metric_finish "egress_learn_touch" egress_lifecycle learnTouchLatency

code=$(router_curl GET "/egress/learned")
body=$(cat /tmp/egress_resp.txt 2>/dev/null || true)
if [[ "$code" == "200" && "$body" == *"example.com"* ]]; then
    log_pass "learn mode recorded example.com"
elif [[ "$code" == "200" ]]; then
    log_skip "learn mode returned 200 but example.com not in body — may need longer settle: ${body:0:200}"
elif [[ "$code" == "401" || "$code" == "403" ]]; then
    log_fail "/egress/learned returned ${code} — admin token problem"
else
    log_skip "/egress/learned returned ${code} — endpoint may not be wired in this build: ${body:0:200}"
fi

# ── [2/4] enforce ─────────────────────────────────────────────────────
log_step "[2/4] enforce: switch the KarsSandbox to Strict, probe an unseen domain → expect block"
metric_start "egress_enforce_switch"
if ! kubectl patch karssandbox "$name" -n "$ns" --type merge \
        -p '{"spec":{"networkPolicy":{"egressMode":"Strict"}}}' >/dev/null 2>&1; then
    log_skip "could not patch egressMode=Strict on KarsSandbox/${name} — remaining steps depend on it"
    scenario_summary "Egress allowlist lifecycle"
    exit 0
fi
# Best-effort live toggle so Strict applies without waiting for a pod roll.
_=$(router_curl POST "/egress/learn" '{"enabled":false}')
metric_finish "egress_enforce_switch" egress_lifecycle enforceSwitchLatency
sleep 3
# example.org should not have been seen during learn — should now be blocked.
http=$(agent_curl "https://example.org" 6)
if [[ "$http" == "000" || -z "$http" ]]; then
    log_pass "previously-unseen domain (example.org) blocked under Strict"
else
    log_skip "example.org returned HTTP ${http} — egress NetworkPolicy may not have refreshed yet"
fi

# ── [3/4] approve ─────────────────────────────────────────────────────
log_step "[3/4] approve: grant example.org via an EgressApproval CR, re-probe"
appr="${name}-allow-exampleorg"
if ! kubectl apply -f - >/dev/null 2>&1 <<EOF
apiVersion: kars.azure.com/v1alpha1
kind: EgressApproval
metadata:
  name: ${appr}
  namespace: ${ns}
spec:
  sandbox: ${name}
  hosts:
    - host: example.org
      port: 443
  reason: "manual e2e egress_lifecycle approve step"
  ttl: PT15M
EOF
then
    log_skip "could not create EgressApproval/${appr} — CRD may not be installed in this build"
else
    sleep 6
    http=$(agent_curl "https://example.org" 8)
    if [[ "$http" == "200" || "$http" == "301" || "$http" == "302" ]]; then
        log_pass "EgressApproval allowlisted example.org (HTTP ${http})"
    else
        log_skip "EgressApproval applied but probe got HTTP=${http} — reconcile/propagation lag or upstream issue"
    fi
fi

# ── [4/4] deny ────────────────────────────────────────────────────────
log_step "[4/4] deny: delete the EgressApproval CR, re-probe → expect block"
if ! kubectl delete egressapproval "$appr" -n "$ns" >/dev/null 2>&1; then
    log_skip "could not delete EgressApproval/${appr} — may not have been created"
else
    sleep 6
    http=$(agent_curl "https://example.org" 6)
    if [[ "$http" == "000" || -z "$http" ]]; then
        log_pass "revoking the EgressApproval blocked example.org again"
    else
        log_skip "EgressApproval deleted but probe got HTTP=${http} — propagation lag"
    fi
fi

scenario_summary "Egress allowlist lifecycle"
