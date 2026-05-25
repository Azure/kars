#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# monitor.sh — concurrent live tail of every relevant signal during an
# e2e-harness run on a K8s platform (AKS or local-k8s). Writes one JSONL
# line per event to out/<run>/trace.jsonl AND prints a colour-coded
# timeline to stdout so a human can watch the execution unfold.
#
# This script is kubectl-based and only applicable to K8s platforms.
# For PLATFORM=docker, run.sh skips this step and the docker helper
# relies on docker-exec artifact collection instead.
#
# Tails:
#   K8S-EVT  kubectl get events -A --watch        (CRD lifecycle)
#   CTRL     azureclaw-controller logs            (reconcile decisions)
#   ROUTER   inference-router logs                (provider + AGT calls)
#   RELAY    agentmesh-relay logs                 (E2E message flow)
#   REGISTRY agentmesh-registry logs              (peer discovery)
#   POD      execbrief openclaw container logs    (agent reasoning trace)
#
# Run in parallel with drive.sh. Stops cleanly on SIGINT.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${OUT_DIR:-${SCRIPT_DIR}/out/latest}"
SANDBOX_NAME="${SANDBOX_NAME:-execbrief}"

mkdir -p "${OUT_DIR}"
TRACE="${OUT_DIR}/trace.jsonl"
: >"${TRACE}"

# Colour palette (ANSI). Disable with NO_COLOR=1.
if [ "${NO_COLOR:-}" = "1" ]; then
    C_EVT='' C_CTRL='' C_ROUTER='' C_RELAY='' C_REG='' C_POD='' C_OFF=''
else
    C_EVT=$'\033[36m'    # cyan
    C_CTRL=$'\033[35m'   # magenta
    C_ROUTER=$'\033[33m' # yellow
    C_RELAY=$'\033[32m'  # green
    C_REG=$'\033[34m'    # blue
    C_POD=$'\033[37m'    # white/dim
    C_OFF=$'\033[0m'
fi

emit() {
    # emit <source> <color> <message>
    local src="$1" color="$2" msg="$3"
    local ts
    ts="$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)"
    printf '%s%-9s %s%s\n' "${color}" "${src}" "${msg}" "${C_OFF}"
    # JSONL — single line, escaped via jq-free hand-roll (no jq dep)
    printf '{"ts":"%s","src":"%s","msg":%s}\n' \
        "${ts}" "${src}" \
        "$(printf '%s' "${msg}" | python3 -c 'import sys,json; print(json.dumps(sys.stdin.read()))')" \
        >>"${TRACE}"
}

# Pid tracking for clean shutdown.
PIDS=()
cleanup() {
    for pid in "${PIDS[@]:-}"; do
        kill "${pid}" 2>/dev/null || true
    done
    wait 2>/dev/null || true
    emit "MONITOR" "${C_OFF}" "monitor stopped — trace=${TRACE}"
}
trap cleanup EXIT INT TERM

emit "MONITOR" "${C_OFF}" "starting — sandbox=${SANDBOX_NAME} out=${OUT_DIR}"

# 1. CRD events across all azureclaw namespaces.
{
    kubectl get events -A --watch \
        --field-selector 'involvedObject.kind in (ClawSandbox,InferencePolicy,ToolPolicy,ClawMemory,McpServer)' \
        2>/dev/null \
        | while IFS= read -r line; do emit "K8S-EVT" "${C_EVT}" "${line}"; done
} &
PIDS+=($!)

# 2. Controller logs.
{
    kubectl logs -n azureclaw-system deploy/azureclaw-controller -f --tail=5 2>/dev/null \
        | while IFS= read -r line; do emit "CTRL" "${C_CTRL}" "${line}"; done
} &
PIDS+=($!)

# 3. Router logs (per-sandbox container).
{
    # the deployment doesn't exist until reconcile finishes; retry-wait
    until kubectl get deploy -n "azureclaw-${SANDBOX_NAME}" "${SANDBOX_NAME}" >/dev/null 2>&1; do
        sleep 3
    done
    kubectl logs -n "azureclaw-${SANDBOX_NAME}" "deploy/${SANDBOX_NAME}" \
        -c inference-router -f --tail=5 2>/dev/null \
        | while IFS= read -r line; do emit "ROUTER" "${C_ROUTER}" "${line}"; done
} &
PIDS+=($!)

# 4. Relay logs.
{
    kubectl logs -n agentmesh -l app=agentmesh-relay -f --tail=5 2>/dev/null \
        | while IFS= read -r line; do emit "RELAY" "${C_RELAY}" "${line}"; done
} &
PIDS+=($!)

# 5. Registry logs.
{
    kubectl logs -n agentmesh -l app=agentmesh-registry -f --tail=5 2>/dev/null \
        | while IFS= read -r line; do emit "REGISTRY" "${C_REG}" "${line}"; done
} &
PIDS+=($!)

# 6. Sandbox pod openclaw container.
{
    until kubectl get deploy -n "azureclaw-${SANDBOX_NAME}" "${SANDBOX_NAME}" >/dev/null 2>&1; do
        sleep 3
    done
    kubectl logs -n "azureclaw-${SANDBOX_NAME}" "deploy/${SANDBOX_NAME}" \
        -c openclaw -f --tail=5 2>/dev/null \
        | while IFS= read -r line; do emit "POD" "${C_POD}" "${line}"; done
} &
PIDS+=($!)

# 7. Sub-agent openclaw containers — dynamic discovery. Each sub-agent sandbox
# lands in its own azureclaw-<name> namespace. Tail each one's openclaw
# container with a source tag of POD-<name> so verify.py can attribute
# 'AGT relay: sent to X' lines to a specific sender.
SUBAGENT_NAMES=("analyst" "viz" "writer")
for sub in "${SUBAGENT_NAMES[@]}"; do
    (
        # Wait up to 10 minutes for the sub-agent deployment to appear.
        deadline=$(( $(date +%s) + 600 ))
        until kubectl get deploy -n "azureclaw-${sub}" "${sub}" >/dev/null 2>&1; do
            [ "$(date +%s)" -gt "${deadline}" ] && exit 0
            sleep 5
        done
        kubectl logs -n "azureclaw-${sub}" "deploy/${sub}" \
            -c openclaw -f --tail=5 2>/dev/null \
            | while IFS= read -r line; do emit "POD-${sub}" "${C_POD}" "${line}"; done
    ) &
    PIDS+=($!)
    (
        # Also tail the sub-agent's inference-router so foundry tool usage
        # (code_execute, image_generation, /openai/responses, /openai/containers)
        # shows up in trace.jsonl for verify.py. Without this, sub-agent
        # foundry calls are invisible and check_hero / check_chart misfire.
        deadline=$(( $(date +%s) + 600 ))
        until kubectl get deploy -n "azureclaw-${sub}" "${sub}" >/dev/null 2>&1; do
            [ "$(date +%s)" -gt "${deadline}" ] && exit 0
            sleep 5
        done
        kubectl logs -n "azureclaw-${sub}" "deploy/${sub}" \
            -c inference-router -f --tail=5 2>/dev/null \
            | while IFS= read -r line; do emit "ROUTER" "${C_ROUTER}" "${line}"; done
    ) &
    PIDS+=($!)
done

# Block until SIGINT.
wait
