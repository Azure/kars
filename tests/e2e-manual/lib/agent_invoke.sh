#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Per-runtime "send one prompt, get one reply" helper.
#
# Per the discovery in plan.md only OpenClaw exposes an in-pod HTTP API
# (the gateway on 127.0.0.1:18789). The other six runtimes are
# stdin/stdout — the agent process IS the entrypoint and we can't push
# new input into a long-running pod without re-exec'ing the runtime.
#
# Strategy:
#   - openclaw  → kubectl port-forward 18789, POST a chat request to the
#                 OpenClaw gateway, parse the assistant reply.
#   - others    → tail container logs for evidence the boot-time default
#                 agent successfully called the router and got a reply.
#                 Each default-agent prints its result to stdout on
#                 startup; if it shows up in pod logs, the runtime →
#                 router → Foundry path works.
#
# Callers:
#   agent_invoke <runtime> <pod_ns> <pod>
#
# Returns 0 on PASS, 1 on FAIL. Caller is expected to log_pass / log_fail
# based on the return code (so scenarios can wrap with metric_finish).

# OpenClaw gateway local port for the forward (chosen to not collide
# with router_pf_open's 18743 default).
_OPENCLAW_LOCAL_PORT="${AZURECLAW_E2E_OPENCLAW_PORT:-18790}"

_invoke_openclaw() {
    local pod_ns="$1"
    local pod="$2"

    kubectl -n "$pod_ns" port-forward "pod/${pod}" \
        "${_OPENCLAW_LOCAL_PORT}:18789" >/tmp/_openclaw-pf.log 2>&1 &
    local pf=$!
    # shellcheck disable=SC2064
    trap "kill ${pf} 2>/dev/null || true" RETURN

    local i
    for i in $(seq 1 30); do
        if curl -sS -o /dev/null --max-time 1 \
                "http://127.0.0.1:${_OPENCLAW_LOCAL_PORT}/healthz" 2>/dev/null; then
            break
        fi
        sleep 0.5
    done

    # OpenClaw gateway exposes an OpenAI-shaped chat endpoint on the
    # default model. The shape follows OpenAI chat-completions; the
    # gateway routes to azure-openai via the loopback router.
    local body='{"messages":[{"role":"user","content":"Reply with the single word PONG."}],"max_tokens":8}'
    local code
    code=$(curl -sS -o /tmp/_agent-reply.json -w '%{http_code}' --max-time 60 \
        -H 'content-type: application/json' \
        -X POST "http://127.0.0.1:${_OPENCLAW_LOCAL_PORT}/v1/chat/completions" \
        --data "${body}" 2>/dev/null || echo "000")

    if [[ "${code}" != "200" ]]; then
        echo "openclaw chat returned HTTP ${code}; body:" >&2
        head -c 800 /tmp/_agent-reply.json >&2
        return 1
    fi

    local reply
    reply=$(python3 -c "
import json, sys
try:
    d = json.load(open('/tmp/_agent-reply.json'))
    print((d.get('choices', [{}])[0].get('message', {}).get('content') or '').strip())
except Exception as e:
    print('', file=sys.stderr)
    sys.exit(0)
" 2>/dev/null)

    if [[ -z "${reply}" ]]; then
        echo "openclaw replied 200 but body had no choices[0].message.content" >&2
        head -c 400 /tmp/_agent-reply.json >&2
        return 1
    fi
    echo "openclaw reply: ${reply}"
    return 0
}

# For non-openclaw runtimes the default-agent runs at pod startup and
# prints its output to stdout. We grep container logs for a known
# "the agent ran and got a reply" marker. Each default-agent emits a
# line on success — the exact wording differs per runtime so we accept
# any non-empty assistant-style line that's *not* an exception trace.
_invoke_via_logs() {
    local runtime="$1"
    local pod_ns="$2"
    local pod="$3"

    local container
    case "$runtime" in
        oai-agents|openai-agents) container="openai-agents" ;;
        anthropic)                container="anthropic" ;;
        maf-python)               container="maf-python" ;;
        langgraph)                container="langgraph" ;;
        langgraph-typescript)     container="langgraph-ts" ;;
        pydantic-ai)              container="pydantic-ai" ;;
        *)                        container="" ;;
    esac

    # Containers in adapter pods inherit the runtime image's main
    # process — the CR-defined name is what the controller assigns.
    # Try the canonical name first, fall back to the runtime-named
    # container, then any container that isn't inference-router.
    local logs=""
    for c in "$container" "agent" "openclaw" ""; do
        if [[ -n "$c" ]]; then
            logs=$(kubectl -n "$pod_ns" logs "pod/${pod}" -c "$c" --tail=200 2>/dev/null || true)
        else
            logs=$(kubectl -n "$pod_ns" logs "pod/${pod}" --all-containers --tail=200 2>/dev/null || true)
        fi
        [[ -n "$logs" ]] && break
    done

    if [[ -z "$logs" ]]; then
        echo "no container logs available for runtime ${runtime}" >&2
        return 1
    fi

    # Strip ANSI then look for any non-trivial text line that isn't
    # a Python traceback / exception / kubernetes/docker noise.
    local ok_line
    ok_line=$(printf '%s\n' "$logs" \
        | sed 's/\x1b\[[0-9;]*[a-zA-Z]//g' \
        | grep -v -E '^(Traceback|  File |Error|raise |[[:space:]]*at |^$)' \
        | grep -E '[A-Za-z]{4,}' \
        | tail -5)

    if [[ -z "$ok_line" ]]; then
        echo "${runtime}: no plausible reply in pod logs (last 200 lines):" >&2
        printf '%s\n' "$logs" | tail -20 >&2
        return 1
    fi

    echo "${runtime} startup output: $(printf '%s' "$ok_line" | tail -1)"
    return 0
}

agent_invoke() {
    local runtime="$1"
    local pod_ns="$2"
    local pod="$3"
    case "$runtime" in
        openclaw)
            _invoke_openclaw "$pod_ns" "$pod"
            ;;
        *)
            _invoke_via_logs "$runtime" "$pod_ns" "$pod"
            ;;
    esac
}
