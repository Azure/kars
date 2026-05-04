#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Manual E2E runner. Dispatches one or more scenarios from
# tests/e2e-manual/scenarios/.
#
# This is *not* part of CI. It is run by hand against a real cluster
# with AzureClaw already installed (Kind, AKS, or any conformant K8s).
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
    "mesh|cross_runtime_mesh.sh|Cross-runtime AgentMesh round-trip"
    "governance|governance_lane.sh|Content Safety, Policy, Rate-Limit, Trust"
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
AzureClaw manual E2E runner.

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
  AZURECLAW_E2E_RUNTIMES   Same as --runtime, env-var form.

This runner does NOT create or destroy clusters; it assumes a working
kubeconfig context with AzureClaw already installed in the
'azureclaw-system' namespace.
EOF
}

# ── Arg parsing ────────────────────────────────────────────────────────
WANTED=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --scenario) WANTED="$2"; shift 2 ;;
        --runtime)  export AZURECLAW_E2E_RUNTIMES="${2//,/ }"; shift 2 ;;
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

# ── Pre-flight ─────────────────────────────────────────────────────────
require_cluster
require_azureclaw_installed

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
    # Run in a subshell so counters reset per-scenario; capture exit.
    if ( bash "$path" ); then
        : # scenario_summary inside the script reports its own counts
    else
        rc=$?
        echo "scenario '${id}' exited with code ${rc}" >&2
        FAILED_SCENARIOS+=("$id")
        TOTAL_FAIL=$((TOTAL_FAIL + 1))
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

[[ ${#FAILED_SCENARIOS[@]} -eq 0 ]] || exit 1
