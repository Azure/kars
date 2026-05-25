#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# drive.sh — provision the exec-brief sandbox surface on the AKS cluster
# the caller is already kubectl-logged into, then post the executive-brief
# prompt to the gateway and wait for the final assembled brief.
#
# What this does NOT do:
#   - create or destroy the AKS cluster (run `azureclaw up` first)
#   - install the helm chart (run `azureclaw up` first)
#   - touch your Azure subscription
#   - log into Telegram on your behalf — bring the bot token in
#     TELEGRAM_BOT_TOKEN; if absent, telegram acceptance check is skipped.
#
# Exit codes:
#   0 — happy path
#   1 — preflight failure (missing kubectl context / required CRDs absent)
#   2 — apply failed
#   3 — sandbox never became Ready
#   4 — prompt timed out without a final brief
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCENARIOS_DIR="${SCRIPT_DIR}/scenarios"
PROMPT_FILE="${SCRIPT_DIR}/prompts/exec-brief.txt"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

OUT_DIR="${OUT_DIR:-${SCRIPT_DIR}/out/$(date -u +%Y%m%dT%H%M%SZ)}"
SANDBOX_NAME="${SANDBOX_NAME:-execbrief}"
WATCHDOG_SECS="${WATCHDOG_SECS:-1500}"  # 25 min

mkdir -p "${OUT_DIR}"

