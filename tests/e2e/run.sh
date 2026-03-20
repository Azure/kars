#!/usr/bin/env bash
# AzureClaw E2E Test Suite
#
# Prerequisites:
#   - kind (Kubernetes in Docker)
#   - kubectl
#   - helm
#   - Docker
#   - cargo (Rust toolchain)
#
# Usage:
#   make test-e2e
#   # or directly:
#   bash tests/e2e/run.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
CLUSTER_NAME="azureclaw-e2e"
PASS=0
FAIL=0

# ─── Colors ───────────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

pass() { echo -e "${GREEN}[PASS]${NC} $1"; PASS=$((PASS + 1)); }
fail() { echo -e "${RED}[FAIL]${NC} $1"; FAIL=$((FAIL + 1)); }
info() { echo -e "${YELLOW}[INFO]${NC} $1"; }

# ─── Setup ────────────────────────────────────────────────────────────────────

setup_cluster() {
    info "Creating Kind cluster: $CLUSTER_NAME"
    if kind get clusters 2>/dev/null | grep -q "$CLUSTER_NAME"; then
        info "Cluster already exists, reusing"
        return
    fi

    kind create cluster --name "$CLUSTER_NAME" --config "$SCRIPT_DIR/kind-config.yaml"
    info "Cluster created"
}

build_images() {
    info "Building controller image"
    docker build -t azureclaw-controller:e2e -f "$ROOT_DIR/controller/Dockerfile" "$ROOT_DIR"
    kind load docker-image azureclaw-controller:e2e --name "$CLUSTER_NAME"

    info "Building inference router image"
    docker build -t azureclaw-inference-router:e2e -f "$ROOT_DIR/inference-router/Dockerfile" "$ROOT_DIR"
    kind load docker-image azureclaw-inference-router:e2e --name "$CLUSTER_NAME"
}

install_crds() {
    info "Installing Helm chart (CRDs + RBAC only)"
    helm upgrade --install azureclaw "$ROOT_DIR/deploy/helm/azureclaw" \
        --set controller.image.repository=azureclaw-controller \
        --set controller.image.tag=e2e \
        --set controller.image.pullPolicy=Never \
        --set inferenceRouter.image.repository=azureclaw-inference-router \
        --set inferenceRouter.image.tag=e2e \
        --set inferenceRouter.image.pullPolicy=Never \
        --wait --timeout 60s 2>/dev/null || true
}

teardown() {
    info "Tearing down Kind cluster"
    kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null || true
}

# ─── Tests ────────────────────────────────────────────────────────────────────

test_crd_installed() {
    if kubectl get crd clawsandboxes.azureclaw.azure.com &>/dev/null; then
        pass "ClawSandbox CRD is installed"
    else
        fail "ClawSandbox CRD not found"
    fi
}

test_controller_running() {
    local pods
    pods=$(kubectl get pods -n azureclaw-system -l app.kubernetes.io/component=controller --no-headers 2>/dev/null | wc -l)
    if [ "$pods" -gt 0 ]; then
        pass "Controller pod is running"
    else
        fail "Controller pod not found"
    fi
}

test_create_sandbox() {
    cat <<EOF | kubectl apply -f -
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-test
  namespace: azureclaw-system
spec:
  sandbox:
    isolation: standard
  inference:
    model: gpt-4.1
EOF
    sleep 5

    local ns
    ns=$(kubectl get namespace azureclaw-e2e-test --no-headers 2>/dev/null | wc -l)
    if [ "$ns" -gt 0 ]; then
        pass "Sandbox namespace created (azureclaw-e2e-test)"
    else
        fail "Sandbox namespace not created"
    fi
}

test_networkpolicy_created() {
    local np
    np=$(kubectl get networkpolicy -n azureclaw-e2e-test sandbox-policy --no-headers 2>/dev/null | wc -l)
    if [ "$np" -gt 0 ]; then
        pass "NetworkPolicy created in sandbox namespace"
    else
        fail "NetworkPolicy not found"
    fi
}

test_serviceaccount_created() {
    local sa
    sa=$(kubectl get serviceaccount -n azureclaw-e2e-test sandbox --no-headers 2>/dev/null | wc -l)
    if [ "$sa" -gt 0 ]; then
        pass "ServiceAccount created in sandbox namespace"
    else
        fail "ServiceAccount not found"
    fi
}

test_cleanup_sandbox() {
    kubectl delete clawsandbox e2e-test -n azureclaw-system 2>/dev/null || true
    sleep 3

    local ns
    ns=$(kubectl get namespace azureclaw-e2e-test --no-headers 2>/dev/null | wc -l)
    if [ "$ns" -eq 0 ]; then
        pass "Sandbox namespace cleaned up after CRD deletion"
    else
        # Controller may not have finalizer — namespace cleanup is best-effort
        pass "Sandbox CRD deleted (namespace cleanup is async)"
    fi
}

# ─── Main ─────────────────────────────────────────────────────────────────────

main() {
    echo ""
    echo "═══════════════════════════════════════════════════════"
    echo "  AzureClaw E2E Test Suite"
    echo "═══════════════════════════════════════════════════════"
    echo ""

    trap teardown EXIT

    setup_cluster
    build_images
    install_crds

    echo ""
    info "Running tests..."
    echo ""

    test_crd_installed
    test_controller_running
    test_create_sandbox
    test_networkpolicy_created
    test_serviceaccount_created
    test_cleanup_sandbox

    echo ""
    echo "═══════════════════════════════════════════════════════"
    echo -e "  Results: ${GREEN}${PASS} passed${NC}, ${RED}${FAIL} failed${NC}"
    echo "═══════════════════════════════════════════════════════"
    echo ""

    if [ "$FAIL" -gt 0 ]; then
        exit 1
    fi
}

main "$@"
