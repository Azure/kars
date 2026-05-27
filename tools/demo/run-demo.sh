#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
# tools/demo/run-demo.sh
#
# One scripted demo flow that exercises the full KarsSandbox lifecycle
# in three modes:
#
#   --mode dev     kars dev (local Docker, single sandbox container).
#                  No K8s; the demo terminates after the dev container
#                  has come up Ready (router echo).
#   --mode kind    Local Kind cluster bootstrapped by tests/e2e helpers.
#                  Applies the four scenario YAMLs and verifies each
#                  phase transition with kubectl.
#   --mode aks     A real AKS cluster the caller is already kubectl-logged
#                  into (kars up has already created it). Applies the
#                  same scenarios, expects production-grade observability
#                  (router echo, Job creation, EgressApproval Active phase).
#
# Scenarios live in tools/demo/scenarios/ and are shared across modes —
# the only thing that changes is how we bring up the substrate.
#
# Exit codes: 0 success, 1 setup failure, 2 scenario failure, 3 cleanup
# failure, 4 unsupported mode.
#
# No secrets, no provisioning, no destructive actions. Bring-your-own
# credentials in dev/aks modes (the script never logs into Azure on
# your behalf).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCENARIOS_DIR="${SCRIPT_DIR}/scenarios"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

MODE=""
SKIP_CLEANUP=0
TIMEOUT_SECS=180

usage() {
    cat <<EOF
Usage: $(basename "$0") --mode <dev|kind|aks> [--skip-cleanup] [--timeout SECS]

Modes:
  dev    Local Docker via 'kars dev' (no K8s; smoke check only).
  kind   Local Kind cluster (uses tests/e2e infra helpers).
  aks    A real AKS cluster you've already targeted with kubectl.

Options:
  --skip-cleanup   Leave the demo resources behind for inspection.
  --timeout SECS   Per-scenario wait timeout (default 180).
  -h, --help       Show this help.

The script never touches your Azure subscription or creates clusters.
For AKS, run 'kars up' first; for Kind, ensure docker is running.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --mode) MODE="${2:-}"; shift 2 ;;
        --skip-cleanup) SKIP_CLEANUP=1; shift ;;
        --timeout) TIMEOUT_SECS="${2:-}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown arg: $1" >&2; usage; exit 4 ;;
    esac
done

if [[ -z "$MODE" ]]; then
    usage
    exit 4
fi

case "$MODE" in
    dev|kind|aks) ;;
    *) echo "Unsupported --mode: $MODE" >&2; exit 4 ;;
esac

# ---------------------------------------------------------------- helpers ---

log()  { printf "\033[36m[demo:%s]\033[0m %s\n" "$MODE" "$*"; }
ok()   { printf "\033[32m[demo:%s]\033[0m %s\n" "$MODE" "$*"; }
warn() { printf "\033[33m[demo:%s]\033[0m %s\n" "$MODE" "$*" >&2; }
fail() { printf "\033[31m[demo:%s]\033[0m %s\n" "$MODE" "$*" >&2; exit 2; }

need() {
    command -v "$1" >/dev/null 2>&1 || { warn "missing dependency: $1"; exit 1; }
}

wait_for() {
    local desc="$1"; shift
    local deadline=$(( $(date +%s) + TIMEOUT_SECS ))
    while (( $(date +%s) < deadline )); do
        if "$@"; then ok "$desc — ready"; return 0; fi
        sleep 3
    done
    fail "timed out waiting for: $desc"
}

# ---------------------------------------------------------------- dev mode --

run_dev() {
    need docker
    need kars

    log "starting kars dev (--name demo-agent, --build)"
    log "ctrl-c after the TUI shows Ready; the demo verifies router echo only"
    kars dev --name demo-agent --build &
    local dev_pid=$!

    trap '[[ $SKIP_CLEANUP -eq 0 ]] && kill '"$dev_pid"' 2>/dev/null || true' EXIT

    wait_for "dev container Router=Ready" bash -c '
        docker ps --format "{{.Names}}" | grep -q "^kars-demo-agent$" &&
        docker exec kars-demo-agent curl -sf http://127.0.0.1:8443/internal/policy-status >/dev/null 2>&1
    '

    ok "dev demo green — agent running locally, router enforcing policy"
    ok "press Ctrl-C to tear down (or re-run with --skip-cleanup to keep)"

    [[ $SKIP_CLEANUP -eq 1 ]] && wait "$dev_pid"
}

