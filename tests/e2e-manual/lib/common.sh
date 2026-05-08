#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Shared helpers for the manual E2E suite (tests/e2e-manual/).
#
# This file is sourced by every scenario script. It owns:
#   - colour-coded logging
#   - a tiny assertion API (assert_eq, assert_ready, assert_pod_running,
#     wait_until, ...)
#   - shared cluster + namespace conventions
#   - per-scenario PASS/FAIL counters
#
# It does NOT create or destroy any cluster on its own. The manual suite
# assumes you already have a working cluster reachable via the current
# kubeconfig context — see ../README.md.

set -euo pipefail

# ── Metrics layer (sourced from sibling lib) ────────────────────────────
# shellcheck source=metrics.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/metrics.sh"

# ── Colours ─────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
GREY='\033[0;90m'
NC='\033[0m'

# ── Counters ────────────────────────────────────────────────────────────
: "${MANUAL_E2E_PASS:=0}"
: "${MANUAL_E2E_FAIL:=0}"
: "${MANUAL_E2E_SKIP:=0}"
export MANUAL_E2E_PASS MANUAL_E2E_FAIL MANUAL_E2E_SKIP

# ── Defaults (overridable via env) ──────────────────────────────────────
: "${MANUAL_E2E_NAMESPACE_PREFIX:=azureclaw-e2e-manual}"
: "${MANUAL_E2E_TIMEOUT:=300}"     # default per-resource readiness wait, seconds
: "${MANUAL_E2E_KEEP_NS:=0}"       # 1 → leave namespaces in place after a scenario
: "${MANUAL_E2E_VERBOSE:=0}"       # 1 → dump kubectl describe on failure

# ── Logging ─────────────────────────────────────────────────────────────
log_info()  { echo -e "${CYAN}[INFO]${NC} $*"; }
log_pass()  { echo -e "${GREEN}[PASS]${NC} $*"; MANUAL_E2E_PASS=$((MANUAL_E2E_PASS + 1)); }
log_fail()  { echo -e "${RED}[FAIL]${NC} $*"; MANUAL_E2E_FAIL=$((MANUAL_E2E_FAIL + 1)); }
log_skip()  { echo -e "${YELLOW}[SKIP]${NC} $*"; MANUAL_E2E_SKIP=$((MANUAL_E2E_SKIP + 1)); }
log_warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
log_step()  { echo -e "${GREY}[ ⟶  ]${NC} $*"; }

# ── Pre-flight: check required CLIs ─────────────────────────────────────
require_cli() {
    local missing=()
    for c in "$@"; do
        command -v "$c" >/dev/null 2>&1 || missing+=("$c")
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        log_fail "missing required CLIs: ${missing[*]}"
        exit 2
    fi
}

# ── Cluster preflight ───────────────────────────────────────────────────
require_cluster() {
    require_cli kubectl
    if ! kubectl version --request-timeout=5s >/dev/null 2>&1; then
        log_fail "no Kubernetes cluster reachable via current kubeconfig context"
        exit 2
    fi
    local ctx
    ctx=$(kubectl config current-context 2>/dev/null || echo "<unknown>")
    log_info "using kubeconfig context: ${ctx}"
}

require_azureclaw_installed() {
    if ! kubectl get crd clawsandboxes.azureclaw.azure.com >/dev/null 2>&1; then
        log_fail "AzureClaw CRDs not installed in this cluster (no clawsandboxes.azureclaw.azure.com)"
        log_info "install with: helm upgrade --install azureclaw deploy/helm/azureclaw -n azureclaw-system --create-namespace"
        exit 2
    fi
    if ! kubectl -n azureclaw-system get deploy azureclaw-controller >/dev/null 2>&1; then
        log_warn "controller deployment not found in azureclaw-system; some scenarios may fail"
    fi
}

# ── Namespace helpers ───────────────────────────────────────────────────
new_ns() {
    # Create a fresh namespace under the manual-suite prefix and echo its name.
    local suffix="${1:-$(date +%s)-$RANDOM}"
    local ns="${MANUAL_E2E_NAMESPACE_PREFIX}-${suffix}"
    kubectl create namespace "$ns" >/dev/null
    echo "$ns"
}

cleanup_ns() {
    local ns="$1"
    if [[ "${MANUAL_E2E_KEEP_NS:-0}" == "1" ]]; then
        log_info "MANUAL_E2E_KEEP_NS=1 — leaving namespace ${ns} in place"
        return
    fi
    kubectl delete namespace "$ns" --wait=false --ignore-not-found=true >/dev/null 2>&1 || true
}

# Return the controller-managed pod namespace for a given sandbox name.
# The controller always provisions sandbox pods in `azureclaw-<name>`,
# regardless of where the ClawSandbox CR itself lives.
pod_ns_for() {
    echo "azureclaw-$1"
}

# Best-effort cleanup of both the test scenario namespace and the
# matching `azureclaw-<name>` pod namespace the controller created.
# Honours MANUAL_E2E_KEEP_NS exactly like `cleanup_ns`.
cleanup_sandbox() {
    local cr_ns="$1" name="$2"
    cleanup_ns "$cr_ns"
    cleanup_ns "$(pod_ns_for "$name")"
}

# Toggle the audited break-glass label on a sandbox pod namespace so
# tests can `kubectl exec` into the openclaw container. Production-grade
# admission policy (azureclaw-sandbox-exec-ban) blocks exec/attach into
# the agent runtime container by default; the label is the documented
# emergency-override path. Bypasses are audited at the apiserver layer.
enable_break_glass() {
    local pod_ns="$1"
    kubectl label namespace "$pod_ns" \
        azureclaw.azure.com/break-glass=true --overwrite >/dev/null 2>&1 || true
}

