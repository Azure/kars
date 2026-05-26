#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Manual E2E runner. Dispatches one or more scenarios from
# tests/e2e-manual/scenarios/.
#
# This is *not* part of CI. It is run by hand against a real cluster
# with Kars already installed (Kind, AKS, or any conformant K8s).
#
# Usage:
#   bash tests/e2e-manual/run.sh                       # run every scenario
#   bash tests/e2e-manual/run.sh --scenario governance
#   bash tests/e2e-manual/run.sh --list
#   bash tests/e2e-manual/run.sh --scenario runtime --runtime openclaw,oai-agents
#
# See tests/e2e-manual/README.md for prerequisites and troubleshooting.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCENARIOS_DIR="${SCRIPT_DIR}/scenarios"
LIB_DIR="${SCRIPT_DIR}/lib"

# shellcheck source=lib/common.sh
source "${LIB_DIR}/common.sh"

# ── Scenario registry ──────────────────────────────────────────────────
# id           script                                 description
declare -a SCENARIOS=(
    "runtime|runtime_matrix.sh|Bring up every supported runtime and assert Ready"
    "crds|crd_admission.sh|Admission + status for the 7 untested Kars CRDs"
    "inference|inference_smoke.sh|Agent → router → Foundry round-trip (per runtime)"
    "foundry-bing|foundry_bing.sh|Foundry Bing grounding tool through the router"
    "agt-mesh|agt_mesh.sh|AGT mesh: 1sub, 2sub-parallel, sibling, multiturn"
    "mesh|cross_runtime_mesh.sh|Cross-runtime AgentMesh round-trip"
    "governance|governance_lane.sh|Content Safety, Policy, Rate-Limit, Trust"
    "egress|egress_lifecycle.sh|Egress allowlist: learn → enforce → approve → deny"
    "failures|failure_modes.sh|Router crash, relay disconnect, OOM"
    "isolation|multi_tenant_isolation.sh|NetworkPolicy + SA + token isolation"
)

list_scenarios() {
    printf '%-12s %s\n' "ID" "DESCRIPTION"
    printf '%-12s %s\n' "──" "───────────"
    for s in "${SCENARIOS[@]}"; do
        IFS='|' read -r id _ desc <<<"$s"
        printf '%-12s %s\n' "$id" "$desc"
    done
}

usage() {
    cat <<EOF
Kars manual E2E runner.

Usage: bash tests/e2e-manual/run.sh [options]

Options:
  --scenario ID[,ID,...]   Run only the named scenarios (default: all).
                           Use --list to see available IDs.
  --runtime LIST           Comma- or space-separated runtime aliases for
                           the runtime-matrix scenario.
  --keep-ns                Leave namespaces in place after each scenario.
  --list                   Print available scenarios and exit.
  -h, --help               Show this help.

Environment overrides:
  MANUAL_E2E_TIMEOUT       Per-resource wait, seconds (default 300).
  MANUAL_E2E_KEEP_NS       1 → keep namespaces (same as --keep-ns).
  MANUAL_E2E_VERBOSE       1 → dump kubectl describe on failure.
  KARS_E2E_RUNTIMES   Same as --runtime, env-var form.

This runner does NOT create or destroy clusters; it assumes a working
kubeconfig context with Kars already installed in the
'kars-system' namespace.
EOF
}

# ── Arg parsing ────────────────────────────────────────────────────────
WANTED=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --scenario) WANTED="$2"; shift 2 ;;
        --runtime)  export KARS_E2E_RUNTIMES="${2//,/ }"; shift 2 ;;
        --keep-ns)  export MANUAL_E2E_KEEP_NS=1; shift ;;
        --list)     list_scenarios; exit 0 ;;
        -h|--help)  usage; exit 0 ;;
        *) echo "unknown arg: $1" >&2; usage; exit 2 ;;
    esac
done

# ── Resolve scenario list ──────────────────────────────────────────────
selected=()
if [[ -z "$WANTED" ]]; then
    for s in "${SCENARIOS[@]}"; do selected+=("$s"); done
else
    IFS=',' read -ra wants <<<"$WANTED"
    for w in "${wants[@]}"; do
        match=""
        for s in "${SCENARIOS[@]}"; do
            IFS='|' read -r id _ _ <<<"$s"
            [[ "$id" == "$w" ]] && match="$s" && break
        done
        if [[ -n "$match" ]]; then
            selected+=("$match")
        else
            echo "no such scenario: $w" >&2
            list_scenarios >&2
            exit 2
        fi
    done
fi

# ── Signal handling + cleanup ──────────────────────────────────────────
# Without these, ^C is swallowed by the bash subshell that runs each
# scenario and the run can't be aborted. We forward SIGINT/SIGTERM to
# the current scenario child, then run a label-scoped sweep on EXIT to
# remove any leftover CR namespaces and sandbox pod namespaces. The
# sweep is opt-out via --keep-ns / MANUAL_E2E_KEEP_NS=1.
SCENARIO_PID=""
INTERRUPTED=0

