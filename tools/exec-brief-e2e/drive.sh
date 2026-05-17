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
    log "posting executive-brief prompt to ${SANDBOX_NAME} (kubectl exec → openclaw agent)"
    # `azureclaw connect` is a port-forward + browser-open flow with no
    # stdin pipe mode — it never accepted `--no-tty` and was never the
    # right vehicle for an unattended harness. We drive the agent the
    # same way sub-agent spawn does: `kubectl exec` into the openclaw
    # container and call the local CLI in one-shot mode
    # (`openclaw agent --local -m "<prompt>" --session-id <fresh>`),
    # which sends a single message, prints the agent's full response to
    # stdout, and exits.
    #
    # We copy the prompt into the pod via stdin and read it from there
    # to avoid shell-quoting the entire multi-paragraph prompt as a CLI
    # argument (it contains backticks, quotes, and newlines that would
    # be a quoting nightmare).
    local ns="azureclaw-${SANDBOX_NAME}"
    local session_id="execbrief-$(date -u +%Y%m%dT%H%M%SZ)"
    log "session_id=${session_id}"
    # shellcheck disable=SC2002
    run_with_watchdog "${WATCHDOG_SECS}" \
        kubectl exec -i -n "${ns}" "deploy/${SANDBOX_NAME}" -c openclaw -- \
            sh -c "cat > /tmp/exec-brief-prompt.txt && \
                   openclaw agent --agent main --local \
                       --session-id '${session_id}' \
                       -m \"\$(cat /tmp/exec-brief-prompt.txt)\"" \
        < "${PROMPT_FILE}" \
        | tee "${OUT_DIR}/transcript.log"
    local rc=${PIPESTATUS[0]}
    if [ "${rc}" -eq 124 ]; then
        log "ERR prompt timed out after ${WATCHDOG_SECS}s"; exit 4
    elif [ "${rc}" -ne 0 ]; then
        log "ERR prompt exited rc=${rc}"; exit 4
    fi
    log "prompt completed"
}

main() {
    preflight
    apply_scenarios
    # credentials must run AFTER apply (so the namespace exists from 00-)
    credentials
    wait_for_sandbox
    post_prompt
    log "driver done — OUT_DIR=${OUT_DIR}"
}

main "$@"