disable_break_glass() {
    local pod_ns="$1"
    kubectl label namespace "$pod_ns" \
        azureclaw.azure.com/break-glass- >/dev/null 2>&1 || true
}

# ── Wait helpers ────────────────────────────────────────────────────────
wait_until() {
    # Usage: wait_until <timeout-sec> <description> -- <cmd ...>
    local timeout="$1"; shift
    local desc="$1"; shift
    [[ "$1" == "--" ]] && shift
    local deadline=$((SECONDS + timeout))
    while (( SECONDS < deadline )); do
        if "$@" >/dev/null 2>&1; then
            return 0
        fi
        sleep 2
    done
    log_fail "timed out after ${timeout}s waiting for: ${desc}"
    return 1
}

wait_for_condition() {
    # Usage: wait_for_condition <ns> <kind> <name> <condition> [timeout]
    local ns="$1" kind="$2" name="$3" cond="$4" timeout="${5:-$MANUAL_E2E_TIMEOUT}"
    log_step "waiting for ${kind}/${name} condition=${cond} (≤${timeout}s)…"
    if kubectl -n "$ns" wait --for="condition=${cond}" --timeout="${timeout}s" "${kind}/${name}" >/dev/null 2>&1; then
        log_pass "${kind}/${name} reached ${cond}"
        return 0
    fi
    log_fail "${kind}/${name} did not reach ${cond} within ${timeout}s"
    if [[ "${MANUAL_E2E_VERBOSE:-0}" == "1" ]]; then
        kubectl -n "$ns" describe "${kind}/${name}" | sed 's/^/    /'
    fi
    return 1
}

wait_for_clawsandbox_ready() {
    # ClawSandbox doesn't ship a stock .status.conditions Ready type until
    # phase 2; we check for the .status.phase == Running OR the Ready condition.
    local ns="$1" name="$2" timeout="${3:-$MANUAL_E2E_TIMEOUT}"
    log_step "waiting for ClawSandbox/${name} to become Ready (≤${timeout}s)…"
    local deadline=$((SECONDS + timeout))
    local started_ms now_ms
    started_ms=$(_metrics_now_ms)
    while (( SECONDS < deadline )); do
        local phase ready
        phase=$(kubectl -n "$ns" get clawsandbox "$name" -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
        ready=$(kubectl -n "$ns" get clawsandbox "$name" \
            -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || echo "")
        if [[ "$ready" == "True" || "$phase" == "Running" ]]; then
            log_pass "ClawSandbox/${name} is Ready"
            now_ms=$(_metrics_now_ms)
            metric_emit "${MANUAL_E2E_SCENARIO:-?}" ttiSandbox ms $((now_ms - started_ms)) \
                "sandbox=${name}" "ns=${ns}" "phase=${phase:-Ready}"
            return 0
        fi
        sleep 3
    done
    log_fail "ClawSandbox/${name} did not become Ready within ${timeout}s"
    metric_emit "${MANUAL_E2E_SCENARIO:-?}" ttiSandboxTimeout ms "$((timeout * 1000))" \
        "sandbox=${name}" "ns=${ns}"
    if [[ "${MANUAL_E2E_VERBOSE:-0}" == "1" ]]; then
        kubectl -n "$ns" describe clawsandbox "$name" | sed 's/^/    /'
        kubectl -n "$ns" get pods -o wide | sed 's/^/    /'
    fi
    return 1
}

# ── Assertion API ───────────────────────────────────────────────────────
assert_eq() {
    local desc="$1" expected="$2" actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        log_pass "${desc}: ${actual}"
    else
        log_fail "${desc}: expected '${expected}', got '${actual}'"
    fi
}

assert_contains() {
    local desc="$1" needle="$2" haystack="$3"
    if [[ "$haystack" == *"$needle"* ]]; then
        log_pass "${desc} contains '${needle}'"
    else
        log_fail "${desc} missing '${needle}' (got: '${haystack:0:200}')"
    fi
}

assert_pod_running() {
    local ns="$1" selector="$2"
    local count
    count=$(kubectl -n "$ns" get pods -l "$selector" \
        -o jsonpath='{range .items[?(@.status.phase=="Running")]}{.metadata.name}{"\n"}{end}' 2>/dev/null | wc -l | tr -d ' ')
    if (( count > 0 )); then
        log_pass "pod(s) running in ${ns} for selector '${selector}' (count=${count})"
        return 0
    fi
    log_fail "no Running pods in ${ns} for selector '${selector}'"
    return 1
}

# ── Scenario boilerplate ────────────────────────────────────────────────
scenario_header() {
    local name="$1"
    echo
    echo -e "${CYAN}╔═══════════════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║${NC} Scenario: ${name}"
    echo -e "${CYAN}╚═══════════════════════════════════════════════════════════════════════╝${NC}"
}

scenario_summary() {
    local name="$1"
    echo
    echo -e "${CYAN}── Summary: ${name} ──${NC}"
    echo "  Passed: ${MANUAL_E2E_PASS}"
    echo "  Failed: ${MANUAL_E2E_FAIL}"
    echo "  Skipped: ${MANUAL_E2E_SKIP}"
    if (( MANUAL_E2E_FAIL > 0 )); then
        return 1
    fi
    return 0
}
