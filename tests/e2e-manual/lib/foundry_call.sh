#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Shared helper for talking to the inference-router sidecar through a
# kubectl port-forward. The router exposes:
#   - inference: /v1/chat/completions, /v1/responses, /anthropic/v1/messages
#   - foundry:   /openai/responses, /memory_stores, /agents, …
#   - governance:/agt/evaluate, /agt/trust/<id>, /agt/audit, …
#   - mesh:      /agt/relay (WS), /agt/registry/*, /agt/mesh/inbox
#   - egress:    /egress/{learn,enforce,allowlist,approve,deny}
#
# Scenarios call:
#   router_pf_open <pod_ns> <pod>          # opens port-forward, sets ROUTER_PORT
#   router_pf_close                        # tears down (also called from EXIT)
#   router_call    <path> <body> [method]  # POST by default; writes body to /tmp/_body.json, prints HTTP code
#   router_get     <path>                  # GET
#   router_body                            # cat /tmp/_body.json
#   router_body_jq <jq_filter>             # jq the response body
#
# The forwarder writes its log to /tmp/_router-pf.log so scenarios can
# attach failure context.

# shellcheck disable=SC2034  # ROUTER_PORT/_PID are consumed by callers
ROUTER_PORT=""
ROUTER_PF_PID=""

router_pf_open() {
    local pod_ns="$1"
    local pod="$2"
    local local_port="${3:-18743}"
    ROUTER_PORT="${local_port}"
    kubectl -n "$pod_ns" port-forward "pod/${pod}" \
        "${ROUTER_PORT}:8443" >/tmp/_router-pf.log 2>&1 &
    ROUTER_PF_PID=$!
    # Wait for the listener; the router takes ≤2s on a warm pod.
    local i
    for i in $(seq 1 30); do
        if curl -sS -o /dev/null --max-time 1 \
                "http://127.0.0.1:${ROUTER_PORT}/healthz" 2>/dev/null; then
            return 0
        fi
        sleep 0.5
    done
    echo "router_pf_open: forwarder never came up; tail of pf log:" >&2
    tail -20 /tmp/_router-pf.log >&2 || true
    return 1
}

router_pf_close() {
    if [[ -n "${ROUTER_PF_PID}" ]]; then
        kill "${ROUTER_PF_PID}" 2>/dev/null || true
        wait "${ROUTER_PF_PID}" 2>/dev/null || true
        ROUTER_PF_PID=""
    fi
}

router_call() {
    local path="$1"
    local body="$2"
    local method="${3:-POST}"
    curl -sS -o /tmp/_body.json -w '%{http_code}' --max-time 30 \
        -H 'content-type: application/json' \
        -X "${method}" "http://127.0.0.1:${ROUTER_PORT}${path}" \
        --data "${body}" 2>/dev/null || echo "000"
}

router_get() {
    local path="$1"
    curl -sS -o /tmp/_body.json -w '%{http_code}' --max-time 30 \
        "http://127.0.0.1:${ROUTER_PORT}${path}" 2>/dev/null || echo "000"
}

router_body() {
    cat /tmp/_body.json 2>/dev/null || true
}

router_body_jq() {
    local filter="$1"
    if command -v jq >/dev/null 2>&1; then
        jq -r "$filter" /tmp/_body.json 2>/dev/null || true
    else
        python3 -c "
import json, sys
try:
    d = json.load(open('/tmp/_body.json'))
    # Very small jq-shim: support .a.b.c, .a[0].b, .a // \"\"
    f = sys.argv[1].lstrip('.')
    parts = f.split('.')
    cur = d
    for p in parts:
        if '[' in p:
            k, idx = p[:p.index('[')], int(p[p.index('[')+1:p.index(']')])
            cur = cur[k][idx] if k else cur[idx]
        else:
            cur = cur.get(p, '')
    print(cur if cur is not None else '')
except Exception:
    print('')
" "$filter" 2>/dev/null || true
    fi
}

# Returns the boolean value of `.allowed` from the body of the most
# recent router_call. Empty string if the body wasn't JSON.
last_body_allowed() {
    router_body_jq '.allowed'
}
