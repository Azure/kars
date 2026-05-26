#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Manual E2E scenario: inference smoke (T1 — runtime → router → Foundry).
#
# This is the marquee path the README sells: an agent inside a sandbox
# pod calls a model and gets a response, with the inference router
# brokering credentials so the agent never sees an API key.
#
# We exercise it three ways:
#
#   1. inf-chat-openclaw  — port-forward the OpenClaw gateway (:18789)
#                            and POST /v1/chat/completions. Asserts a
#                            non-empty assistant reply.
#   2. inf-chat-others    — for each of maf-python / langgraph /
#                            oai-agents (representative non-OpenClaw
#                            runtimes), tail container logs for
#                            evidence the boot-time default-agent
#                            successfully called the router.
#   3. inf-anthropic-shape — port-forward the inference router and POST
#                            /v1/messages (Anthropic shape). Asserts the
#                            response has the expected `content[0].type
#                            == "text"` envelope.
#
# Probe (3) does not require a specific runtime (it talks to the
# router directly), but we co-locate it on the OpenClaw sandbox to
# re-use its router sidecar.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="$(cd "$SCRIPT_DIR/../lib" && pwd)"
# shellcheck source=../lib/common.sh
source "$LIB_DIR/common.sh"
# shellcheck source=../lib/cr_factory.sh
source "$LIB_DIR/cr_factory.sh"
# shellcheck source=../lib/foundry_call.sh
source "$LIB_DIR/foundry_call.sh"
# shellcheck source=../lib/agent_invoke.sh
source "$LIB_DIR/agent_invoke.sh"

scenario_header "Inference smoke — runtime → router → Foundry"

require_cluster
require_kars_installed

export MANUAL_E2E_SCENARIO=inference_smoke

# Comma-separated list of runtimes to probe. OpenClaw is mandatory
# (only runtime with an in-pod HTTP API); the others share the
# log-tail probe.
runtimes_csv="${KARS_E2E_INF_RUNTIMES:-openclaw,maf-python,langgraph,oai-agents}"
IFS=',' read -r -a runtimes <<<"$runtimes_csv"

declare -A pod_ns_of pod_of cr_ns_of
created_karssandboxes=()

# ── Apply each sandbox in parallel-ish (admission only blocks briefly).
for runtime in "${runtimes[@]}"; do
    name="inf-${runtime//[._]/-}"
    cr_ns=$(new_ns "inf-${runtime//[._]/-}")
    cr_ns_of["$runtime"]="$cr_ns"
    pod_ns_of["$runtime"]=$(pod_ns_for "$name")
    metric_start "admit_${name}"
    if cr_dispatch "$runtime" "$name" "$cr_ns" | kubectl apply -f - >/dev/null; then
        metric_finish "admit_${name}" inference_smoke admitKarsSandbox \
            "runtime=${runtime}" "sandbox=${name}"
        log_pass "${runtime}: KarsSandbox admitted"
        created_karssandboxes+=("$runtime|$name|$cr_ns")
    else
        log_fail "${runtime}: admission rejected"
    fi
done

# ── Wait for each Ready, then probe.
for entry in "${created_karssandboxes[@]}"; do
    IFS='|' read -r runtime name cr_ns <<<"$entry"
    pod_ns="${pod_ns_of[$runtime]}"

    if ! wait_for_karssandbox_ready "$cr_ns" "$name"; then
        log_fail "${runtime}: never reached Ready, skipping probes"
        continue
    fi

    pod=$(kubectl -n "$pod_ns" get pod \
        -l "kars.azure.com/sandbox=${name}" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
    if [[ -z "$pod" ]]; then
        log_fail "${runtime}: KarsSandbox=Ready but no pod (likely controller misclassification)"
        continue
    fi
    pod_of["$runtime"]="$pod"

    log_step "[inf-chat] ${runtime}: send prompt → expect assistant reply"
    metric_start "inf_${runtime}"
    # OpenClaw needs the openclaw container reachable via PF — that's
    # not blocked by the exec-ban policy because port-forward goes
    # against the kubelet, not exec. Other runtimes use log tail
    # (also unblocked).
    if agent_invoke "$runtime" "$pod_ns" "$pod"; then
        metric_finish "inf_${runtime}" inference_smoke ttfrInference \
            "runtime=${runtime}"
        log_pass "${runtime}: inference round-trip succeeded"
    else
        log_fail "${runtime}: inference round-trip failed"
    fi
done

# ── Probe (3): /v1/messages on the OpenClaw sandbox's router sidecar.
oc_runtime="openclaw"
oc_pod_ns="${pod_ns_of[$oc_runtime]:-}"
oc_pod="${pod_of[$oc_runtime]:-}"
if [[ -n "$oc_pod_ns" && -n "$oc_pod" ]]; then
    log_step "[inf-anthropic-shape] POST /v1/messages → expect content[0].type=text"
    if router_pf_open "$oc_pod_ns" "$oc_pod" 18745; then
        # shellcheck disable=SC2064
        trap "router_pf_close" EXIT
        body='{"model":"claude-sonnet-4-5","max_tokens":16,"messages":[{"role":"user","content":"Reply with the single word PONG."}]}'
        code=$(router_call "/anthropic/v1/messages" "$body")
        type=$(router_body_jq '.content[0].type')
        if [[ "$code" == "200" && "$type" == "text" ]]; then
            log_pass "Anthropic-shape /v1/messages returned text content (HTTP 200)"
        elif [[ "$code" == "200" ]]; then
            log_skip "Anthropic-shape returned 200 but content[0].type='${type}' — model may not be available on this Foundry project"
        else
            log_fail "Anthropic-shape /v1/messages HTTP=${code}, body head:"
            router_body | head -c 400
            echo
        fi
        router_pf_close
        trap - EXIT
    else
        log_skip "Anthropic-shape: could not port-forward router"
    fi
else
    log_skip "Anthropic-shape: OpenClaw sandbox not available"
fi

# ── Cleanup all admitted sandboxes.
for entry in "${created_karssandboxes[@]}"; do
    IFS='|' read -r runtime name cr_ns <<<"$entry"
    cleanup_sandbox "$cr_ns" "$name"
done

scenario_summary "Inference smoke"
