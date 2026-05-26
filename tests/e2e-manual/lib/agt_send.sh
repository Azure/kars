#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# AGT mesh send/receive helper.
#
# Path A (the only path implemented today): drive the OpenClaw mesh
# plugin via its control socket. The plugin exposes a `mesh_send` route
# at `/mesh/send` on the unix socket /tmp/openclaw.sock inside the pod.
# Reading the inbox uses the router's HTTP route `/agt/mesh/inbox`,
# which proxies to the AGT inbox API.
#
# These helpers are exec-based, so the caller MUST `enable_break_glass`
# on the pod's namespace first (the openclaw container is normally
# blocked by ValidatingAdmissionPolicy `kars-sandbox-exec-ban`).
#
# Path B (raw relay WebSocket framing) would require a JS test harness
# to perform a real Signal-Protocol handshake — deferred to Tier C.
#
# Callers:
#   agt_mesh_send <pod_ns> <pod> <to_amid> <text>
#   agt_mesh_inbox <pod_ns> <pod> <since_unix_seconds?>
#       prints inbox JSON to stdout; returns 0 if at least one entry
#   agt_mesh_wait_for_message <pod_ns> <pod> <expect_text> [timeout_s=60]
#       polls inbox until expect_text shows up or timeout

agt_mesh_send() {
    local pod_ns="$1"
    local pod="$2"
    local to_amid="$3"
    local text="$4"

    local payload
    payload=$(printf '{"to":"%s","text":"%s"}' \
        "$(printf '%s' "$to_amid" | sed 's/"/\\"/g')" \
        "$(printf '%s' "$text"    | sed 's/"/\\"/g')")

    kubectl -n "$pod_ns" exec "pod/${pod}" -c openclaw -- sh -c "
        printf '%s' '$payload' > /tmp/_mesh-send.json
        curl -sS --unix-socket /tmp/openclaw.sock \
             -H 'content-type: application/json' \
             -X POST http://localhost/mesh/send \
             -d @/tmp/_mesh-send.json
    "
}

agt_mesh_inbox() {
    local pod_ns="$1"
    local pod="$2"
    local since="${3:-0}"

    # The router exposes the inbox as a GET on /agt/mesh/inbox; query
    # via loopback inside the pod (works regardless of whether the
    # caller has port-forwarded the router separately).
    kubectl -n "$pod_ns" exec "pod/${pod}" -c openclaw -- sh -c "
        curl -sS --max-time 10 \
             'http://127.0.0.1:8443/agt/mesh/inbox?since=${since}'
    " 2>/dev/null
}

agt_mesh_wait_for_message() {
    local pod_ns="$1"
    local pod="$2"
    local expect="$3"
    local timeout="${4:-60}"

    local deadline=$(( $(date +%s) + timeout ))
    while [[ $(date +%s) -lt $deadline ]]; do
        local inbox
        inbox=$(agt_mesh_inbox "$pod_ns" "$pod" 2>/dev/null || true)
        if printf '%s' "$inbox" | grep -q "$expect"; then
            return 0
        fi
        sleep 2
    done
    echo "agt_mesh_wait_for_message: '${expect}' never appeared in inbox of ${pod}" >&2
    return 1
}

# Confirm both peers landed in the AgentMesh registry. Reads the
# registry pod's recent log lines (assumes the registry namespace is
# 'agentmesh'); useful as an early gate before sending.
agt_registry_has_peers() {
    local peer_a="$1"
    local peer_b="$2"
    local logs
    logs=$(kubectl -n agentmesh logs deploy/registry --tail=500 2>/dev/null || true)
    printf '%s' "$logs" | grep -q "$peer_a" && \
    printf '%s' "$logs" | grep -q "$peer_b"
}
