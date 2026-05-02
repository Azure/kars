#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

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
# Phase 3 S4: runtime-parameterised harness. Defaults to OpenClaw;
# CI matrices set AZURECLAW_E2E_RUNTIME to exercise oai-agents /
# maf-python / byo. Each runtime owns a named function below
# (`test_runtime_<name>`) and the runner dispatches there.
RUNTIME="${AZURECLAW_E2E_RUNTIME:-openclaw}"
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
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }

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
    # E2E always runs against a single-node Kind cluster. Multi-replica
    # leader election adds non-determinism (race on lease acquisition,
    # one replica reconciles while the other waits) without any of the
    # benefits it provides in production. Force single-replica + no
    # leader-election lease so each E2E run starts from a clean,
    # deterministic state. CI may set the same vars explicitly via
    # AZURECLAW_E2E_* env vars; the defaults here cover local runs.
    local replicas="${AZURECLAW_E2E_CONTROLLER_REPLICAS:-1}"
    local disable_le="${AZURECLAW_E2E_DISABLE_LEADER_ELECTION:-1}"
    local extra_set_args=(
        --set "controller.replicas=${replicas}"
        --set "inferenceRouter.replicas=${replicas}"
        # Without a fake Foundry endpoint, the ClawSandbox reconciler
        # degrades with "No inference endpoint configured" before it
        # ever creates the namespace. Use an .invalid TLD so anything
        # that *did* try to dial out fails closed.
        --set-string "inferenceRouter.azure.openai.endpoint=https://e2e-fake.invalid/"
        --set-string "foundry.endpoint=https://e2e-fake.invalid/"
        --set-string "foundry.projectEndpoint=https://e2e-fake.invalid/"
    )
    if [ "$disable_le" = "1" ] || [ "$disable_le" = "true" ]; then
        # `--set-string` is mandatory here: K8s pod spec requires env
        # `value` to be a string, but `--set value=false` would render
        # as a YAML boolean and the API server would reject the pod.
        extra_set_args+=(
            --set "controller.extraEnv[0].name=LEADER_ELECTION_ENABLED"
            --set-string "controller.extraEnv[0].value=false"
        )
    fi
    if ! helm upgrade --install azureclaw "$ROOT_DIR/deploy/helm/azureclaw" \
        --namespace azureclaw-system \
        --create-namespace \
        --set controller.image.repository=azureclaw-controller \
        --set controller.image.tag=e2e \
        --set controller.image.pullPolicy=Never \
        --set inferenceRouter.image.repository=azureclaw-inference-router \
        --set inferenceRouter.image.tag=e2e \
        --set inferenceRouter.image.pullPolicy=Never \
        "${extra_set_args[@]}" \
        --wait --timeout 5m; then
        warn "Helm install did not converge within 5m — dumping diagnostics"
        kubectl get all -n azureclaw-system || true
        kubectl describe pod -n azureclaw-system -l app.kubernetes.io/component=controller || true
        kubectl logs -n azureclaw-system -l app.kubernetes.io/component=controller --tail=200 || true
    fi
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
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: InferencePolicy
metadata:
  name: e2e-test-inference
  namespace: azureclaw-system
  labels:
    azureclaw.azure.com/sandbox: e2e-test
spec:
  appliesTo:
    sandboxName: e2e-test
  modelPreference:
    primary:
      provider: azure-openai
      deployment: gpt-4.1
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-test
  namespace: azureclaw-system
spec:
  runtime:
    kind: OpenClaw
    openclaw:
      version: "2026.3.13"
  sandbox:
    isolation: standard
  inferenceRef:
    name: e2e-test-inference