# ----------------------------------------------------------- k8s mode core --

apply_scenarios() {
    log "applying KarsSandbox + InferencePolicy"
    kubectl apply -f "$SCENARIOS_DIR/01-sandbox.yaml"

    wait_for "KarsSandbox demo-agent phase=Ready" bash -c '
        kubectl get karssandbox -n kars-system demo-agent \
            -o jsonpath="{.status.phase}" 2>/dev/null | grep -q "^Ready$"
    '

    log "applying ToolPolicy gate (web.fetch → approval-required)"
    kubectl apply -f "$SCENARIOS_DIR/02-toolpolicy.yaml"
    wait_for "ToolPolicy demo-web-fetch phase=Compiled" bash -c '
        kubectl get toolpolicy -n kars-system demo-web-fetch \
            -o jsonpath="{.status.phase}" 2>/dev/null | grep -qE "^(Compiled|Ready)$"
    '

    log "applying EgressApproval grant (api.stripe.com, PT10M)"
    kubectl apply -f "$SCENARIOS_DIR/03-egress-approval.yaml"
    wait_for "EgressApproval demo-stripe-grant phase=Active" bash -c '
        kubectl get egressapproval -n kars-system demo-stripe-grant \
            -o jsonpath="{.status.phase}" 2>/dev/null | grep -q "^Active$"
    '

    log "applying KarsEval (run-now, jailbreak-baseline)"
    kubectl apply -f "$SCENARIOS_DIR/04-karseval.yaml"
    wait_for "KarsEval demo-eval annotation consumed" bash -c '
        ! kubectl get karseval -n kars-system demo-eval \
            -o jsonpath="{.metadata.annotations.kars\\.azure\\.com/run-now}" \
            2>/dev/null | grep -q "true"
    '
    wait_for "KarsEval demo-eval Job created" bash -c '
        kubectl get jobs -n kars-system \
            -l kars.azure.com/karseval=demo-eval \
            --no-headers 2>/dev/null | grep -q karseval
    '

    ok "all four scenarios reached steady state"
    kubectl get karssandbox,toolpolicy,egressapproval,karseval \
        -n kars-system --no-headers || true
}

cleanup_scenarios() {
    [[ $SKIP_CLEANUP -eq 1 ]] && { warn "skipping cleanup (--skip-cleanup)"; return 0; }
    log "tearing down demo resources"
    kubectl delete --ignore-not-found -f "$SCENARIOS_DIR/04-karseval.yaml" || true
    kubectl delete --ignore-not-found -f "$SCENARIOS_DIR/03-egress-approval.yaml" || true
    kubectl delete --ignore-not-found -f "$SCENARIOS_DIR/02-toolpolicy.yaml" || true
    kubectl delete --ignore-not-found -f "$SCENARIOS_DIR/01-sandbox.yaml" || true
    kubectl delete jobs -n kars-system \
        -l kars.azure.com/karseval=demo-eval --ignore-not-found || true
}

# --------------------------------------------------------------- kind mode --

run_kind() {
    need docker
    need kubectl
    need kind

    if ! kind get clusters | grep -q '^kars-e2e$'; then
        log "no kars-e2e Kind cluster found"
        log "bring one up with: bash tests/e2e/infra-e2e.sh up   (then re-run)"
        exit 1
    fi

    kubectl config use-context kind-kars-e2e
    trap cleanup_scenarios EXIT
    apply_scenarios
    ok "kind demo green"
}

# ---------------------------------------------------------------- aks mode --

run_aks() {
    need kubectl

    local ctx
    ctx="$(kubectl config current-context 2>/dev/null || true)"
    if [[ -z "$ctx" ]]; then
        warn "no kubectl context; run 'az aks get-credentials' or 'kars up' first"
        exit 1
    fi
    log "using kubectl context: $ctx"

    if ! kubectl get crd karssandboxes.kars.azure.com >/dev/null 2>&1; then
        warn "KarsSandbox CRD not installed on this cluster — run 'kars up' first"
        exit 1
    fi

    trap cleanup_scenarios EXIT
    apply_scenarios
    ok "aks demo green"
}

# --------------------------------------------------------------- dispatch ---

case "$MODE" in
    dev)  run_dev ;;
    kind) run_kind ;;
    aks)  run_aks ;;
esac
