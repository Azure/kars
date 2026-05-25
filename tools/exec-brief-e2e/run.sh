#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# run.sh — top-level orchestrator for the exec-brief e2e harness.
# Runs monitor.sh in the background, drive.sh in the foreground, then
# verify.py once the driver returns. All artifacts land under
# tools/exec-brief-e2e/out/<UTC-timestamp>/.
#
# Usage:
#   ./run.sh                                    # uses current kubectl context
#   TELEGRAM_BOT_TOKEN=xxx ./run.sh             # enables telegram check
#   WATCHDOG_SECS=2400 SANDBOX_NAME=foo ./run.sh
#
# Exit code = verify.py's exit code (0 = all 7 checks pass).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="${SCRIPT_DIR}/out/${RUN_ID}"
mkdir -p "${OUT_DIR}"
# Maintain a 'latest' symlink so verify/monitor can find the active run.
ln -sfn "${RUN_ID}" "${SCRIPT_DIR}/out/latest"

export OUT_DIR

echo "[run] RUN_ID=${RUN_ID}  OUT_DIR=${OUT_DIR}"

# Background monitor. Wrap in a subshell so $! is the subshell PID, which is
# the parent of the entire `monitor.sh | tee` pipeline — we can then kill the
# subshell *and* its children together via the process group. (Previously $!
# was just the trailing `tee` PID, leaving `monitor.sh` and its `kubectl logs -f`
# grandchildren alive after cleanup, hanging the run.)
( "${SCRIPT_DIR}/monitor.sh" 2>&1 | tee "${OUT_DIR}/monitor.log" ) &
MONITOR_PID=$!

cleanup() {
    if kill -0 "${MONITOR_PID}" 2>/dev/null; then
        # Kill the subshell process group first (covers monitor.sh + tee + any
        # kubectl logs -f grandchildren spawned by monitor.sh).
        kill -TERM "-${MONITOR_PID}" 2>/dev/null || true
        kill "${MONITOR_PID}" 2>/dev/null || true
        # Belt-and-braces: explicitly hunt any stray kubectl logs -f processes
        # that escaped (macOS sandboxes forbid pkill/killall).
        for pid in $(ps -o pid=,command= | awk '/kubectl[[:space:]]+logs[[:space:]]+-f/ {print $1}'); do
            kill "${pid}" 2>/dev/null || true
        done
        # Don't `wait` — the subshell may have detached from job control.
        # Just give the kernel a moment to reap.
        sleep 1
    fi
}
trap cleanup EXIT INT TERM

# Foreground driver (provision + drive prompt).
set +e
"${SCRIPT_DIR}/drive.sh" 2>&1 | tee "${OUT_DIR}/drive.log"
DRIVE_RC=$?
set -e

# Give the monitor a beat to flush the last log lines.
sleep 5
cleanup

if [ "${DRIVE_RC}" -ne 0 ]; then
    echo "[run] driver exited rc=${DRIVE_RC}, skipping verify"
    exit "${DRIVE_RC}"
fi

# Verify.
python3 "${SCRIPT_DIR}/verify.py"