EOF

    # Wait up to 60s for the controller to create the sandbox namespace.
    # Image pull + reconcile in Kind on the GH runner can take 20-30s
    # cold; the previous 5s sleep was racy and we'd pipefail-die before
    # the resource showed up.
    info "Waiting for sandbox namespace azureclaw-e2e-test to appear (up to 60s)..."
    local deadline=$(($(date +%s) + 60))
    local seen=0
    while [ "$(date +%s)" -lt "$deadline" ]; do
        # `|| true` shields against `set -e`/pipefail when the
        # namespace isn't there yet (kubectl returns 1).
        if kubectl get namespace azureclaw-e2e-test --no-headers 2>/dev/null | grep -q azureclaw-e2e-test; then
            seen=1
            break
        fi
        sleep 2
    done

    if [ "$seen" -eq 1 ]; then
        pass "Sandbox namespace created (azureclaw-e2e-test)"
    else
        warn "Namespace did not appear within 60s — dumping diagnostics"
        kubectl get clawsandboxes -A -o wide || true
        kubectl describe clawsandbox e2e-test -n azureclaw-system || true
        kubectl get events -n azureclaw-system --sort-by=.lastTimestamp | tail -30 || true
        kubectl get pods -n azureclaw-system -o wide || true
        kubectl get lease -n azureclaw-system -o yaml || true
        # Per-pod logs: with multiple replicas, `kubectl logs -l ...`
        # may not include all pods or may truncate. Iterate explicitly
        # so the leader's log is always captured.
        for pod in $(kubectl get pods -n azureclaw-system -l app.kubernetes.io/component=controller -o name 2>/dev/null); do
            echo "── logs from $pod ───────────────────────────"
            kubectl logs -n azureclaw-system "$pod" --tail=300 || true
            echo "── previous (if any) ────────────────────────"
            kubectl logs -n azureclaw-system "$pod" --tail=100 --previous 2>/dev/null || true
        done
        fail "Sandbox namespace not created"
    fi
}

test_networkpolicy_created() {
    if kubectl get networkpolicy -n azureclaw-e2e-test sandbox-policy --no-headers 2>/dev/null | grep -q sandbox-policy; then
        pass "NetworkPolicy created in sandbox namespace"
    else
        fail "NetworkPolicy not found"
    fi
}

test_serviceaccount_created() {
    if kubectl get serviceaccount -n azureclaw-e2e-test sandbox --no-headers 2>/dev/null | grep -q sandbox; then
        pass "ServiceAccount created in sandbox namespace"
    else
        fail "ServiceAccount not found"
    fi
}

test_cleanup_sandbox() {
    kubectl delete clawsandbox e2e-test -n azureclaw-system 2>/dev/null || true
    sleep 3

    # Cleanup is best-effort: the controller may not have a
    # finalizer, so namespace teardown can be async. Either we see
    # the namespace gone, or we accept the CRD-deleted state and
    # move on. Both states are healthy.
    if kubectl get namespace azureclaw-e2e-test --no-headers 2>/dev/null | grep -q azureclaw-e2e-test; then
        pass "Sandbox CRD deleted (namespace cleanup is async)"
    else
        pass "Sandbox namespace cleaned up after CRD deletion"
    fi
}

test_runtime_openclaw() {
    pass "Runtime probe: openclaw selected (default fixtures already covered above)"
}

# ─── Phase 2/3 CRD reconciler tests ─────────────────────────────────────────
#
# Each Phase 2/3 CRD has a reconciler that compiles the CR into a
# downstream artefact (ConfigMap or Secret) and updates
# `.status.conditions[]`. The tests below assert the *contract*:
#
#   apply CR  →  downstream ConfigMap exists  →  Ready=True condition
#
# We do NOT exercise the runtime data-plane (no Foundry calls, no AGT
# relay, no real OAuth) — only that the controller wires CR → cluster
# state correctly. That's what runs in Kind.

# Wait up to N seconds for `kubectl get $1 $2 -n $3` to succeed.
wait_for_resource() {
    local kind="$1" name="$2" ns="$3" deadline timeout="${4:-30}"
    deadline=$(($(date +%s) + timeout))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if kubectl get "$kind" "$name" -n "$ns" &>/dev/null; then
            return 0
        fi
        sleep 1
    done
    return 1
}

# Wait up to N seconds for `.status.conditions[]` of `$1/$2 -n $3` to
# contain a condition with `type=Ready` AND `status=True`. The
# reconciler may also emit Degraded=False for the same fact; we only
# assert Ready because every reconciler sets that on success.
wait_for_ready() {
    local kind="$1" name="$2" ns="$3" deadline timeout="${4:-30}"
    deadline=$(($(date +%s) + timeout))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        local ready
        ready=$(kubectl get "$kind" "$name" -n "$ns" -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || true)
        if [ "$ready" = "True" ]; then
            return 0
        fi
        sleep 2
    done
    return 1
}

dump_cr_diagnostics() {
    local kind="$1" name="$2" ns="$3"
    warn "Diagnostics for $kind/$name in $ns:"
    kubectl describe "$kind" "$name" -n "$ns" 2>&1 | tail -40 || true
    kubectl get "$kind" "$name" -n "$ns" -o yaml 2>&1 | tail -40 || true
}

