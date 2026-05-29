#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# run.sh — top-level orchestrator for the e2e harness.
# Runs monitor.sh in the background, drive.sh in the foreground, then
# verify.py once the driver returns. All artifacts land under
# tools/e2e-harness/out/<UTC-timestamp>/.
#
# Usage:
#   SCENARIO=exec-brief PLATFORM=aks ./run.sh
#   TELEGRAM_BOT_TOKEN=xxx ./run.sh                  # enables telegram check
#   WATCHDOG_SECS=2400 ./run.sh
#
# Defaults: SCENARIO=exec-brief PLATFORM=aks
# Exit code = verify.py's exit code (0 = all scenario checks pass).
set -euo pipefail

SCENARIO="${SCENARIO:-exec-brief}"
PLATFORM="${PLATFORM:-aks}"
export SCENARIO PLATFORM

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="${SCRIPT_DIR}/out/${RUN_ID}"
mkdir -p "${OUT_DIR}"
# Maintain a 'latest' symlink so verify/monitor can find the active run.
ln -sfn "${RUN_ID}" "${SCRIPT_DIR}/out/latest"

export OUT_DIR

echo "[run] RUN_ID=${RUN_ID}  SCENARIO=${SCENARIO}  PLATFORM=${PLATFORM}"
echo "[run] OUT_DIR=${OUT_DIR}"

# Source the scenario config so monitor.sh (which still expects
# SANDBOX_NAME) sees the scenario's parent sandbox identifier.
SCENARIO_DIR="${SCRIPT_DIR}/scenarios/${SCENARIO}"
if [ -f "${SCENARIO_DIR}/config.sh" ]; then
    # shellcheck disable=SC1091
    source "${SCENARIO_DIR}/config.sh"
    export SANDBOX_NAME="${SCENARIO_SANDBOX:-${SANDBOX_NAME:-}}"
fi

# monitor.sh is currently kubectl-based (AKS / local-k8s). On docker we
# rely on docker-exec artifact collection in platform_collect_artifacts.
MONITOR_PID=""
if [ "${PLATFORM}" != "docker" ]; then
    # Background monitor. Wrap in a subshell so $! is the subshell PID, which is
    # the parent of the entire `monitor.sh | tee` pipeline — we can then kill the
    # subshell *and* its children together via the process group.
    # In demo mode, the formatter renders the narrative — silence the
    # raw monitor stream to the terminal but still capture it to disk.
    if [ "${DEMO:-0}" = "1" ]; then
        ( "${SCRIPT_DIR}/monitor.sh" >"${OUT_DIR}/monitor.log" 2>&1 ) &
    else
        ( "${SCRIPT_DIR}/monitor.sh" 2>&1 | tee "${OUT_DIR}/monitor.log" ) &
    fi
    MONITOR_PID=$!
fi

# DEMO=1 — render a clean storyboard view to stdout instead of the raw
# monitor stream. The raw drive.log + monitor.log + trace.jsonl still
# accumulate in OUT_DIR for verify.py and post-mortem inspection; the
# formatter only changes what the operator sees live during recording.
# Start AFTER monitor.sh so trace.jsonl is being written by the time
# the formatter picks it up.
DEMO_PID=""
if [ "${DEMO:-0}" = "1" ]; then
    # format_demo.py tails drive.log + trace.jsonl and renders one-line
    # milestones with evidence. Run in the background; its stdout
    # becomes the demo view interleaved with run.sh's setup lines.
    sleep 1  # let monitor.sh create trace.jsonl
    ( python3 "${SCRIPT_DIR}/format_demo.py" "${OUT_DIR}" ) &
    DEMO_PID=$!
fi