log() { printf '[drive %s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }

# ─── Preflight ───────────────────────────────────────────────────────────────
preflight() {
    command -v kubectl >/dev/null || { log "ERR kubectl not on PATH"; exit 1; }
    command -v azureclaw >/dev/null || { log "ERR azureclaw CLI not on PATH"; exit 1; }
    kubectl config current-context >/dev/null || {
        log "ERR no current kubectl context — run 'azureclaw up' first"; exit 1
    }
    for crd in clawsandboxes.azureclaw.azure.com \
               inferencepolicies.azureclaw.azure.com \
               toolpolicies.azureclaw.azure.com \
               clawmemories.azureclaw.azure.com \
               mcpservers.azureclaw.azure.com; do
        kubectl get crd "$crd" >/dev/null 2>&1 || {
            log "ERR CRD ${crd} missing — helm chart not installed"; exit 1
        }
    done
    log "preflight ok — kubectl context: $(kubectl config current-context)"
}

# ─── Credentials ─────────────────────────────────────────────────────────────
credentials() {
    if [ -n "${TELEGRAM_BOT_TOKEN:-}" ]; then
        log "creating ${SANDBOX_NAME}-credentials secret with TELEGRAM_BOT_TOKEN"
        kubectl create secret generic "${SANDBOX_NAME}-credentials" \
            --namespace "azureclaw-${SANDBOX_NAME}" \
            --from-literal=TELEGRAM_BOT_TOKEN="${TELEGRAM_BOT_TOKEN}" \
            --dry-run=client -o yaml | kubectl apply -f -
    else
        log "no TELEGRAM_BOT_TOKEN set — Telegram acceptance check will be skipped"
    fi
}

# ─── Apply ───────────────────────────────────────────────────────────────────
apply_scenarios() {
    log "applying ${SCENARIOS_DIR}/*.yaml in order"
    # 00 first so the namespace exists before secrets land.
    for f in "${SCENARIOS_DIR}"/0*.yaml; do
        log "  -> $(basename "$f")"
        kubectl apply -f "$f" >>"${OUT_DIR}/apply.log" 2>&1 || {
            tail -n 40 "${OUT_DIR}/apply.log"; exit 2
        }
    done
}

# ─── Wait for sandbox ────────────────────────────────────────────────────────
wait_for_sandbox() {
    log "waiting for ClawSandbox/${SANDBOX_NAME} → Ready (timeout 600s)"
    kubectl wait --for=condition=Ready \
        "clawsandbox/${SANDBOX_NAME}" \
        --namespace azureclaw-system \
        --timeout=600s || { log "ERR sandbox not Ready in time"; exit 3; }
    # Then for the actual deployment in azureclaw-<name>
    kubectl wait --for=condition=Available \
        "deploy/${SANDBOX_NAME}" \
        --namespace "azureclaw-${SANDBOX_NAME}" \
        --timeout=300s || { log "ERR deployment not Available in time"; exit 3; }
    log "sandbox Ready"
}

# ─── Post the prompt ─────────────────────────────────────────────────────────
# Resolve a portable watchdog. macOS lacks GNU `timeout` by default; many
# users have `gtimeout` via `brew install coreutils`. Fall back to a
# python-based watchdog so the harness still works on a bare macOS without
# brew. We avoid `perl -e 'alarm'` because some macOS perl builds drop
# alarm() in the default profile.
resolve_timeout_cmd() {
    if command -v timeout >/dev/null 2>&1; then
        echo "timeout"
    elif command -v gtimeout >/dev/null 2>&1; then
        echo "gtimeout"
    else
        echo ""
    fi
}

run_with_watchdog() {
    local secs="$1"; shift
    local tcmd
    tcmd="$(resolve_timeout_cmd)"
    if [ -n "${tcmd}" ]; then
        "${tcmd}" "${secs}s" "$@"
        return $?
    fi
    # Fallback: python-based watchdog. Exit 124 mirrors GNU timeout.
    python3 - "${secs}" "$@" <<'PY'
import os, signal, subprocess, sys
secs = int(sys.argv[1])
argv = sys.argv[2:]
proc = subprocess.Popen(argv, stdin=sys.stdin)
def kill(_signum=None, _frame=None):
    try:
        proc.send_signal(signal.SIGTERM)
    except ProcessLookupError:
        pass
signal.signal(signal.SIGALRM, lambda *_: (kill(), sys.exit(124)))
signal.alarm(secs)
sys.exit(proc.wait())
PY
}

post_prompt() {
    log "posting executive-brief prompt to ${SANDBOX_NAME} gateway (/v1/chat/completions)"
    # Operator-mode delivery, matching `azureclaw connect`'s security model:
    #
    #   1. Read the gateway-token Secret (RBAC-gated, namespaced) — same
    #      Secret the WebUI flow uses (`cli/src/commands/connect.ts:142`).
    #   2. `kubectl port-forward` deploy/<name> :18789. The exec-ban VAP
    #      does NOT block port-forward (see admission-pod-exec-ban.yaml
    #      comment "We do NOT block pods/portforward").
    #   3. POST the prompt to the gateway's OpenAI-shape
    #      `/v1/chat/completions` endpoint with `Authorization: Bearer
    #      <gateway-token>`. The gateway translates this into an
    #      `agentCommandFromIngress` and runs the full agent pipeline
    #      (plugins, AGT, mesh, sub-agent spawn) — NOT a model-only
    #      passthrough (see openai-http-CMbrDoh_.js).
    #
    # We must NOT `kubectl exec -c openclaw` — that's blocked by the
    # `azureclaw-sandbox-exec-ban` ValidatingAdmissionPolicy by design.
    local ns="azureclaw-${SANDBOX_NAME}"
    local local_port="${GATEWAY_LOCAL_PORT:-28789}"

    log "fetching gateway token from Secret ${ns}/gateway-token"
    local token_b64
    token_b64=$(kubectl get secret -n "${ns}" gateway-token \
        -o jsonpath='{.data.token}' 2>/dev/null || true)
    if [ -z "${token_b64}" ]; then
        log "ERR gateway-token Secret missing or empty in ${ns}"; exit 4
    fi
    local gateway_token
    gateway_token=$(printf '%s' "${token_b64}" | base64 -d | tr -d '\n')
    if [ -z "${gateway_token}" ]; then
        log "ERR gateway token decoded to empty string"; exit 4
    fi

    log "starting kubectl port-forward localhost:${local_port} → ${ns}/${SANDBOX_NAME}:18789"
    kubectl port-forward -n "${ns}" "deploy/${SANDBOX_NAME}" \
        "${local_port}:18789" \
        > "${OUT_DIR}/port-forward.log" 2>&1 &
    local pf_pid=$!
    # shellcheck disable=SC2064
    trap "kill ${pf_pid} 2>/dev/null || true" EXIT INT TERM

    local i=0
    while [ $i -lt 30 ]; do
        if curl -sf --max-time 1 "http://127.0.0.1:${local_port}/healthz" \
                > /dev/null 2>&1; then
            break
        fi
        i=$((i+1)); sleep 1
    done
    if [ $i -ge 30 ]; then
        log "ERR port-forward never started serving HTTP on localhost:${local_port}"
        cat "${OUT_DIR}/port-forward.log" >&2 || true
        exit 4
    fi
    log "gateway reachable at localhost:${local_port}"

    local session_id="execbrief-$(date -u +%Y%m%dT%H%M%SZ)"
    log "session_id=${session_id}"

    # Build the JSON body. Use python to JSON-encode the prompt safely
    # (preserves newlines, backticks, quotes — bash heredoc + jq -Rs would
    # also work but python avoids the jq dependency).
    local body_file="${OUT_DIR}/request.json"
    python3 - "${PROMPT_FILE}" "${session_id}" > "${body_file}" <<'PY'
import json, sys
prompt = open(sys.argv[1]).read()
session_id = sys.argv[2]
print(json.dumps({
    "model": "openclaw",  # gateway dispatches to the configured agent runtime
    "messages": [{"role": "user", "content": prompt}],
    "stream": False,
    "user": session_id,
}))
PY

    run_with_watchdog "${WATCHDOG_SECS}" \
        curl -sS --no-buffer --fail-with-body \
            -H "Authorization: Bearer ${gateway_token}" \
            -H "Content-Type: application/json" \
            --data-binary "@${body_file}" \
            "http://127.0.0.1:${local_port}/v1/chat/completions" \
        | tee "${OUT_DIR}/response.json"
    local rc=${PIPESTATUS[0]}
    kill "${pf_pid}" 2>/dev/null || true

    if [ "${rc}" -eq 124 ]; then
        log "ERR prompt timed out after ${WATCHDOG_SECS}s"; exit 4
    elif [ "${rc}" -ne 0 ]; then
        log "ERR gateway request failed rc=${rc}"; exit 4
    fi

    # Extract the assistant text into transcript.log for verify.py.
    python3 - "${OUT_DIR}/response.json" "${OUT_DIR}/transcript.log" <<'PY'
import json, sys
resp = json.load(open(sys.argv[1]))
out = open(sys.argv[2], 'w')
for choice in resp.get('choices', []):
    msg = choice.get('message', {})
    txt = msg.get('content', '')
    if isinstance(txt, list):
        txt = ''.join(p.get('text','') for p in txt if isinstance(p, dict))
    out.write(txt + '\n')
out.close()
PY
    log "prompt completed — transcript at ${OUT_DIR}/transcript.log"
}

collect_artifacts() {
    # Capture sub-agent gateway logs and writer's incoming/ listing as
    # post-run artifacts so verify.py has authoritative evidence of mesh
    # file transfers (which live in /tmp/gateway.log inside the container,
    # not in kubectl logs stdout). Uses break-glass label, removes it after.
    log "collecting post-run artifacts (writer incoming + gateway tails)"
    for ns in azureclaw-writer azureclaw-viz; do
        kubectl label namespace "${ns}" azureclaw.azure.com/break-glass=true \
            --overwrite >/dev/null 2>&1 || true
    done
    # Give the validating policy a moment to refresh.
    sleep 2

    WRITER_POD=$(kubectl get pod -n azureclaw-writer \
        -l azureclaw.azure.com/sandbox=writer -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
    VIZ_POD=$(kubectl get pod -n azureclaw-viz \
        -l azureclaw.azure.com/sandbox=viz -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")

    if [ -n "${WRITER_POD}" ]; then
        kubectl exec -n azureclaw-writer "${WRITER_POD}" -c openclaw -- \
            ls -la /sandbox/.openclaw/workspace/incoming/ 2>/dev/null \
            >"${OUT_DIR}/writer-incoming.txt" || true
        kubectl exec -n azureclaw-writer "${WRITER_POD}" -c openclaw -- \
            sh -c 'grep -E "file_transfer_ack|mesh_transfer_file" /tmp/gateway.log 2>/dev/null || true' \
            >"${OUT_DIR}/writer-gateway.log" || true
    fi
    if [ -n "${VIZ_POD}" ]; then
        kubectl exec -n azureclaw-viz "${VIZ_POD}" -c openclaw -- \
            sh -c 'grep -E "mesh_transfer_file|foundry_image_generation|downloaded_files" /tmp/gateway.log 2>/dev/null || true' \
            >"${OUT_DIR}/viz-gateway.log" || true
    fi

    # Remove break-glass labels.
    for ns in azureclaw-writer azureclaw-viz; do
        kubectl label namespace "${ns}" azureclaw.azure.com/break-glass- \
            >/dev/null 2>&1 || true
    done
    log "artifacts collected"
}

main() {
    preflight
    apply_scenarios
    # credentials must run AFTER apply (so the namespace exists from 00-)
    credentials
    wait_for_sandbox
    post_prompt
    collect_artifacts
    log "driver done — OUT_DIR=${OUT_DIR}"
}

main "$@"