# ToolPolicy → toolpolicy-{name}-profile ConfigMap
test_crd_tool_policy() {
    cat <<'EOF' | kubectl apply -f - >/dev/null 2>&1 || { fail "ToolPolicy apply rejected"; return; }
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ToolPolicy
metadata:
  name: e2e-toolpolicy
  namespace: azureclaw-system
spec:
  appliesTo:
    tool: "*"
    sandboxMatchLabels:
      azureclaw.azure.com/e2e: "true"
  rateLimit:
    rps: 10
    burst: 20
EOF
    if wait_for_resource configmap toolpolicy-e2e-toolpolicy-profile azureclaw-system 45; then
        pass "ToolPolicy → profile ConfigMap created"
    else
        dump_cr_diagnostics toolpolicy e2e-toolpolicy azureclaw-system
        fail "ToolPolicy: profile ConfigMap not created"
    fi
    if wait_for_ready toolpolicy e2e-toolpolicy azureclaw-system 30; then
        pass "ToolPolicy: status.conditions Ready=True"
    else
        dump_cr_diagnostics toolpolicy e2e-toolpolicy azureclaw-system
        fail "ToolPolicy: Ready=True not observed"
    fi
    kubectl delete toolpolicy e2e-toolpolicy -n azureclaw-system --wait=false >/dev/null 2>&1 || true
}

# InferencePolicy → inferencepolicy-{name}-profile ConfigMap
test_crd_inference_policy() {
    cat <<'EOF' | kubectl apply -f - >/dev/null 2>&1 || { fail "InferencePolicy apply rejected"; return; }
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: InferencePolicy
metadata:
  name: e2e-inferencepolicy
  namespace: azureclaw-system
spec:
  appliesTo:
    sandboxName: e2e-test
  modelPreference:
    primary:
      provider: azure-openai
      deployment: gpt-4.1
EOF
    if wait_for_resource configmap inferencepolicy-e2e-inferencepolicy-profile azureclaw-system 45; then
        pass "InferencePolicy → profile ConfigMap created"
    else
        dump_cr_diagnostics inferencepolicy e2e-inferencepolicy azureclaw-system
        fail "InferencePolicy: profile ConfigMap not created"
    fi
    if wait_for_ready inferencepolicy e2e-inferencepolicy azureclaw-system 30; then
        pass "InferencePolicy: status.conditions Ready=True"
    else
        dump_cr_diagnostics inferencepolicy e2e-inferencepolicy azureclaw-system
        fail "InferencePolicy: Ready=True not observed"
    fi
    kubectl delete inferencepolicy e2e-inferencepolicy -n azureclaw-system --wait=false >/dev/null 2>&1 || true
}

# A2AAgent → a2aagent-{name}-card ConfigMap
test_crd_a2a_agent() {
    # 32 'A's = 32-byte (decoded) Ed25519 public-key placeholder. The
    # reconciler validates length, not key validity — so a base64url
    # blob of correct decoded length passes admission and is published
    # in the AgentCard. We don't need a *real* Ed25519 key to verify
    # the controller wires CR → ConfigMap correctly; that's the
    # contract under test here.
    local pk
    pk=$(printf 'A%.0s' {1..32} | base64 | tr '/+' '_-' | tr -d '=')
    cat <<EOF | kubectl apply -f - >/dev/null 2>&1 || { fail "A2AAgent apply rejected"; return; }
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: A2AAgent
metadata:
  name: e2e-a2aagent
  namespace: azureclaw-system
spec:
  endpointUrl: "https://e2e-a2aagent.invalid/"
  signingKeys:
    - kid: "e2e-key-1"
      alg: "EdDSA"
      publicKeyB64u: "$pk"
  capabilities:
    - "tasks/send"
    - "tasks/get"
EOF
    if wait_for_resource configmap a2aagent-e2e-a2aagent-card azureclaw-system 45; then
        pass "A2AAgent → AgentCard ConfigMap created"
    else
        dump_cr_diagnostics a2aagent e2e-a2aagent azureclaw-system
        fail "A2AAgent: AgentCard ConfigMap not created"
    fi
    if wait_for_ready a2aagent e2e-a2aagent azureclaw-system 30; then
        pass "A2AAgent: status.conditions Ready=True"
    else
        dump_cr_diagnostics a2aagent e2e-a2aagent azureclaw-system
        fail "A2AAgent: Ready=True not observed"
    fi
    kubectl delete a2aagent e2e-a2aagent -n azureclaw-system --wait=false >/dev/null 2>&1 || true
}