_forward_signal() {
    local sig="$1"
    INTERRUPTED=1
    if [[ -n "$SCENARIO_PID" ]] && kill -0 "$SCENARIO_PID" 2>/dev/null; then
        # Send to the whole process group of the scenario subshell.
        kill -"$sig" "-$SCENARIO_PID" 2>/dev/null || kill -"$sig" "$SCENARIO_PID" 2>/dev/null || true
    fi
}
trap '_forward_signal INT'  INT
trap '_forward_signal TERM' TERM

_cleanup_on_exit() {
    local rc=$?
    trap - INT TERM EXIT
    if [[ "${MANUAL_E2E_KEEP_NS:-0}" == "1" ]]; then
        echo "[INFO] --keep-ns set; skipping namespace sweep"
        return $rc
    fi
    if (( INTERRUPTED == 1 )); then
        echo "[INFO] interrupted — sweeping leftover manual-e2e namespaces…"
    else
        echo "[INFO] sweeping leftover manual-e2e namespaces…"
    fi
    # CR namespaces are labeled by the factory; pod namespaces are
    # named kars-<sandbox> and labeled by the controller. We match
    # both via the same label so the sweep is scoped.
    local ns_list
    ns_list=$(kubectl get ns \
        -l kars.azure.com/test-suite=manual-e2e \
        -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || true)
    if [[ -z "$ns_list" ]]; then
        echo "[INFO] nothing to sweep"
        return $rc
    fi
    # Drop CR finalizers first so deletion isn't blocked when the
    # controller is unhealthy. Best-effort.
    for ns in $ns_list; do
        kubectl -n "$ns" get karssandbox -o name 2>/dev/null | while read -r cr; do
            kubectl -n "$ns" patch "$cr" --type=merge -p '{"metadata":{"finalizers":[]}}' >/dev/null 2>&1 || true
        done
    done
    # shellcheck disable=SC2086
    kubectl delete ns $ns_list --wait=false --ignore-not-found 2>&1 | sed 's/^/    /' || true
    return $rc
}
trap _cleanup_on_exit EXIT

# ── Pre-flight ─────────────────────────────────────────────────────────
require_cluster
require_kars_installed

# Reset metrics file for this run so percentile aggregation reflects
# only scenarios from this invocation. Past runs are preserved as
# *.jsonl.<timestamp> archives in the output dir.
if [[ -s "${MANUAL_E2E_METRICS_FILE}" ]]; then
    archive="${MANUAL_E2E_METRICS_FILE}.$(date +%Y%m%dT%H%M%S)"
    mv "${MANUAL_E2E_METRICS_FILE}" "${archive}"
    echo "[INFO] archived prior metrics → ${archive}"
fi
: > "${MANUAL_E2E_METRICS_FILE}"
echo "[INFO] metrics file:   ${MANUAL_E2E_METRICS_FILE}"
echo "[INFO] run id:         ${MANUAL_E2E_RUN_ID}"

start_ts=$(date +%s)
TOTAL_PASS=0; TOTAL_FAIL=0; TOTAL_SKIP=0
FAILED_SCENARIOS=()

for s in "${selected[@]}"; do
    IFS='|' read -r id script desc <<<"$s"
    path="${SCENARIOS_DIR}/${script}"
    if [[ ! -x "$path" && ! -r "$path" ]]; then
        echo "missing scenario script: ${path}" >&2
        TOTAL_FAIL=$((TOTAL_FAIL + 1))
        FAILED_SCENARIOS+=("$id")
        continue
    fi
    echo
    echo "════════════════════════════════════════════════════════════════"
    echo "▶ scenario: ${id} — ${desc}"
    echo "════════════════════════════════════════════════════════════════"
    scen_start=$(_metrics_now_ms)
    # Run scenario in its own process group (job control) so we can
    # forward ^C cleanly. `wait` returns immediately on signal, letting
    # the INT trap fire.
    set +e
    set -m
    bash "$path" &
    SCENARIO_PID=$!
    set +m
    wait "$SCENARIO_PID"
    scen_rc=$?
    SCENARIO_PID=""
    set -e
    if (( scen_rc != 0 )); then
        echo "scenario '${id}' exited with code ${scen_rc}" >&2
        FAILED_SCENARIOS+=("$id")
        TOTAL_FAIL=$((TOTAL_FAIL + 1))
    fi
    scen_end=$(_metrics_now_ms)
    metric_emit "$id" scenarioWallClock ms "$((scen_end - scen_start))" \
        "exit=${scen_rc}"
    if (( INTERRUPTED == 1 )); then
        echo "[INFO] interrupted — stopping scenario loop"
        break
    fi
done

end_ts=$(date +%s)
elapsed=$((end_ts - start_ts))

echo
echo "════════════════════════════════════════════════════════════════"
echo "▶ run.sh aggregate result"
echo "  scenarios run:      ${#selected[@]}"
echo "  scenarios failed:   ${#FAILED_SCENARIOS[@]}"
[[ ${#FAILED_SCENARIOS[@]} -gt 0 ]] && echo "  failures:           ${FAILED_SCENARIOS[*]}"
echo "  elapsed:            ${elapsed}s"
echo "════════════════════════════════════════════════════════════════"

# Benchmark summary across every scenario run.
metrics_summary "${MANUAL_E2E_METRICS_FILE}"
echo "  raw metrics: ${MANUAL_E2E_METRICS_FILE}"

[[ ${#FAILED_SCENARIOS[@]} -eq 0 ]] || exit 1
