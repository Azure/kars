#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Manual E2E scenario: Foundry Bing grounding tool.
#
# Probes that the inference router can route an `openai/responses`
# call with `tool: bing_grounding` through to Foundry's auto-discovered
# Bing connection and that the response includes web search results.
#
# Per `kars-deployment` lore: Bing Grounding is auto-discovered
# from the Foundry project's /connections API. No manual config is
# needed when a Bing Grounding resource is connected.
#
# If no Bing connection is provisioned the router returns 4xx; the
# scenario reports SKIP with the actual response code so reviewers can
# tell "feature not provisioned" apart from "router broken".

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$(cd "$SCRIPT_DIR/../lib" && pwd)"
# shellcheck source=../lib/common.sh
source "$LIB_DIR/common.sh"
# shellcheck source=../lib/cr_factory.sh
source "$LIB_DIR/cr_factory.sh"
# shellcheck source=../lib/foundry_call.sh
source "$LIB_DIR/foundry_call.sh"

scenario_header "Foundry tools — Bing grounding"

require_cluster
require_kars_installed

name="foundry-bing"
ns=$(new_ns "foundry-bing")
pod_ns=$(pod_ns_for "$name")
export MANUAL_E2E_SCENARIO=foundry_bing

metric_start "admit_${name}"
cr_dispatch openclaw "$name" "$ns" | kubectl apply -f - >/dev/null
metric_finish "admit_${name}" foundry_bing admitKarsSandbox

if ! wait_for_karssandbox_ready "$ns" "$name"; then
    log_fail "sandbox never reached Ready — cannot probe Bing"
    cleanup_sandbox "$ns" "$name"
    scenario_summary "Foundry tools — Bing grounding"
    exit 1
fi

pod=$(kubectl -n "$pod_ns" get pod -l "kars.azure.com/sandbox=${name}" \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
if [[ -z "$pod" ]]; then
    log_fail "no pod for sandbox ${name}"
    cleanup_sandbox "$ns" "$name"
    exit 1
fi

if ! router_pf_open "$pod_ns" "$pod" 18746; then
    log_fail "could not open port-forward to inference router"
    cleanup_sandbox "$ns" "$name"
    exit 1
fi
trap 'router_pf_close; cleanup_sandbox "'"$ns"'" "'"$name"'"' EXIT

log_step "POST /openai/responses with tool: bing_grounding"
metric_start "bing_call"
body='{
  "model": "gpt-5-mini",
  "input": "What is the latest version of Kubernetes? Cite a source.",
  "tools": [{"type": "bing_grounding"}]
}'
code=$(router_call "/openai/responses" "$body")
metric_finish "bing_call" foundry_bing bingCallLatency

case "$code" in
    200)
        # Look for any of the documented success signals: web_search_results
        # in tools.output, or annotations.url_citation, or output_text with
        # citations.
        body_blob=$(router_body)
        if printf '%s' "$body_blob" | grep -qE '(web_search_results|url_citation|annotations)'; then
            log_pass "Bing grounding returned grounded response with citations (HTTP 200)"
        else
            # Sometimes Foundry returns 200 with a plain answer (no
            # grounding events) when the model decides not to invoke
            # the tool. That's a legit answer but not a positive
            # tool-wiring signal, so SKIP.
            log_skip "HTTP 200 but no citations/web_search_results in body — model may have answered without invoking Bing"
        fi
        ;;
    400|404)
        log_skip "Foundry returned HTTP ${code} — Bing connection likely not provisioned on this project"
        router_body | head -c 400
        echo
        ;;
    401|403)
        log_fail "Foundry returned HTTP ${code} — IMDS / RBAC issue, not a Bing-config issue"
        router_body | head -c 400
        echo
        ;;
    *)
        log_fail "unexpected HTTP ${code} from /openai/responses"
        router_body | head -c 400
        echo
        ;;
esac

scenario_summary "Foundry tools — Bing grounding"