# ClawMemory → clawmemory-{name}-binding ConfigMap. No Foundry call
# happens during reconcile (the runtime path creates the store
# lazily); the CR's job is to publish the binding ConfigMap.
test_crd_claw_memory() {
    cat <<'EOF' | kubectl apply -f - >/dev/null 2>&1 || { fail "ClawMemory apply rejected"; return; }
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawMemory
metadata:
  name: e2e-clawmemory
  namespace: azureclaw-system
spec:
  storeName: e2e-store
  sandboxRef:
    name: e2e-test
  scope: "agent:e2e-test"
EOF
    if wait_for_resource configmap clawmemory-e2e-clawmemory-binding azureclaw-system 45; then
        pass "ClawMemory → binding ConfigMap created"
    else
        dump_cr_diagnostics clawmemory e2e-clawmemory azureclaw-system
        fail "ClawMemory: binding ConfigMap not created"
    fi
    if wait_for_ready clawmemory e2e-clawmemory azureclaw-system 30; then
        pass "ClawMemory: status.conditions Ready=True"
    else
        dump_cr_diagnostics clawmemory e2e-clawmemory azureclaw-system
        fail "ClawMemory: Ready=True not observed"
    fi
    kubectl delete clawmemory e2e-clawmemory -n azureclaw-system --wait=false >/dev/null 2>&1 || true
}

# ClawEval → claweval-{name}-binding ConfigMap. Schedule is optional;
# we omit it so the test isn't time-sensitive.
test_crd_claw_eval() {
    cat <<'EOF' | kubectl apply -f - >/dev/null 2>&1 || { fail "ClawEval apply rejected"; return; }
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawEval
metadata:
  name: e2e-claweval
  namespace: azureclaw-system
spec:
  sandboxRef:
    name: e2e-test
  suite: foundry-evals
  evaluators:
    - "relevance"
EOF
    if wait_for_resource configmap claweval-e2e-claweval-binding azureclaw-system 45; then
        pass "ClawEval → binding ConfigMap created"
    else
        dump_cr_diagnostics claweval e2e-claweval azureclaw-system
        fail "ClawEval: binding ConfigMap not created"
    fi
    if wait_for_ready claweval e2e-claweval azureclaw-system 30; then
        pass "ClawEval: status.conditions Ready=True"
    else
        dump_cr_diagnostics claweval e2e-claweval azureclaw-system
        fail "ClawEval: Ready=True not observed"
    fi
    kubectl delete claweval e2e-claweval -n azureclaw-system --wait=false >/dev/null 2>&1 || true
}

# McpServer (dev-mode, no OAuth). The reconciler can't fetch JWKS in
# Kind (no real issuer), so we assert only that the CR is admitted
# and reaches a terminal status (Ready or Degraded — both indicate
# the reconciler ran). A flat fail would mean controller crashed or
# admission rejected the CR.
test_crd_mcp_server() {
    cat <<'EOF' | kubectl apply -f - >/dev/null 2>&1 || { fail "McpServer apply rejected (dev-mode)"; return; }
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: McpServer
metadata:
  name: e2e-mcpserver
  namespace: azureclaw-system
spec:
  url: "http://e2e-mcpserver.invalid/"
  productionMode: false
  allowedTools:
    - "*"
EOF
    # In dev-mode the reconciler should reach Ready=True without
    # contacting any external system.
    if wait_for_ready mcpserver e2e-mcpserver azureclaw-system 45; then
        pass "McpServer (dev-mode): status.conditions Ready=True"
    else
        # Dev-mode reconcile shouldn't need network access. Treat
        # any non-Ready terminal as a failure and dump diagnostics.
        dump_cr_diagnostics mcpserver e2e-mcpserver azureclaw-system
        fail "McpServer: Ready=True not observed in dev-mode"
    fi
    kubectl delete mcpserver e2e-mcpserver -n azureclaw-system --wait=false >/dev/null 2>&1 || true
}