cleanup() {
    if [ -n "${MONITOR_PID}" ] && kill -0 "${MONITOR_PID}" 2>/dev/null; then
        # Kill the subshell process group first (covers monitor.sh + tee + any
        # kubectl logs -f grandchildren spawned by monitor.sh).
        kill -TERM "-${MONITOR_PID}" 2>/dev/null || true
        kill "${MONITOR_PID}" 2>/dev/null || true
        # Belt-and-braces: explicitly hunt any stray kubectl logs -f processes
        # that escaped (macOS sandboxes forbid pkill/killall).
        for pid in $(ps -o pid=,command= | awk '/kubectl[[:space:]]+logs[[:space:]]+-f/ {print $1}'); do
            kill "${pid}" 2>/dev/null || true
        done
    fi
    if [ -n "${DEMO_PID}" ] && kill -0 "${DEMO_PID}" 2>/dev/null; then
        # SIGINT lets the demo formatter print its summary cleanly.
        kill -INT "${DEMO_PID}" 2>/dev/null || true
    fi
    # Hunt any in-flight kubectl port-forward / kubectl wait / kubectl
    # apply children of this run. macOS BSD pkill/killall would be
    # easier but is sandbox-forbidden, so iterate via ps.
    for pid in $(ps -o pid=,command= | awk '/kubectl[[:space:]]+(port-forward|wait|logs)/ {print $1}'); do
        kill "${pid}" 2>/dev/null || true
    done
    sleep 1
    # Last-resort SIGKILL for anything still alive.
    [ -n "${MONITOR_PID}" ] && kill -9 "${MONITOR_PID}" 2>/dev/null || true
    [ -n "${DEMO_PID}" ] && kill -9 "${DEMO_PID}" 2>/dev/null || true
}

# Explicit exit in the INT/TERM trap — without `exit`, bash runs the
# trap then continues the script as if nothing happened (e.g. sleeps
# resume, the next command runs). That's why Ctrl+C felt unresponsive.
trap 'cleanup; exit 130' INT TERM
trap cleanup EXIT

# Foreground driver (provision + drive prompt). With DEMO=1 we suppress
# the raw drive output from the terminal — the formatter shows the
# narrative — but keep tee'ing to drive.log for verify.py.
set +e
if [ "${DEMO:-0}" = "1" ]; then
    "${SCRIPT_DIR}/drive.sh" >"${OUT_DIR}/drive.log" 2>&1
else
    "${SCRIPT_DIR}/drive.sh" 2>&1 | tee "${OUT_DIR}/drive.log"
fi
DRIVE_RC=$?
set -e

# Give the monitor + formatter a beat to flush the last log lines.
sleep 5

if [ "${DRIVE_RC}" -ne 0 ]; then
    echo "[run] driver exited rc=${DRIVE_RC}, skipping verify"
    cleanup
    exit "${DRIVE_RC}"
fi

# Verify. In DEMO mode, suppress verify.py's raw stdout — the
# formatter polls verify.json and renders the check panel itself.
# Run verify.py BEFORE cleaning up the formatter so the formatter
# is still alive to pick up verify.json.
if [ "${DEMO:-0}" = "1" ]; then
    python3 "${SCRIPT_DIR}/verify.py" >"${OUT_DIR}/verify.stdout.log" 2>&1
    VERIFY_RC=$?
    # Now stop the monitor + formatter; formatter will print verify panel.
    cleanup
    # Render the final brief as a browser-openable HTML page so the
    # operator can showcase the actual deliverable (with hero +
    # scorecard images inline) at the end of the demo. Best-effort
    # — never fails the run.
    python3 "${SCRIPT_DIR}/render_html.py" "${OUT_DIR}" 2>/dev/null || true
    if [ -f "${OUT_DIR}/brief.html" ]; then
        echo
        echo "  📄  Final brief rendered: ${OUT_DIR}/brief.html"
        if [ "${NO_OPEN_BROWSER:-0}" != "1" ]; then
            if command -v open >/dev/null 2>&1; then
                open "${OUT_DIR}/brief.html" 2>/dev/null || true
            elif command -v xdg-open >/dev/null 2>&1; then
                xdg-open "${OUT_DIR}/brief.html" >/dev/null 2>&1 || true
            fi
        fi
    fi
    exit "${VERIFY_RC}"
fi

cleanup
python3 "${SCRIPT_DIR}/verify.py"
VERIFY_RC=$?
python3 "${SCRIPT_DIR}/render_html.py" "${OUT_DIR}" 2>/dev/null || true
if [ -f "${OUT_DIR}/brief.html" ]; then
    echo
    echo "  Final brief rendered: ${OUT_DIR}/brief.html"
fi
exit "${VERIFY_RC}"
