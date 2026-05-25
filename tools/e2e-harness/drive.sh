#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# drive.sh — generic end-to-end harness driver.
#
# This is the scenario- and platform-agnostic outer loop. It selects:
#   * a scenario directory under scenarios/$SCENARIO/, which carries the
#     manifests, the prompt text, the per-scenario knobs in config.sh,
#     and the verify checks module.
#   * a platform helper under platforms/$PLATFORM.sh, which knows how
#     to bring the runtime up, apply the manifests, post the prompt,
#     and tear the runtime down.
#
# Inputs (env):
#   SCENARIO  — scenario name (default: exec-brief)
#   PLATFORM  — aks | local-k8s | docker (default: aks)
#   OUT_DIR   — capture directory (default: out/<UTC timestamp>)
#   WATCHDOG_SECS — overall prompt watchdog in seconds; if unset, falls
#                   back to the scenario's SCENARIO_WATCHDOG_SECS.
#   TELEGRAM_BOT_TOKEN — optional; if set, mounted as a sandbox Secret
#                        so the Telegram channel plugin is active.
#
# Exit codes:
#   0 — happy path
#   1 — preflight failure
#   2 — apply failed
#   3 — sandbox never became Ready
#   4 — prompt timed out without a final reply
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

SCENARIO="${SCENARIO:-exec-brief}"
PLATFORM="${PLATFORM:-aks}"

SCENARIO_DIR="${SCRIPT_DIR}/scenarios/${SCENARIO}"
PLATFORM_HELPER="${SCRIPT_DIR}/platforms/${PLATFORM}.sh"

if [ ! -d "${SCENARIO_DIR}" ]; then
    echo "ERR scenario directory not found: ${SCENARIO_DIR}" >&2
    echo "     available: $(ls "${SCRIPT_DIR}/scenarios" 2>/dev/null | tr '\n' ' ')" >&2
    exit 1
fi
if [ ! -f "${PLATFORM_HELPER}" ]; then
    echo "ERR platform helper not found: ${PLATFORM_HELPER}" >&2
    echo "     available: $(ls "${SCRIPT_DIR}/platforms" 2>/dev/null | tr '\n' ' ')" >&2
    exit 1
fi

# ─── Load scenario config ────────────────────────────────────────────────────
# Defaults; the scenario's config.sh can override.
SCENARIO_SANDBOX="${SCENARIO}"
SCENARIO_SUB_SANDBOXES=()
SCENARIO_PROMPT_FILE="prompt.txt"
SCENARIO_WATCHDOG_SECS=1500
SCENARIO_INCOMING_SANDBOX=""
SCENARIO_INCOMING_PATH=""
# shellcheck disable=SC1090
[ -f "${SCENARIO_DIR}/config.sh" ] && source "${SCENARIO_DIR}/config.sh"

PROMPT_FILE="${SCENARIO_DIR}/${SCENARIO_PROMPT_FILE}"
MANIFESTS_DIR="${SCENARIO_DIR}/manifests"
if [ ! -f "${PROMPT_FILE}" ]; then
    echo "ERR scenario prompt not found: ${PROMPT_FILE}" >&2
    exit 1
fi

WATCHDOG_SECS="${WATCHDOG_SECS:-${SCENARIO_WATCHDOG_SECS}}"

OUT_DIR="${OUT_DIR:-${SCRIPT_DIR}/out/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "${OUT_DIR}"
# Keep a symlink to the latest run so verify.py's default OUT_DIR works.
ln -sfn "${OUT_DIR}" "${SCRIPT_DIR}/out/latest" 2>/dev/null || true

# RFC3339 timestamp captured BEFORE manifests are applied, so post-hoc
# `kubectl logs --since-time=…` windowed log collection covers the full
# run (apply → ready → prompt → response) and excludes noise from
# previous runs sharing the same long-lived pods.
RUN_START_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Export everything platform helpers need.
export SCRIPT_DIR REPO_ROOT SCENARIO PLATFORM SCENARIO_DIR MANIFESTS_DIR \
       PROMPT_FILE SCENARIO_SANDBOX WATCHDOG_SECS OUT_DIR RUN_START_TS \
       SCENARIO_INCOMING_SANDBOX SCENARIO_INCOMING_PATH

log() { printf '[drive %s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }
export -f log

# ─── Watchdog ────────────────────────────────────────────────────────────────
# Resolve a portable watchdog. macOS lacks GNU `timeout` by default; many
# users have `gtimeout` via `brew install coreutils`. Fall back to a
# python-based watchdog so the harness still works on a bare macOS.
resolve_timeout_cmd() {
    if command -v timeout >/dev/null 2>&1; then
        echo "timeout"
    elif command -v gtimeout >/dev/null 2>&1; then
        echo "gtimeout"
    else
        echo ""
    fi
}
export -f resolve_timeout_cmd

run_with_watchdog() {
    local secs="$1"; shift
    local tcmd
    tcmd="$(resolve_timeout_cmd)"
    if [ -n "${tcmd}" ]; then
        "${tcmd}" "${secs}s" "$@"
        return $?
    fi
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
export -f run_with_watchdog

# ─── Source the platform helper ──────────────────────────────────────────────
# The platform helper MUST define the following functions:
#   platform_preflight     — verify the platform is reachable & has CRDs/services
#   platform_apply         — apply the scenario manifests (or equivalent)
#   platform_credentials   — write any per-scenario secrets (e.g. Telegram)
#   platform_wait_for_sandbox — wait for the sandbox to be Ready
#   platform_post_prompt   — POST the prompt and tee the response into OUT_DIR
#   platform_collect_artifacts — harvest in-pod logs/files for verify.py
# A helper may stub any of these as no-ops (e.g. `platform_credentials` on
# docker if the scenario doesn't need channel secrets).
# shellcheck disable=SC1090
source "${PLATFORM_HELPER}"

for fn in platform_preflight platform_apply platform_credentials \
          platform_wait_for_sandbox platform_post_prompt \
          platform_collect_artifacts; do
    if ! declare -F "${fn}" >/dev/null; then
        echo "ERR platform helper ${PLATFORM_HELPER} missing function ${fn}()" >&2
        exit 1
    fi
done

# ─── Run ─────────────────────────────────────────────────────────────────────
log "scenario=${SCENARIO} platform=${PLATFORM} out=${OUT_DIR}"
log "scenario sandbox=${SCENARIO_SANDBOX} subs=(${SCENARIO_SUB_SANDBOXES[*]:-})"

platform_preflight
platform_apply
# credentials run after apply so the sandbox namespace exists from 00-*.
platform_credentials
platform_wait_for_sandbox
platform_post_prompt
platform_collect_artifacts
log "driver done — OUT_DIR=${OUT_DIR}"