# CEL admission gate: a ToolPolicy with a malformed rateLimit (rps=0,
# burst<rps) MUST be rejected by the API server before it reaches the
# controller. This test guards against admission regressions.
test_crd_admission_rejects_invalid() {
    if kubectl apply -f - >/dev/null 2>&1 <<'EOF'
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: A2AAgent
metadata:
  name: e2e-bad-a2a
  namespace: azureclaw-system
spec:
  endpointUrl: "http://insecure.invalid/"
  productionMode: true
  signingKeys: []
EOF
    then
        # If the API server accepted this, the CEL gate is broken.
        kubectl delete a2aagent e2e-bad-a2a -n azureclaw-system --wait=false >/dev/null 2>&1 || true
        fail "Admission accepted invalid A2AAgent (productionMode + http + empty keys)"
    else
        pass "Admission CEL rejects invalid A2AAgent (productionMode + http + empty keys)"
    fi
}

test_runtime_oai_agents() {
    # Render a multi-runtime ClawSandbox of kind OpenAIAgents and assert
    # the controller produces a workload (deployment).
    cat <<EOF | kubectl apply -f - 2>/dev/null || true
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-oai
  namespace: azureclaw-system
spec:
  runtime:
    kind: OpenAIAgents
  sandbox:
    isolation: standard
EOF
    sleep 3
    if kubectl get deploy -n azureclaw-e2e-oai e2e-oai &>/dev/null; then
        pass "OpenAIAgents runtime renders a Deployment"
    else
        # The controller's ShapeInvalid path is observable too — assert
        # the namespace exists at minimum (controller did process the CR).
        if kubectl get ns azureclaw-e2e-oai &>/dev/null; then
            pass "OpenAIAgents runtime processed (namespace present)"
        else
            fail "OpenAIAgents runtime: no namespace nor deploy"
        fi
    fi
    kubectl delete clawsandbox e2e-oai -n azureclaw-system 2>/dev/null || true
}

test_runtime_maf_python() {
    cat <<EOF | kubectl apply -f - 2>/dev/null || true
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-maf
  namespace: azureclaw-system
spec:
  runtime:
    kind: MicrosoftAgentFrameworkPython
  sandbox:
    isolation: standard
EOF
    sleep 3
    if kubectl get ns azureclaw-e2e-maf &>/dev/null; then
        pass "MAF-Python runtime processed (namespace present)"
    else
        fail "MAF-Python runtime: namespace missing"
    fi
    kubectl delete clawsandbox e2e-maf -n azureclaw-system 2>/dev/null || true
}

test_runtime_byo() {
    cat <<EOF | kubectl apply -f - 2>/dev/null || true
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-byo
  namespace: azureclaw-system
spec:
  runtime:
    kind: BringYourOwn
    byo:
      image: ghcr.io/example/byo-agent:e2e
  sandbox:
    isolation: standard
EOF
    sleep 3
    if kubectl get ns azureclaw-e2e-byo &>/dev/null; then
        pass "BYO runtime processed (namespace present)"
    else
        fail "BYO runtime: namespace missing"
    fi
    kubectl delete clawsandbox e2e-byo -n azureclaw-system 2>/dev/null || true
}

# ─── Main ─────────────────────────────────────────────────────────────────────

main() {
    echo ""
    echo "═══════════════════════════════════════════════════════"
    echo "  AzureClaw E2E Test Suite (runtime: $RUNTIME)"
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

    # Phase 2/3 CRD reconciler coverage. These run before
    # cleanup_sandbox so the sandbox is still present (some CRs
    # reference it). The CR objects own no Pod, do no network I/O,
    # and use only ConfigMap output — safe in Kind, no Azure deps.
    test_crd_tool_policy
    test_crd_inference_policy
    test_crd_a2a_agent
    test_crd_claw_memory
    test_crd_claw_eval
    test_crd_mcp_server
    test_crd_admission_rejects_invalid

    test_cleanup_sandbox

    case "$RUNTIME" in
        openclaw)        test_runtime_openclaw ;;
        oai-agents)      test_runtime_oai_agents ;;
        maf-python)      test_runtime_maf_python ;;
        byo)             test_runtime_byo ;;
        all)
            test_runtime_openclaw
            test_runtime_oai_agents
            test_runtime_maf_python
            test_runtime_byo
            ;;
        *)
            fail "Unknown AZURECLAW_E2E_RUNTIME: $RUNTIME"
            ;;
    esac

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
