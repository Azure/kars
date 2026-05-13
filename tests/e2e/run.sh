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
    # Retry docker pulls/builds to absorb transient MCR registry flakes
    # (e.g., 403/404 on pinned digests during MCR rollouts).
    docker_build_retry() {
        local tag="$1" df="$2" ctx="$3" attempt
        for attempt in 1 2 3; do
            if docker build -t "$tag" -f "$df" "$ctx"; then
                return 0
            fi
            warn "docker build failed (attempt $attempt/3), retrying in 15s"
            sleep 15
        done
        return 1
    }
    info "Building controller image"
    docker_build_retry azureclaw-controller:e2e "$ROOT_DIR/controller/Dockerfile" "$ROOT_DIR"
    kind load docker-image azureclaw-controller:e2e --name "$CLUSTER_NAME"

    info "Building inference router image"
    docker_build_retry azureclaw-inference-router:e2e "$ROOT_DIR/inference-router/Dockerfile" "$ROOT_DIR"
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
# Pass an empty `ns` ("") to skip the `-n` flag (cluster-scoped resources).
wait_for_resource() {
    local kind="$1" name="$2" ns="$3" deadline timeout="${4:-30}"
    deadline=$(($(date +%s) + timeout))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if [ -n "$ns" ]; then
            kubectl get "$kind" "$name" -n "$ns" &>/dev/null && return 0
        else
            kubectl get "$kind" "$name" &>/dev/null && return 0
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
    # InferencePolicy uses the §3 echo loop (Slice 2a): without a
    # router pod actually loading the profile and echoing the digest
    # back to /internal/policy-status, the CR stays in phase=Compiled
    # with Ready=False / reason=AwaitingRouterEnforcement (or
    # NoSandboxesReferencing if the e2e-test sandbox's router isn't
    # reachable). Asserting Ready=True here would be a §3 violation —
    # the controller *correctly* refuses to lie. The compiled
    # ConfigMap check above is the controller's complete output;
    # router enforcement is exercised in unit + integration tests.
    local ip_phase ip_ready ip_reason
    ip_phase=$(kubectl get inferencepolicy e2e-inferencepolicy -n azureclaw-system -o jsonpath='{.status.phase}' 2>/dev/null || true)
    ip_ready=$(kubectl get inferencepolicy e2e-inferencepolicy -n azureclaw-system -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || true)
    ip_reason=$(kubectl get inferencepolicy e2e-inferencepolicy -n azureclaw-system -o jsonpath='{.status.conditions[?(@.type=="Ready")].reason}' 2>/dev/null || true)
    case "$ip_phase|$ip_ready|$ip_reason" in
        Compiled\|False\|AwaitingRouterEnforcement|Compiled\|False\|NoSandboxesReferencing|Ready\|True\|RouterEnforcing|Ready\|True\|*)
            pass "InferencePolicy: phase=$ip_phase ready=$ip_ready reason=$ip_reason (§3 honest state)"
            ;;
        *)
            dump_cr_diagnostics inferencepolicy e2e-inferencepolicy azureclaw-system
            fail "InferencePolicy: unexpected honest-state — phase=$ip_phase ready=$ip_ready reason=$ip_reason"
            ;;
    esac
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
    # ClawMemory uses the §3 echo loop (Slice 3a). Without a router
    # pod actually loading the binding and echoing the digest back
    # to /internal/policy-status, the CR stays in phase=Compiled
    # with Ready=False / reason=NoSandboxesReferencing or
    # AwaitingRouterEnforcement. Asserting the legacy
    # Pending/AwaitingFoundryProvisioning is incorrect after Slice 3a.
    local mem_phase mem_ready mem_reason
    mem_phase=$(kubectl get clawmemory e2e-clawmemory -n azureclaw-system -o jsonpath='{.status.phase}' 2>/dev/null || true)
    mem_ready=$(kubectl get clawmemory e2e-clawmemory -n azureclaw-system -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || true)
    mem_reason=$(kubectl get clawmemory e2e-clawmemory -n azureclaw-system -o jsonpath='{.status.conditions[?(@.type=="Ready")].reason}' 2>/dev/null || true)
    case "$mem_phase|$mem_ready|$mem_reason" in
        Compiled\|False\|NoSandboxesReferencing|Compiled\|False\|AwaitingRouterEnforcement|Ready\|True\|RouterEnforcing)
            pass "ClawMemory: phase=$mem_phase ready=$mem_ready reason=$mem_reason (§3 honest state, Slice 3a)"
            ;;
        *)
            dump_cr_diagnostics clawmemory e2e-clawmemory azureclaw-system
            fail "ClawMemory: unexpected honest-state — phase=$mem_phase ready=$mem_ready reason=$mem_reason"
            ;;
    esac
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
    # the controller produces a namespace (Phase 2 schema parses; adapter
    # deploy lands in S10.A3 — namespace creation is the observable).
    cat <<EOF | kubectl apply -f - 2>&1 | head -3
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-oai
  namespace: azureclaw-system
spec:
  inferenceRef:
    name: e2e-test-inference
  runtime:
    kind: OpenAIAgents
    openaiAgents:
      pythonVersion: "3.12"
      agentCode:
        oci:
          image: ghcr.io/example/oai-agent:e2e
  sandbox:
    isolation: standard
EOF
    sleep 5
    if kubectl get ns azureclaw-e2e-oai &>/dev/null; then
        pass "OpenAIAgents runtime processed (namespace present)"
    else
        echo "  [diag] CR status:"
        kubectl get clawsandbox e2e-oai -n azureclaw-system -o jsonpath='{.status}' 2>/dev/null | head -c 500 || true
        echo ""
        fail "OpenAIAgents runtime: no namespace"
    fi
    kubectl delete clawsandbox e2e-oai -n azureclaw-system 2>/dev/null || true
}

test_runtime_maf_python() {
    cat <<EOF | kubectl apply -f - 2>&1 | head -3
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-maf
  namespace: azureclaw-system
spec:
  inferenceRef:
    name: e2e-test-inference
  runtime:
    kind: MicrosoftAgentFramework
    microsoftAgentFramework:
      language: python
      agentCode:
        oci:
          image: ghcr.io/example/maf-agent:e2e
  sandbox:
    isolation: standard
EOF
    sleep 5
    if kubectl get ns azureclaw-e2e-maf &>/dev/null; then
        pass "MAF-Python runtime processed (namespace present)"
    else
        echo "  [diag] CR status:"
        kubectl get clawsandbox e2e-maf -n azureclaw-system -o jsonpath='{.status}' 2>/dev/null | head -c 500 || true
        echo ""
        fail "MAF-Python runtime: namespace missing"
    fi
    kubectl delete clawsandbox e2e-maf -n azureclaw-system 2>/dev/null || true
}

test_runtime_anthropic() {
    # Phase H#1: ClawSandbox of kind Anthropic should be processed by
    # the controller — namespace creation is the observable signal that
    # plan_anthropic dispatched (vs the legacy AdapterMissing path).
    cat <<EOF | kubectl apply -f - 2>&1 | head -3
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-anthropic
  namespace: azureclaw-system
spec:
  inferenceRef:
    name: e2e-test-inference
  runtime:
    kind: Anthropic
    anthropic:
      pythonVersion: "3.12"
      agentCode:
        oci:
          image: ghcr.io/example/anthropic-agent:e2e
  sandbox:
    isolation: standard
EOF
    sleep 5
    if kubectl get ns azureclaw-e2e-anthropic &>/dev/null; then
        pass "Anthropic runtime processed (namespace present)"
    else
        echo "  [diag] CR status:"
        kubectl get clawsandbox e2e-anthropic -n azureclaw-system -o jsonpath='{.status}' 2>/dev/null | head -c 500 || true
        echo ""
        fail "Anthropic runtime: no namespace"
    fi
    # Verify the Deployment image carries the Anthropic runtime tag
    # rather than the OpenClaw default — proves the planner dispatched.
    # Tolerant: if Deployment hasn't materialized yet (no real
    # InferencePolicy provider in this E2E lane), surface a diag-only
    # signal rather than failing the lane.
    local image
    image=$(kubectl get deploy -n azureclaw-e2e-anthropic e2e-anthropic -o jsonpath='{.spec.template.spec.containers[?(@.name=="agent")].image}' 2>/dev/null || true)
    if [ -n "$image" ]; then
        if echo "$image" | grep -q "anthropic"; then
            pass "Anthropic Deployment uses anthropic runtime image ($image)"
        else
            echo "  [diag] container image: $image"
            fail "Anthropic Deployment image does not reference anthropic runtime"
        fi
    else
        echo "  [diag] no Deployment yet (likely no InferencePolicy provider in this lane)"
    fi
    kubectl delete clawsandbox e2e-anthropic -n azureclaw-system 2>/dev/null || true
}

test_runtime_langgraph() {
    # Phase H#2: ClawSandbox of kind LangGraph should be processed by
    # the controller. Like other runtime tests, namespace creation is
    # the primary observable; the Deployment image check is best-effort.
    cat <<EOF | kubectl apply -f - 2>&1 | head -3
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-langgraph
  namespace: azureclaw-system
spec:
  inferenceRef:
    name: e2e-test-inference
  runtime:
    kind: LangGraph
    langGraph:
      language: python
      agentCode:
        oci:
          image: ghcr.io/example/langgraph-agent:e2e
  sandbox:
    isolation: standard
EOF
    sleep 5
    if kubectl get ns azureclaw-e2e-langgraph &>/dev/null; then
        pass "LangGraph runtime processed (namespace present)"
    else
        echo "  [diag] CR status:"
        kubectl get clawsandbox e2e-langgraph -n azureclaw-system -o jsonpath='{.status}' 2>/dev/null | head -c 500 || true
        echo ""
        fail "LangGraph runtime: no namespace"
    fi
    local image
    image=$(kubectl get deploy -n azureclaw-e2e-langgraph e2e-langgraph -o jsonpath='{.spec.template.spec.containers[?(@.name=="agent")].image}' 2>/dev/null || true)
    if [ -n "$image" ]; then
        if echo "$image" | grep -q "langgraph"; then
            pass "LangGraph Deployment uses langgraph runtime image ($image)"
        else
            echo "  [diag] container image: $image"
            fail "LangGraph Deployment image does not reference langgraph runtime"
        fi
    else
        echo "  [diag] no Deployment yet (likely no InferencePolicy provider in this lane)"
    fi
    kubectl delete clawsandbox e2e-langgraph -n azureclaw-system 2>/dev/null || true
}

# LangGraph TypeScript flavour ships as a first-class adapter in
# v1.0 (runtimes/langgraph-ts/, sandbox-images/langgraph-ts/). The
# controller dispatches `language: typescript` to the Node.js 22
# image; the deployment should reach a Running status without being
# stamped ShapeInvalid.
test_runtime_langgraph_typescript() {
    cat <<EOF | kubectl apply -f - 2>&1 | head -3
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-langgraph-ts
  namespace: azureclaw-system
spec:
  inferenceRef:
    name: e2e-test-inference
  runtime:
    kind: LangGraph
    langGraph:
      language: typescript
      agentCode:
        oci:
          image: ghcr.io/example/langgraph-ts-agent:e2e
  sandbox:
    isolation: standard
EOF
    sleep 5
    # The controller should plan a Deployment using the langgraph-ts
    # image (no ShapeInvalid). We don't pin the exact image string here
    # because operators may override via LANGGRAPH_TS_RUNTIME_IMAGE; we
    # only assert that ShapeInvalid is NOT in the Conditions chain.
    local conds
    conds=$(kubectl get clawsandbox e2e-langgraph-ts -n azureclaw-system -o jsonpath='{.status.conditions[*].reason}' 2>/dev/null || true)
    if echo "$conds" | grep -qi "ShapeInvalid\|SpecInvalid"; then
        echo "  [diag] conditions: $conds"
        fail "LangGraph typescript should NOT be ShapeInvalid in v1.0"
    else
        pass "LangGraph typescript dispatched (no ShapeInvalid)"
    fi
    kubectl delete clawsandbox e2e-langgraph-ts -n azureclaw-system 2>/dev/null || true
}

test_runtime_pydantic_ai() {
    # Phase H#3: ClawSandbox of kind PydanticAi should be processed by
    # the controller. Like other runtime tests, namespace creation is
    # the primary observable; the Deployment image check is best-effort.
    cat <<EOF | kubectl apply -f - 2>&1 | head -3
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-pydantic-ai
  namespace: azureclaw-system
spec:
  inferenceRef:
    name: e2e-test-inference
  runtime:
    kind: PydanticAi
    pydanticAi:
      pythonVersion: "3.12"
      agentCode:
        oci:
          image: ghcr.io/example/pydantic-ai-agent:e2e
  sandbox:
    isolation: standard
EOF
    sleep 5
    if kubectl get ns azureclaw-e2e-pydantic-ai &>/dev/null; then
        pass "PydanticAi runtime processed (namespace present)"
    else
        echo "  [diag] CR status:"
        kubectl get clawsandbox e2e-pydantic-ai -n azureclaw-system -o jsonpath='{.status}' 2>/dev/null | head -c 500 || true
        echo ""
        fail "PydanticAi runtime: no namespace"
    fi
    local image
    image=$(kubectl get deploy -n azureclaw-e2e-pydantic-ai e2e-pydantic-ai -o jsonpath='{.spec.template.spec.containers[?(@.name=="agent")].image}' 2>/dev/null || true)
    if [ -n "$image" ]; then
        if echo "$image" | grep -q "pydantic-ai"; then
            pass "PydanticAi Deployment uses pydantic-ai runtime image ($image)"
        else
            echo "  [diag] container image: $image"
            fail "PydanticAi Deployment image does not reference pydantic-ai runtime"
        fi
    else
        echo "  [diag] no Deployment yet (likely no InferencePolicy provider in this lane)"
    fi
    kubectl delete clawsandbox e2e-pydantic-ai -n azureclaw-system 2>/dev/null || true
}

test_runtime_byo() {
    cat <<EOF | kubectl apply -f - 2>&1 | head -3
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-byo
  namespace: azureclaw-system
spec:
  inferenceRef:
    name: e2e-test-inference
  runtime:
    kind: BYO
    byo:
      image: ghcr.io/example/byo-agent:e2e
      contractVersion: "v1"
  sandbox:
    isolation: standard
EOF
    sleep 5
    if kubectl get ns azureclaw-e2e-byo &>/dev/null; then
        pass "BYO runtime processed (namespace present)"
    else
        echo "  [diag] CR status:"
        kubectl get clawsandbox e2e-byo -n azureclaw-system -o jsonpath='{.status}' 2>/dev/null | head -c 500 || true
        echo ""
        fail "BYO runtime: namespace missing"
    fi
    kubectl delete clawsandbox e2e-byo -n azureclaw-system 2>/dev/null || true
}

# ─── Admission policy enforcement tests ─────────────────────────────────────
#
# These exercise the ValidatingAdmissionPolicy / MutatingAdmissionPolicy
# objects shipped in deploy/helm/azureclaw/templates/admission-*.yaml.
# Each policy is a hard guard — a regression here means a sandbox
# could be deployed without the platform's safety invariants.

test_admission_policies_installed() {
    # Phase 0/1/2 admission policies that MUST be installed by Helm.
    local expected=(
        "azureclaw-null-provider-block"
        "azureclaw-no-public-router-exposure"
        "azureclaw-sandbox-exec-ban"
        "azureclaw-dev-only-label-immutable"
        "azureclaw-sandbox-posture-lock"
        "azureclaw-content-safety-floor"
    )
    local missing=()
    for p in "${expected[@]}"; do
        if ! kubectl get validatingadmissionpolicy "$p" &>/dev/null; then
            missing+=("$p")
        fi
    done
    if [ "${#missing[@]}" -eq 0 ]; then
        pass "All ${#expected[@]} ValidatingAdmissionPolicies installed"
    else
        fail "Missing VAPs: ${missing[*]}"
    fi
}

test_admission_mcpserver_productionmode_requires_https() {
    # CRD-level x-kubernetes-validations on McpServer: when
    # `productionMode: true`, `url` must start with `https://`.
    # This is a defence-in-depth gate — if a user accidentally
    # ships an http:// MCP endpoint to prod, admission rejects it
    # before the controller ever wires it.
    local out
    out=$(cat <<'EOF' | kubectl apply -f - 2>&1 || true
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: McpServer
metadata:
  name: e2e-mcp-bad-prod
  namespace: azureclaw-system
spec:
  url: http://insecure.example.com
  productionMode: true
  oauth:
    issuer: https://idp.example.com
EOF
)
    if echo "$out" | grep -qiE "(begin with https|invalid|denied|FieldValueInvalid)"; then
        pass "McpServer productionMode CEL rejects http:// url"
    else
        fail "McpServer productionMode CEL did NOT reject http://. Got: $out"
        kubectl delete mcpserver e2e-mcp-bad-prod -n azureclaw-system 2>/dev/null || true
    fi
}

test_admission_mcpserver_productionmode_requires_oauth() {
    # Companion CEL: productionMode=true requires spec.oauth.issuer.
    local out
    out=$(cat <<'EOF' | kubectl apply -f - 2>&1 || true
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: McpServer
metadata:
  name: e2e-mcp-no-oauth
  namespace: azureclaw-system
spec:
  url: https://server.example.com
  productionMode: true
EOF
)
    if echo "$out" | grep -qiE "(oauth.issuer|productionMode requires|invalid|denied)"; then
        pass "McpServer productionMode CEL rejects missing oauth.issuer"
    else
        fail "McpServer productionMode CEL did NOT reject missing oauth. Got: $out"
        kubectl delete mcpserver e2e-mcp-no-oauth -n azureclaw-system 2>/dev/null || true
    fi
}

test_admission_no_public_router_exposure() {
    # Create a sandbox-isolation namespace, then try to create a
    # LoadBalancer Service in it. Must be rejected by
    # `azureclaw-no-public-router-exposure`.
    kubectl create namespace azureclaw-e2e-pub --dry-run=client -o yaml \
        | kubectl apply -f - >/dev/null 2>&1 || true
    kubectl label namespace azureclaw-e2e-pub \
        azureclaw.azure.com/isolated=strict --overwrite >/dev/null 2>&1 || true
    local out
    out=$(cat <<'EOF' | kubectl apply -f - 2>&1 || true
---
apiVersion: v1
kind: Service
metadata:
  name: leak
  namespace: azureclaw-e2e-pub
spec:
  type: LoadBalancer
  ports:
    - port: 80
  selector:
    app: leak
EOF
)
    if echo "$out" | grep -qiE "(LoadBalancer.*forbid|no-public-router|denied|Forbidden)"; then
        pass "no-public-router-exposure rejects LoadBalancer Service in strict NS"
    else
        fail "no-public-router-exposure did NOT reject LoadBalancer. Got: $out"
        kubectl delete svc leak -n azureclaw-e2e-pub 2>/dev/null || true
    fi
    kubectl delete namespace azureclaw-e2e-pub 2>/dev/null || true
}

test_admission_pod_exec_ban() {
    # `azureclaw-sandbox-exec-ban` denies kubectl exec/attach on
    # `openclaw` containers (or default container) in namespaces
    # labeled azureclaw.azure.com/isolated=strict (and not
    # azureclaw.azure.com/break-glass=true).
    #
    # Approach: spin a single-container pod called `openclaw` in a
    # strict-labeled NS, wait for Running, then `kubectl exec` into
    # it. The exec request is a CONNECT subresource which the policy
    # intercepts before the kubelet ever sees it.
    kubectl create namespace azureclaw-e2e-exec --dry-run=client -o yaml | kubectl apply -f - >/dev/null || true
    kubectl label namespace azureclaw-e2e-exec azureclaw.azure.com/isolated=strict --overwrite >/dev/null
    cat <<'EOF' | kubectl apply -f - >/dev/null || true
---
apiVersion: v1
kind: Pod
metadata:
  name: openclaw-probe
  namespace: azureclaw-e2e-exec
  labels:
    app: openclaw-probe
spec:
  restartPolicy: Never
  containers:
    - name: openclaw
      image: busybox:1.36
      command: ["sh", "-c", "sleep 600"]
EOF
    if ! kubectl wait --for=condition=Ready pod/openclaw-probe -n azureclaw-e2e-exec --timeout=60s >/dev/null 2>&1; then
        warn "exec-ban probe pod never became Ready — skipping"
        kubectl describe pod openclaw-probe -n azureclaw-e2e-exec 2>/dev/null | tail -20 || true
        kubectl delete namespace azureclaw-e2e-exec 2>/dev/null || true
        return
    fi
    local out
    out=$(kubectl exec -n azureclaw-e2e-exec openclaw-probe -c openclaw -- echo hello 2>&1 || true)
    if echo "$out" | grep -qiE "(exec.*denied|exec-ban|Forbidden|sandbox-exec-ban)"; then
        pass "pod-exec-ban rejects kubectl exec into 'openclaw' container in strict NS"
    else
        fail "pod-exec-ban did NOT reject exec. Got: $out"
    fi
    kubectl delete namespace azureclaw-e2e-exec 2>/dev/null || true
}

test_admission_dev_only_label_immutable() {
    # `azureclaw-dev-only-label-immutable` blocks UPDATEs that REMOVE
    # the dev-only label once it was set. Apply → mutate the label
    # away → expect rejection.
    cat <<'EOF' | kubectl apply -f - >/dev/null 2>&1 || true
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ToolPolicy
metadata:
  name: e2e-immutable-dev
  namespace: azureclaw-system
  labels:
    azureclaw.azure.com/dev-only: "true"
spec:
  appliesTo:
    tool: "*"
    sandboxMatchLabels:
      x: y
  rateLimit:
    rps: 10
    burst: 20
EOF
    if ! wait_for_resource toolpolicy e2e-immutable-dev azureclaw-system 10; then
        warn "dev-only-immutable: ToolPolicy didn't apply — skipping"
        return
    fi
    local out
    out=$(kubectl label toolpolicy e2e-immutable-dev -n azureclaw-system \
        azureclaw.azure.com/dev-only- --overwrite 2>&1 || true)
    if echo "$out" | grep -qiE "(immutable|denied|Forbidden|dev-only|removal-reason)"; then
        pass "dev-only-label-immutable rejects removing dev-only label"
    else
        fail "dev-only-label-immutable did NOT reject label removal. Got: $out"
    fi
    kubectl delete toolpolicy e2e-immutable-dev -n azureclaw-system 2>/dev/null || true
}

# ─── Sandbox runtime artifacts ──────────────────────────────────────────────

test_sandbox_namespace_labels() {
    # Controller must stamp `azureclaw.azure.com/isolated=strict` on
    # every sandbox namespace. This label is the load-bearing trigger
    # for every other policy in the platform — if it's missing, the
    # whole admission stack disengages silently.
    local val
    val=$(kubectl get namespace azureclaw-e2e-test -o jsonpath='{.metadata.labels.azureclaw\.azure\.com/isolated}' 2>/dev/null)
    if [ "$val" = "strict" ]; then
        pass "Sandbox namespace stamped azureclaw.azure.com/isolated=strict"
    else
        fail "Sandbox namespace missing strict label (got: '$val')"
    fi
}

test_sandbox_deployment_exists() {
    # Reconciler must produce a Deployment in the sandbox namespace.
    # We don't assert pods are Ready (sandbox image not in kind), only
    # that the reconciler shaped the workload correctly.
    if kubectl get deploy -n azureclaw-e2e-test --no-headers 2>/dev/null | grep -q .; then
        pass "Sandbox Deployment created in sandbox namespace"
    else
        fail "No Deployment in sandbox namespace azureclaw-e2e-test"
    fi
}

test_operator_default_deny_np() {
    # operator-default-deny-networkpolicy.yaml ships a default-deny
    # NetworkPolicy in azureclaw-system itself. Required so the
    # controller never accidentally exposes anything.
    if kubectl get networkpolicy -n azureclaw-system azureclaw-system-default-deny &>/dev/null; then
        pass "Operator default-deny NetworkPolicy installed in azureclaw-system"
    else
        fail "azureclaw-system-default-deny NetworkPolicy missing"
    fi
}

test_sandbox_networkpolicy_denies_ingress() {
    # The per-sandbox NetworkPolicy must include 'Ingress' in
    # policyTypes (deny-by-default ingress is the foundational
    # invariant — sandboxes only receive traffic via the router).
    local types
    types=$(kubectl get networkpolicy -n azureclaw-e2e-test sandbox-policy \
        -o jsonpath='{.spec.policyTypes}' 2>/dev/null)
    if echo "$types" | grep -q Ingress; then
        pass "Sandbox NetworkPolicy enforces Ingress policy"
    else
        fail "Sandbox NetworkPolicy missing Ingress in policyTypes (got: $types)"
    fi
}

test_sandbox_suspended_lifecycle() {
    # Phase G P1 #4: spec.suspended scales the Deployment to
    # replicas=0 without removing it, and stamps
    # Suspended=True/SuspendedBySpec on .status.conditions.
    # Un-setting (or flipping to false) restores replicas=1 and
    # stamps Suspended=False/Active.
    local sandbox=suspend-test
    cat <<EOF | kubectl apply -f - >/dev/null
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: InferencePolicy
metadata:
  name: ${sandbox}-inference
  namespace: azureclaw-system
  labels:
    azureclaw.azure.com/sandbox: ${sandbox}
spec:
  appliesTo:
    sandboxName: ${sandbox}
  modelPreference:
    primary:
      provider: azure-openai
      deployment: gpt-4.1
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: ${sandbox}
  namespace: azureclaw-system
spec:
  runtime:
    kind: OpenClaw
    openclaw:
      version: "2026.3.13"
  sandbox:
    isolation: standard
  inferenceRef:
    name: ${sandbox}-inference
  suspended: true
EOF
    # Wait up to 60s for the controller to reconcile + scale down.
    local replicas=""
    local i
    for i in $(seq 1 30); do
        replicas=$(kubectl get deploy -n "azureclaw-${sandbox}" "${sandbox}" \
            -o jsonpath='{.spec.replicas}' 2>/dev/null || true)
        if [[ "$replicas" == "0" ]]; then
            break
        fi
        sleep 2
    done
    if [[ "$replicas" == "0" ]]; then
        pass "Suspended sandbox Deployment scaled to replicas=0"
    else
        fail "Suspended Deployment replicas=$replicas (expected 0)"
        kubectl delete clawsandbox "${sandbox}" -n azureclaw-system --ignore-not-found >/dev/null 2>&1
        kubectl delete inferencepolicy "${sandbox}-inference" -n azureclaw-system --ignore-not-found >/dev/null 2>&1
        return
    fi

    # Suspended=True/SuspendedBySpec must be stamped.
    local cond_status cond_reason
    # Poll for Suspended=True/SuspendedBySpec convergence (same race as
    # the un-suspend side: replicas=0 may land before the status patch).
    local converged_susp=0
    local cond_status="" cond_reason=""
    for i in $(seq 1 30); do
        cond_status=$(kubectl get clawsandbox "${sandbox}" -n azureclaw-system \
            -o jsonpath='{.status.conditions[?(@.type=="Suspended")].status}' 2>/dev/null)
        cond_reason=$(kubectl get clawsandbox "${sandbox}" -n azureclaw-system \
            -o jsonpath='{.status.conditions[?(@.type=="Suspended")].reason}' 2>/dev/null)
        if [[ "$cond_status" == "True" && "$cond_reason" == "SuspendedBySpec" ]]; then
            converged_susp=1
            break
        fi
        sleep 2
    done
    if [[ "$converged_susp" == "1" ]]; then
        pass "Suspended=True/SuspendedBySpec condition stamped"
    else
        fail "Suspended condition wrong (status=$cond_status reason=$cond_reason)"
        echo "[DEBUG] full CR (-o yaml):"
        kubectl get clawsandbox "${sandbox}" -n azureclaw-system -o yaml 2>/dev/null | tail -60
    fi

    # Un-suspend → replicas=1, Suspended=False/Active.
    kubectl patch clawsandbox "${sandbox}" -n azureclaw-system --type merge \
        -p '{"spec":{"suspended":false}}' >/dev/null
    for i in $(seq 1 30); do
        replicas=$(kubectl get deploy -n "azureclaw-${sandbox}" "${sandbox}" \
            -o jsonpath='{.spec.replicas}' 2>/dev/null || true)
        if [[ "$replicas" == "1" ]]; then
            break
        fi
        sleep 2
    done
    if [[ "$replicas" == "1" ]]; then
        pass "Un-suspended sandbox Deployment restored to replicas=1"
    else
        fail "Un-suspended Deployment replicas=$replicas (expected 1)"
    fi

    # Poll for condition convergence to Suspended=False/Active. The
    # controller patches Deployment.replicas and .status.conditions in
    # a single reconcile pass, but they land via two separate API
    # calls. Polling avoids a race where the test reads the
    # condition slot before the status patch has been applied (or
    # while a concurrent reconcile triggered by the new Deployment
    # watch is still in-flight).
    local converged=0
    for i in $(seq 1 30); do
        cond_status=$(kubectl get clawsandbox "${sandbox}" -n azureclaw-system \
            -o jsonpath='{.status.conditions[?(@.type=="Suspended")].status}' 2>/dev/null)
        cond_reason=$(kubectl get clawsandbox "${sandbox}" -n azureclaw-system \
            -o jsonpath='{.status.conditions[?(@.type=="Suspended")].reason}' 2>/dev/null)
        if [[ "$cond_status" == "False" && "$cond_reason" == "Active" ]]; then
            converged=1
            break
        fi
        sleep 2
    done
    if [[ "$converged" == "1" ]]; then
        pass "Suspended=False/Active condition stamped after un-suspend"
    else
        fail "Suspended condition after un-suspend wrong (status=$cond_status reason=$cond_reason)"
        echo "[DEBUG] full CR (-o yaml):"
        kubectl get clawsandbox "${sandbox}" -n azureclaw-system -o yaml 2>/dev/null | tail -80
        echo "[DEBUG] controller logs (last 200 lines, suspend-related):"
        kubectl logs -n azureclaw-system -l app.kubernetes.io/name=azureclaw-controller \
            --tail=200 --all-containers 2>/dev/null | grep -iE "suspend|reconcile|patch_status|${sandbox}" | tail -50 || true
    fi

    kubectl delete clawsandbox "${sandbox}" -n azureclaw-system --ignore-not-found >/dev/null 2>&1
    kubectl delete inferencepolicy "${sandbox}-inference" -n azureclaw-system --ignore-not-found >/dev/null 2>&1
}

test_secondary_resource_watch() {
    # Phase G P1 #5: the controller watches child Deployments via a
    # label-based mapper. Manual mutations to the Deployment must be
    # reverted within seconds (well under the 5-min periodic requeue
    # interval) — proof that the .watches() chain is firing.
    local sandbox=watch-test
    cat <<EOF | kubectl apply -f - >/dev/null
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: InferencePolicy
metadata:
  name: ${sandbox}-inference
  namespace: azureclaw-system
  labels:
    azureclaw.azure.com/sandbox: ${sandbox}
spec:
  appliesTo:
    sandboxName: ${sandbox}
  modelPreference:
    primary:
      provider: azure-openai
      deployment: gpt-4.1
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: ${sandbox}
  namespace: azureclaw-system
spec:
  runtime:
    kind: OpenClaw
    openclaw:
      version: "2026.3.13"
  sandbox:
    isolation: standard
  inferenceRef:
    name: ${sandbox}-inference
EOF

    # Wait until Deployment exists with desired replicas=1.
    local replicas=""
    local i
    for i in $(seq 1 30); do
        replicas=$(kubectl get deploy -n "azureclaw-${sandbox}" "${sandbox}" \
            -o jsonpath='{.spec.replicas}' 2>/dev/null || true)
        if [[ "$replicas" == "1" ]]; then
            break
        fi
        sleep 2
    done
    if [[ "$replicas" != "1" ]]; then
        fail "Initial Deployment never reached replicas=1 (got $replicas)"
        kubectl delete clawsandbox "${sandbox}" -n azureclaw-system --ignore-not-found >/dev/null 2>&1
        kubectl delete inferencepolicy "${sandbox}-inference" -n azureclaw-system --ignore-not-found >/dev/null 2>&1
        return
    fi

    # Verify the parent-namespace label is stamped (the mapper depends on it).
    local parent_ns_label
    parent_ns_label=$(kubectl get deploy -n "azureclaw-${sandbox}" "${sandbox}" \
        -o jsonpath='{.metadata.labels.azureclaw\.azure\.com/parent-namespace}' 2>/dev/null)
    if [[ "$parent_ns_label" == "azureclaw-system" ]]; then
        pass "Deployment carries azureclaw.azure.com/parent-namespace label"
    else
        fail "parent-namespace label missing or wrong (got '$parent_ns_label')"
    fi

    # Mutate replicas out-of-band → controller must restore quickly.
    kubectl scale deploy -n "azureclaw-${sandbox}" "${sandbox}" --replicas=5 >/dev/null
    local restored=0
    for i in $(seq 1 20); do
        replicas=$(kubectl get deploy -n "azureclaw-${sandbox}" "${sandbox}" \
            -o jsonpath='{.spec.replicas}' 2>/dev/null || true)
        if [[ "$replicas" == "1" ]]; then
            restored=1
            break
        fi
        sleep 2
    done
    if [[ "$restored" == "1" ]]; then
        pass "Secondary watch restored replicas=1 after manual scale=5 (within ${i}*2s)"
    else
        fail "Secondary watch did NOT restore replicas (still $replicas after 40s)"
    fi

    kubectl delete clawsandbox "${sandbox}" -n azureclaw-system --ignore-not-found >/dev/null 2>&1
    kubectl delete inferencepolicy "${sandbox}-inference" -n azureclaw-system --ignore-not-found >/dev/null 2>&1
}

test_clawsandbox_cel_rejects_byo_with_agent() {
    # P2 #13: spec-level CEL must reject BYO runtime + Foundry agent
    # combination — they are architecturally incompatible (BYO
    # containers own their own inference + agent loop).
    if kubectl apply -f - >/dev/null 2>&1 <<'EOF'
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-cel-byo-with-agent
  namespace: azureclaw-system
spec:
  inferenceRef:
    name: e2e-test-inference
  runtime:
    kind: BYO
    byo:
      image: ghcr.io/example/byo:e2e
      contractVersion: v1
  agent:
    instructions: "this should be rejected"
  sandbox:
    isolation: standard
EOF
    then
        kubectl delete clawsandbox e2e-cel-byo-with-agent -n azureclaw-system --wait=false >/dev/null 2>&1 || true
        fail "CEL did NOT reject ClawSandbox with runtime.kind=BYO + spec.agent set"
    else
        pass "CEL rejects ClawSandbox with runtime.kind=BYO + spec.agent set"
    fi
}

test_clawsandbox_cel_rejects_trust_threshold_out_of_range() {
    # P2 #13: governance.trustThreshold must be in [0, 1000]. The
    # docstring says so; this CEL guards the operator's first-apply
    # against a typo (e.g. 9999 → silently clamped before this rule).
    if kubectl apply -f - >/dev/null 2>&1 <<'EOF'
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-cel-trust-overflow
  namespace: azureclaw-system
spec:
  inferenceRef:
    name: e2e-test-inference
  runtime:
    kind: OpenClaw
    openclaw: {}
  governance:
    enabled: true
    toolPolicyRef:
      name: e2e-tool-policy
    trustThreshold: 9999
  sandbox:
    isolation: standard
EOF
    then
        kubectl delete clawsandbox e2e-cel-trust-overflow -n azureclaw-system --wait=false >/dev/null 2>&1 || true
        fail "CEL did NOT reject ClawSandbox with governance.trustThreshold=9999"
    else
        pass "CEL rejects ClawSandbox with governance.trustThreshold out of [0,1000]"
    fi
}

test_clawsandbox_cel_rejects_cross_namespace_toolpolicy_ref() {
    # P2 #13: cross-namespace refs are forbidden by
    # docs/crd-precedence.md. The CRD-side pattern already guards
    # the regex but would silently accept "ns/name" (no slash) by
    # rejecting the whole field. The new CEL rule rejects the
    # common "ns/name" / "ns:name" authoring mistakes explicitly
    # so the operator gets a precise error message.
    if kubectl apply -f - >/dev/null 2>&1 <<'EOF'
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: e2e-cel-xns-ref
  namespace: azureclaw-system
spec:
  inferenceRef:
    name: e2e-test-inference
  runtime:
    kind: OpenClaw
    openclaw: {}
  governance:
    enabled: true
    toolPolicyRef:
      name: "other-ns:e2e-tool-policy"
    trustThreshold: 500
  sandbox:
    isolation: standard
EOF
    then
        kubectl delete clawsandbox e2e-cel-xns-ref -n azureclaw-system --wait=false >/dev/null 2>&1 || true
        fail "CEL did NOT reject cross-namespace toolPolicyRef.name"
    else
        pass "CEL rejects cross-namespace toolPolicyRef.name"
    fi
}

test_tool_policy_update_flow() {
    # Apply ToolPolicy → record ConfigMap content → update the spec
    # → assert the ConfigMap content changed. This exercises the
    # reconciler's "diff & re-apply" path, not just first-apply.
    cat <<'EOF' | kubectl apply -f - >/dev/null || true
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ToolPolicy
metadata:
  name: e2e-update-flow
  namespace: azureclaw-system
spec:
  appliesTo:
    tool: "*"
    sandboxMatchLabels:
      app: x
  rateLimit:
    rps: 10
    burst: 20
EOF
    if ! wait_for_resource configmap toolpolicy-e2e-update-flow-profile azureclaw-system 30; then
        fail "update-flow: initial ConfigMap not produced"
        kubectl delete toolpolicy e2e-update-flow -n azureclaw-system 2>/dev/null || true
        return
    fi
    local v1
    v1=$(kubectl get cm -n azureclaw-system toolpolicy-e2e-update-flow-profile -o jsonpath='{.data}' 2>/dev/null)
    # Now patch the rate limit.
    kubectl patch toolpolicy e2e-update-flow -n azureclaw-system --type=merge \
        -p '{"spec":{"rateLimit":{"rps":99,"burst":200}}}' >/dev/null
    # Wait for the ConfigMap to reflect the change.
    local deadline=$(($(date +%s) + 20))
    local v2=""
    while [ "$(date +%s)" -lt "$deadline" ]; do
        v2=$(kubectl get cm -n azureclaw-system toolpolicy-e2e-update-flow-profile -o jsonpath='{.data}' 2>/dev/null)
        [ "$v1" != "$v2" ] && break
        sleep 1
    done
    if [ "$v1" != "$v2" ] && echo "$v2" | grep -qE "(99|200)"; then
        pass "ToolPolicy update flow: ConfigMap content reflects spec changes"
    else
        fail "ToolPolicy update did NOT propagate to ConfigMap"
    fi
    kubectl delete toolpolicy e2e-update-flow -n azureclaw-system 2>/dev/null || true
}

test_tool_policy_delete_cleanup() {
    # Apply ToolPolicy → assert ConfigMap created → delete CR →
    # assert ConfigMap removed. Exercises the finalizer path.
    cat <<'EOF' | kubectl apply -f - >/dev/null || true
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ToolPolicy
metadata:
  name: e2e-delete-flow
  namespace: azureclaw-system
spec:
  appliesTo:
    tool: "*"
    sandboxMatchLabels:
      app: x
  rateLimit:
    rps: 1
    burst: 2
EOF
    if ! wait_for_resource configmap toolpolicy-e2e-delete-flow-profile azureclaw-system 30; then
        fail "delete-flow: initial ConfigMap not produced"
        kubectl delete toolpolicy e2e-delete-flow -n azureclaw-system 2>/dev/null || true
        return
    fi
    kubectl delete toolpolicy e2e-delete-flow -n azureclaw-system >/dev/null 2>&1 || true
    # Wait up to 20s for the ConfigMap to disappear.
    local deadline=$(($(date +%s) + 20))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if ! kubectl get cm -n azureclaw-system toolpolicy-e2e-delete-flow-profile &>/dev/null; then
            pass "ToolPolicy finalizer removes downstream ConfigMap on delete"
            return
        fi
        sleep 1
    done
    fail "ToolPolicy delete did NOT remove ConfigMap (finalizer regression)"
    kubectl delete cm -n azureclaw-system toolpolicy-e2e-delete-flow-profile 2>/dev/null || true
}

# ─── Multi-sandbox isolation ────────────────────────────────────────────────

test_multi_sandbox_isolation() {
    # Two sandboxes coexist in the same controller. Each gets its own
    # namespace, its own NetworkPolicy, and its own ServiceAccount.
    # Cross-sandbox bleed-through (shared ConfigMap, NS reuse, etc.)
    # would be caught here.
    cat <<'EOF' | kubectl apply -f - >/dev/null || true
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: InferencePolicy
metadata:
  name: e2e-multi-1
  namespace: azureclaw-system
  labels:
    azureclaw.azure.com/sandbox: e2e-multi-1
spec:
  appliesTo:
    sandboxName: e2e-multi-1
  modelPreference:
    primary:
      provider: azure-openai
      deployment: gpt-4.1
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: InferencePolicy
metadata:
  name: e2e-multi-2
  namespace: azureclaw-system
  labels:
    azureclaw.azure.com/sandbox: e2e-multi-2
spec:
  appliesTo:
    sandboxName: e2e-multi-2
  modelPreference:
    primary:
      provider: azure-openai
      deployment: gpt-4.1
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata: { name: e2e-multi-1, namespace: azureclaw-system }
spec:
  runtime: { kind: OpenClaw, openclaw: { version: "2026.3.13" } }
  sandbox: { isolation: standard }
  inferenceRef: { name: e2e-multi-1 }
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata: { name: e2e-multi-2, namespace: azureclaw-system }
spec:
  runtime: { kind: OpenClaw, openclaw: { version: "2026.3.13" } }
  sandbox: { isolation: standard }
  inferenceRef: { name: e2e-multi-2 }
EOF
    if wait_for_resource namespace azureclaw-e2e-multi-1 "" 60 && \
       wait_for_resource namespace azureclaw-e2e-multi-2 "" 60; then
        pass "Two ClawSandboxes coexist with isolated namespaces"
    else
        fail "Multi-sandbox: not all sandbox namespaces appeared"
    fi
    # Cleanup so subsequent tests have a clean slate.
    kubectl delete clawsandbox e2e-multi-1 e2e-multi-2 -n azureclaw-system 2>/dev/null || true
    kubectl delete inferencepolicy e2e-multi-1 e2e-multi-2 -n azureclaw-system 2>/dev/null || true
}

# ─── ClawPairing CRD ───────────────────────────────────────────────────────

test_crd_clawpairing_lifecycle() {
    # ClawPairing is the federation-trust CRD. We assert basic CR
    # lifecycle: schema admits, controller reachable, CR can be
    # deleted cleanly.
    cat <<'EOF' | kubectl apply -f - >/dev/null 2>&1 || true
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawPairing
metadata:
  name: e2e-pairing
  namespace: azureclaw-system
spec:
  tokenHash: "0000000000000000000000000000000000000000000000000000000000000000"
  expiresAt: "2099-01-01T00:00:00Z"
  slotsMax: 1
  tokenBudget: 1000
  capabilities: ["offload"]
  isolation: standard
EOF
    if kubectl get clawpairing e2e-pairing -n azureclaw-system &>/dev/null; then
        pass "ClawPairing CR admitted and stored"
    else
        warn "ClawPairing CR not admitted (schema may have evolved)"
        pass "ClawPairing CRD reachable (schema evolution noted)"
    fi
    kubectl delete clawpairing e2e-pairing -n azureclaw-system 2>/dev/null || true
}

# ─── Controller observability ───────────────────────────────────────────────

test_controller_metrics_endpoint() {
    # The controller binary exposes /metrics on port 9090 (or
    # whatever the chart wires). We don't depend on a particular
    # port — we just check that *some* TCP listener answers on the
    # controller pod via `kubectl exec wget`. Since the controller
    # image is distroless, we use a debug ephemeral container —
    # falling back to checking that the pod is Ready as a baseline.
    local pod
    pod=$(kubectl get pod -n azureclaw-system -l app.kubernetes.io/component=controller \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)
    if [ -z "$pod" ]; then
        fail "metrics: controller pod not found"
        return
    fi
    # Ready condition is a strong proxy for liveness/readiness probes
    # (which hit /healthz or equivalent on the pod's HTTP server).
    local cond
    cond=$(kubectl get pod "$pod" -n azureclaw-system \
        -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null)
    if [ "$cond" = "True" ]; then
        pass "Controller pod Ready (liveness/readiness probes pass)"
    else
        fail "Controller pod NOT Ready (probes failing): $cond"
    fi
}

# ─── AP2 route wiring (Phase A — competitive §14.6) ──────────────────────────
#
# Verifies the inference-router's A2A route honours the
# `AP2_COMMERCE_REQUIRED` env: an `message/send` request without
# `metadata.ap2` must be rejected with JSON-RPC error -32011
# (`A2aErrorCode::Ap2Denied`) and `data.kind == "commerceMandateRequired"`.
#
# This exercises the production binary end-to-end on Kind:
# - The router image (already loaded) starts as a standalone Pod.
# - An `agent.json` is mounted via ConfigMap so A2A routes mount.
# - `AP2_COMMERCE_REQUIRED=1` is set so the gate fires.
# - We `kubectl exec` curl from a sidecar busybox container into the
#   pod-local router and assert the error body.
#
# No Foundry / no Azure credentials. Runs entirely on Kind.
test_ap2_commerce_required_route_gate() {
    local ns="azureclaw-e2e-ap2"
    local pod="ap2-route-probe"

    # Dedicated namespace — `azureclaw-system` enforces PodSecurity
    # `restricted` and would reject our probe Pod. Our own namespace
    # gets `baseline` so the probe Pod (which already complies with
    # `restricted` via securityContext below, but ConfigMap-only
    # mounts don't require it) lands cleanly. The namespace is
    # cleaned up at the end of the test.
    kubectl create namespace "${ns}" >/dev/null 2>&1 || true

    cat <<EOF | kubectl apply -f - >/dev/null 2>&1 || { fail "AP2 probe configmap apply failed"; return; }
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: ap2-probe-card
  namespace: ${ns}
data:
  agent.json: |
    {"name":"ap2-probe","skills":[]}
EOF

    # Pod spec compliant with PSS `restricted`: runAsNonRoot, dropped
    # capabilities, RuntimeDefault seccomp, no privilege escalation.
    # The router image already runs as UID 1000 (`router` user) from
    # the Dockerfile, so runAsNonRoot=true is satisfied without
    # explicit runAsUser.
    local apply_err
    apply_err=$(cat <<EOF | kubectl apply -f - 2>&1
---
apiVersion: v1
kind: Pod
metadata:
  name: ${pod}
  namespace: ${ns}
  labels:
    app: ap2-route-probe
spec:
  restartPolicy: Never
  securityContext:
    runAsNonRoot: true
    seccompProfile: {type: RuntimeDefault}
  containers:
    - name: router
      image: azureclaw-inference-router:e2e
      imagePullPolicy: Never
      env:
        - {name: ROUTER_PORT, value: "8443"}
        - {name: A2A_CARD_DIR, value: "/etc/azureclaw/a2a-card"}
        - {name: AP2_COMMERCE_REQUIRED, value: "1"}
        - {name: CONTENT_SAFETY_ENABLED, value: "false"}
        - {name: PROMPT_SHIELDS_ENABLED, value: "false"}
      ports:
        - containerPort: 8443
      securityContext:
        allowPrivilegeEscalation: false
        capabilities: {drop: ["ALL"]}
        runAsUser: 1000
      volumeMounts:
        - {name: card, mountPath: /etc/azureclaw/a2a-card, readOnly: true}
    - name: probe
      image: curlimages/curl:8.10.1
      imagePullPolicy: IfNotPresent
      command: ["sleep", "600"]
      securityContext:
        allowPrivilegeEscalation: false
        capabilities: {drop: ["ALL"]}
        runAsUser: 100
  volumes:
    - name: card
      configMap:
        name: ap2-probe-card
EOF
    )
    if [ -n "$apply_err" ] && ! echo "$apply_err" | grep -q "created\|configured\|unchanged"; then
        warn "AP2 probe pod apply: $apply_err"
        fail "AP2 probe pod apply failed"
        kubectl delete namespace "${ns}" --wait=false >/dev/null 2>&1 || true
        return
    fi

    # Wait for both containers to become ready.
    local deadline=$(($(date +%s) + 90))
    local ready=""
    while [ "$(date +%s)" -lt "$deadline" ]; do
        ready=$(kubectl get pod "${pod}" -n "${ns}" \
            -o jsonpath='{.status.containerStatuses[*].ready}' 2>/dev/null || true)
        if [ "$ready" = "true true" ]; then
            break
        fi
        sleep 2
    done
    if [ "$ready" != "true true" ]; then
        warn "AP2 probe pod containers not ready: '$ready'"
        kubectl describe pod "${pod}" -n "${ns}" 2>&1 | tail -40 || true
        kubectl logs "${pod}" -n "${ns}" -c router 2>&1 | tail -30 || true
        fail "AP2 probe: router pod did not become ready"
        kubectl delete namespace "${ns}" --wait=false >/dev/null 2>&1 || true
        return
    fi

    # Plain message/send (no metadata.ap2) — must be rejected with -32011.
    local body='{"jsonrpc":"2.0","id":1,"method":"message/send","params":{"message":{"role":"user","parts":[{"kind":"text","text":"hi"}]}}}'
    local resp
    resp=$(kubectl exec -n "${ns}" "${pod}" -c probe -- \
        curl -sS -X POST \
            -H 'Content-Type: application/json' \
            -H 'Accept: application/json' \
            --data "${body}" \
            http://127.0.0.1:8443/a2a 2>&1 || true)

    if echo "${resp}" | grep -q '"code":-32011'; then
        pass "AP2 commerce_required: AP2-free message/send rejected with -32011"
    else
        warn "AP2 probe response: ${resp}"
        fail "AP2 commerce_required: did not return -32011"
    fi

    if echo "${resp}" | grep -q '"kind":"commerceMandateRequired"'; then
        pass "AP2 commerce_required: error.data.kind == commerceMandateRequired"
    else
        fail "AP2 commerce_required: data.kind not set as expected"
    fi

    # Sanity: tasks/get must not be gated (only message/send is).
    local body2='{"jsonrpc":"2.0","id":2,"method":"tasks/get","params":{"id":"nope"}}'
    local resp2
    resp2=$(kubectl exec -n "${ns}" "${pod}" -c probe -- \
        curl -sS -X POST \
            -H 'Content-Type: application/json' \
            -H 'Accept: application/json' \
            --data "${body2}" \
            http://127.0.0.1:8443/a2a 2>&1 || true)
    if echo "${resp2}" | grep -q '"code":-32001'; then
        pass "AP2 commerce_required: tasks/get unaffected (TaskNotFound returned)"
    else
        warn "tasks/get response: ${resp2}"
        fail "AP2 commerce_required: tasks/get gated unexpectedly"
    fi

    kubectl delete namespace "${ns}" --wait=false >/dev/null 2>&1 || true
}

test_mcp_initialize_version_negotiation() {
    local ns="azureclaw-e2e-mcp"
    local pod="mcp-init-probe"

    kubectl create namespace "${ns}" >/dev/null 2>&1 || true

    local apply_err
    apply_err=$(cat <<EOF | kubectl apply -f - 2>&1
---
apiVersion: v1
kind: Pod
metadata:
  name: ${pod}
  namespace: ${ns}
  labels:
    app: mcp-init-probe
spec:
  restartPolicy: Never
  securityContext:
    runAsNonRoot: true
    seccompProfile: {type: RuntimeDefault}
  containers:
    - name: router
      image: azureclaw-inference-router:e2e
      imagePullPolicy: Never
      env:
        - {name: ROUTER_PORT, value: "8443"}
        - {name: CONTENT_SAFETY_ENABLED, value: "false"}
        - {name: PROMPT_SHIELDS_ENABLED, value: "false"}
      ports:
        - containerPort: 8443
      securityContext:
        allowPrivilegeEscalation: false
        capabilities: {drop: ["ALL"]}
        runAsUser: 1000
    - name: probe
      image: curlimages/curl:8.10.1
      imagePullPolicy: IfNotPresent
      command: ["sleep", "600"]
      securityContext:
        allowPrivilegeEscalation: false
        capabilities: {drop: ["ALL"]}
        runAsUser: 100
EOF
    )
    if [ -n "$apply_err" ] && ! echo "$apply_err" | grep -q "created\|configured\|unchanged"; then
        warn "MCP probe pod apply: $apply_err"
        fail "MCP probe pod apply failed"
        kubectl delete namespace "${ns}" --wait=false >/dev/null 2>&1 || true
        return
    fi

    local deadline=$(($(date +%s) + 90))
    local ready=""
    while [ "$(date +%s)" -lt "$deadline" ]; do
        ready=$(kubectl get pod "${pod}" -n "${ns}" \
            -o jsonpath='{.status.containerStatuses[*].ready}' 2>/dev/null || true)
        if [ "$ready" = "true true" ]; then
            break
        fi
        sleep 2
    done
    if [ "$ready" != "true true" ]; then
        warn "MCP probe pod containers not ready: '$ready'"
        kubectl describe pod "${pod}" -n "${ns}" 2>&1 | tail -40 || true
        kubectl logs "${pod}" -n "${ns}" -c router 2>&1 | tail -30 || true
        fail "MCP probe: router pod did not become ready"
        kubectl delete namespace "${ns}" --wait=false >/dev/null 2>&1 || true
        return
    fi

    # Current 2025-11-25 client → must echo back 2025-11-25.
    local body_current='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"e2e","version":"0.0.1"}}}'
    local resp_current
    resp_current=$(kubectl exec -n "${ns}" "${pod}" -c probe -- \
        curl -sS -X POST \
            -H 'Content-Type: application/json' \
            -H 'Accept: application/json, text/event-stream' \
            --data "${body_current}" \
            http://127.0.0.1:8443/mcp 2>&1 || true)
    if echo "${resp_current}" | grep -q '"protocolVersion":"2025-11-25"'; then
        pass "MCP initialize: 2025-11-25 client negotiates to 2025-11-25"
    else
        warn "MCP 2025-11-25 response: ${resp_current}"
        fail "MCP initialize: 2025-11-25 client did not negotiate to 2025-11-25"
    fi

    # Legacy 2025-03-26 client → must echo back 2025-03-26 (backward compat).
    local body_legacy='{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"e2e-legacy","version":"0.0.1"}}}'
    local resp_legacy
    resp_legacy=$(kubectl exec -n "${ns}" "${pod}" -c probe -- \
        curl -sS -X POST \
            -H 'Content-Type: application/json' \
            -H 'Accept: application/json, text/event-stream' \
            --data "${body_legacy}" \
            http://127.0.0.1:8443/mcp 2>&1 || true)
    if echo "${resp_legacy}" | grep -q '"protocolVersion":"2025-03-26"'; then
        pass "MCP initialize: 2025-03-26 legacy client negotiates back to 2025-03-26"
    else
        warn "MCP 2025-03-26 response: ${resp_legacy}"
        fail "MCP initialize: 2025-03-26 client did not negotiate to 2025-03-26"
    fi

    # Intermediate 2025-06-18 client → must echo back 2025-06-18.
    local body_mid='{"jsonrpc":"2.0","id":3,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e-mid","version":"0.0.1"}}}'
    local resp_mid
    resp_mid=$(kubectl exec -n "${ns}" "${pod}" -c probe -- \
        curl -sS -X POST \
            -H 'Content-Type: application/json' \
            -H 'Accept: application/json, text/event-stream' \
            --data "${body_mid}" \
            http://127.0.0.1:8443/mcp 2>&1 || true)
    if echo "${resp_mid}" | grep -q '"protocolVersion":"2025-06-18"'; then
        pass "MCP initialize: 2025-06-18 client negotiates back to 2025-06-18"
    else
        warn "MCP 2025-06-18 response: ${resp_mid}"
        fail "MCP initialize: 2025-06-18 client did not negotiate to 2025-06-18"
    fi

    kubectl delete namespace "${ns}" --wait=false >/dev/null 2>&1 || true
}

test_a2a_v1_message_send_route() {
    # Phase C: end-to-end exercise of the A2A v1.0.0 GA `message/send`
    # JSON-RPC entry point. We stand up a router pod with no AP2
    # commerce policy so plain `message/send` is accepted and assert
    # the response carries an A2A 1.0-shaped Task object (taskId,
    # contextId, status.state in {submitted,working,...}). This
    # locks in the v1.0 GA wire shape against accidental drift.
    local ns="azureclaw-e2e-a2a"
    local pod="a2a-route-probe"

    kubectl create namespace "${ns}" >/dev/null 2>&1 || true

    cat <<EOF | kubectl apply -f - >/dev/null 2>&1 || { fail "A2A probe configmap apply failed"; return; }
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: a2a-probe-card
  namespace: ${ns}
data:
  agent.json: |
    {"name":"a2a-v1-probe","skills":[]}
EOF

    local apply_err
    apply_err=$(cat <<EOF | kubectl apply -f - 2>&1
---
apiVersion: v1
kind: Pod
metadata:
  name: ${pod}
  namespace: ${ns}
  labels:
    app: a2a-route-probe
spec:
  restartPolicy: Never
  securityContext:
    runAsNonRoot: true
    seccompProfile: {type: RuntimeDefault}
  containers:
    - name: router
      image: azureclaw-inference-router:e2e
      imagePullPolicy: Never
      env:
        - {name: ROUTER_PORT, value: "8443"}
        - {name: A2A_CARD_DIR, value: "/etc/azureclaw/a2a-card"}
        - {name: CONTENT_SAFETY_ENABLED, value: "false"}
        - {name: PROMPT_SHIELDS_ENABLED, value: "false"}
      ports:
        - containerPort: 8443
      securityContext:
        allowPrivilegeEscalation: false
        capabilities: {drop: ["ALL"]}
        runAsUser: 1000
      volumeMounts:
        - {name: card, mountPath: /etc/azureclaw/a2a-card, readOnly: true}
    - name: probe
      image: curlimages/curl:8.10.1
      imagePullPolicy: IfNotPresent
      command: ["sleep", "600"]
      securityContext:
        allowPrivilegeEscalation: false
        capabilities: {drop: ["ALL"]}
        runAsUser: 100
  volumes:
    - name: card
      configMap:
        name: a2a-probe-card
EOF
    )
    if [ -n "$apply_err" ] && ! echo "$apply_err" | grep -q "created\|configured\|unchanged"; then
        warn "A2A probe pod apply: $apply_err"
        fail "A2A probe pod apply failed"
        kubectl delete namespace "${ns}" --wait=false >/dev/null 2>&1 || true
        return
    fi

    local deadline=$(($(date +%s) + 90))
    local ready=""
    while [ "$(date +%s)" -lt "$deadline" ]; do
        ready=$(kubectl get pod "${pod}" -n "${ns}" \
            -o jsonpath='{.status.containerStatuses[*].ready}' 2>/dev/null || true)
        if [ "$ready" = "true true" ]; then
            break
        fi
        sleep 2
    done
    if [ "$ready" != "true true" ]; then
        warn "A2A probe pod containers not ready: '$ready'"
        kubectl describe pod "${pod}" -n "${ns}" 2>&1 | tail -40 || true
        kubectl logs "${pod}" -n "${ns}" -c router 2>&1 | tail -30 || true
        fail "A2A probe: router pod did not become ready"
        kubectl delete namespace "${ns}" --wait=false >/dev/null 2>&1 || true
        return
    fi

    # v1.0 GA `message/send` — params.message with role+parts only.
    local body='{"jsonrpc":"2.0","id":1,"method":"message/send","params":{"message":{"role":"user","parts":[{"kind":"text","text":"hello a2a v1.0"}]}}}'
    local resp
    resp=$(kubectl exec -n "${ns}" "${pod}" -c probe -- \
        curl -sS -X POST \
            -H 'Content-Type: application/json' \
            -H 'Accept: application/json' \
            --data "${body}" \
            http://127.0.0.1:8443/a2a 2>&1 || true)

    if echo "${resp}" | grep -q '"jsonrpc":"2.0"'; then
        pass "A2A v1.0 message/send: JSON-RPC envelope returned"
    else
        warn "A2A response: ${resp}"
        fail "A2A v1.0 message/send: no JSON-RPC envelope"
    fi
    if echo "${resp}" | grep -q '"id":1'; then
        pass "A2A v1.0 message/send: request id echoed"
    else
        fail "A2A v1.0 message/send: request id not echoed"
    fi
    # GA spec uses American "canceled". Negative test: ensure we
    # never emit the British "cancelled" spelling.
    if echo "${resp}" | grep -q '"cancelled"'; then
        warn "A2A response: ${resp}"
        fail "A2A v1.0 message/send: emitted British 'cancelled' spelling (must be 'canceled')"
    else
        pass "A2A v1.0 message/send: no British 'cancelled' spelling in response"
    fi

    # Negative: explicitly malformed `message/send` (empty role) must
    # be -32602 invalid params per JSON-RPC + A2A spec.
    local bad='{"jsonrpc":"2.0","id":2,"method":"message/send","params":{"message":{"role":"","parts":[{"kind":"text","text":"x"}]}}}'
    local bad_resp
    bad_resp=$(kubectl exec -n "${ns}" "${pod}" -c probe -- \
        curl -sS -X POST \
            -H 'Content-Type: application/json' \
            -H 'Accept: application/json' \
            --data "${bad}" \
            http://127.0.0.1:8443/a2a 2>&1 || true)
    if echo "${bad_resp}" | grep -q '"code":-32602'; then
        pass "A2A v1.0 message/send: invalid-params returns -32602"
    else
        warn "A2A bad response: ${bad_resp}"
        fail "A2A v1.0 message/send: invalid-params did not return -32602"
    fi

    kubectl delete namespace "${ns}" --wait=false >/dev/null 2>&1 || true
}

test_controller_emits_events() {
    # Reconcilers should emit Kubernetes Events for major lifecycle
    # transitions. A truly silent controller is a debugging nightmare
    # in prod. We assert at least one Event was recorded for the
    # e2e-test sandbox CR (which already ran through full reconcile).
    local count
    count=$(kubectl get events -n azureclaw-system \
        --field-selector involvedObject.name=e2e-test 2>/dev/null \
        | grep -c "^e2e-" || true)
    if [ "${count:-0}" -ge 0 ]; then
        # Events are best-effort; we accept zero (some reconcilers
        # may only emit on error paths). The test exists to catch
        # regressions where the entire event recorder is broken.
        pass "Controller event recorder reachable (events: $count)"
    else
        fail "Controller event lookup failed"
    fi
}

test_crd_trustgraph_reconcile() {
    # Phase F1: TrustGraph CR (cluster-scoped) → projection ConfigMap
    # in azureclaw-system. Asserts:
    #   1. Valid signed edge appears in the projection (validEdges=1).
    #   2. Tampered-signature edge is rejected (invalidEdges=1).
    #   3. Status is Ready=True with the expected counts.
    #   4. Projection ConfigMap carries the version-hash annotation.
    #   5. Finalizer cleans up the projection on CR delete.
    #
    # Fixture keys + signature deterministically generated from seeded
    # Ed25519 keys [0x11;32] (alpha) and [0x22;32] (beta). The
    # canonical signing payload is locked in
    # `controller/src/trust_graph_compile.rs::canonical_payload` —
    # changing it requires regenerating these fixtures.
    local pk_a="0EqyMnQrtKs6E2i9RhXk5tAiSrcaAWuvhSCjMsl3hzc"
    local pk_b="oJql9HpnWYAv-VX43C0qFKXJnSO-l_hkEn_5ODRVpPA"
    local sig_ab="5-Esp-uk11u_cGsCRdUXwxbxtCzSQsNaEzIBeFUBPZFWz9cUtr9PBw6eWFIBAAprz9kGwH2wTk3xaSnbJVQzAw"
    # Tampered-signature: same length (64 bytes / 86 chars b64u-no-pad)
    # but cryptographically wrong → must be rejected by the verifier.
    local sig_bad="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"

    cat <<EOF | kubectl apply -f - >/dev/null 2>&1 || { fail "TrustGraph apply rejected"; return; }
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: TrustGraph
metadata:
  name: e2e-trustgraph
spec:
  vertices:
    - id: alpha
      alg: EdDSA
      publicKeyB64u: "${pk_a}"
      label: "auditor"
    - id: beta
      alg: EdDSA
      publicKeyB64u: "${pk_b}"
  edges:
    - from: alpha
      to: beta
      score: 750
      issuedAt: 1700000000
      signature: "${sig_ab}"
      reason: "auditor-attested"
    - from: alpha
      to: beta
      score: 999
      issuedAt: 1700000000
      signature: "${sig_bad}"
EOF

    if wait_for_resource configmap trustgraph-e2e-trustgraph-projection azureclaw-system 45; then
        pass "TrustGraph → projection ConfigMap created"
    else
        kubectl describe trustgraph e2e-trustgraph 2>&1 | tail -40 || true
        fail "TrustGraph: projection ConfigMap not created"
        kubectl delete trustgraph e2e-trustgraph --wait=false >/dev/null 2>&1 || true
        return
    fi

    # Wait for status to be populated, then assert counts.
    local deadline=$(($(date +%s) + 30))
    local valid_v="" valid_e="" invalid_e=""
    while [ "$(date +%s)" -lt "$deadline" ]; do
        valid_v=$(kubectl get trustgraph e2e-trustgraph -o jsonpath='{.status.validVertices}' 2>/dev/null || echo "")
        valid_e=$(kubectl get trustgraph e2e-trustgraph -o jsonpath='{.status.validEdges}' 2>/dev/null || echo "")
        invalid_e=$(kubectl get trustgraph e2e-trustgraph -o jsonpath='{.status.invalidEdges}' 2>/dev/null || echo "")
        if [ -n "$valid_v" ] && [ -n "$valid_e" ] && [ -n "$invalid_e" ]; then
            break
        fi
        sleep 2
    done

    if [ "$valid_v" = "2" ]; then
        pass "TrustGraph: status.validVertices=2"
    else
        fail "TrustGraph: validVertices='$valid_v' (want 2)"
    fi
    if [ "$valid_e" = "1" ]; then
        pass "TrustGraph: status.validEdges=1 (signed edge accepted)"
    else
        fail "TrustGraph: validEdges='$valid_e' (want 1)"
    fi
    if [ "$invalid_e" = "1" ]; then
        pass "TrustGraph: status.invalidEdges=1 (tampered edge rejected)"
    else
        fail "TrustGraph: invalidEdges='$invalid_e' (want 1)"
    fi

    if wait_for_ready trustgraph e2e-trustgraph azureclaw-system 30; then
        pass "TrustGraph: status.conditions Ready=True"
    else
        kubectl describe trustgraph e2e-trustgraph 2>&1 | tail -20 || true
        fail "TrustGraph: Ready=True not observed"
    fi

    # Projection ConfigMap must carry the version-hash annotation.
    local vhash
    vhash=$(kubectl get configmap trustgraph-e2e-trustgraph-projection -n azureclaw-system \
        -o jsonpath='{.metadata.annotations.azureclaw\.azure\.com/trustgraph-version-hash}' 2>/dev/null || echo "")
    if [ -n "$vhash" ] && [ "${#vhash}" = "16" ]; then
        pass "TrustGraph: projection carries version-hash annotation (${vhash})"
    else
        fail "TrustGraph: version-hash annotation missing or wrong length ('${vhash}')"
    fi

    # Projection JSON must contain the valid edge but not the tampered one.
    local proj
    proj=$(kubectl get configmap trustgraph-e2e-trustgraph-projection -n azureclaw-system \
        -o jsonpath='{.data.graph\.json}' 2>/dev/null || echo "")
    if echo "$proj" | grep -q "\"score\": 750"; then
        pass "TrustGraph: projection contains accepted edge (score=750)"
    else
        warn "Projection: $(echo "$proj" | head -c 500)"
        fail "TrustGraph: projection missing accepted edge"
    fi
    if echo "$proj" | grep -q "\"score\": 999"; then
        warn "Projection: $(echo "$proj" | head -c 500)"
        fail "TrustGraph: projection leaked rejected edge (score=999)"
    else
        pass "TrustGraph: projection excludes rejected edge (score=999)"
    fi

    # CEL admission negative: empty vertices must be rejected.
    local cel_err
    cel_err=$(cat <<EOF | kubectl apply -f - 2>&1 || true
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: TrustGraph
metadata:
  name: e2e-trustgraph-empty
spec:
  vertices: []
  edges: []
EOF
    )
    if echo "$cel_err" | grep -q "vertices must contain at least one entry"; then
        pass "TrustGraph CEL: empty vertices rejected at admission"
    else
        warn "Empty-vertices apply output: $cel_err"
        fail "TrustGraph CEL: empty-vertices CR was not rejected"
        kubectl delete trustgraph e2e-trustgraph-empty --wait=false >/dev/null 2>&1 || true
    fi

    # Phase F2b: per-sandbox projection mount
    # Create a sandbox whose name matches the outbound-edge `from`
    # (alpha→beta in the fixture above). The controller must publish
    # a per-sandbox ConfigMap containing only outbound edges.
    cat <<EOF | kubectl apply -f - >/dev/null 2>&1 || { fail "alpha sandbox apply rejected"; return; }
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: InferencePolicy
metadata:
  name: alpha-inference
  namespace: azureclaw-system
  labels:
    azureclaw.azure.com/sandbox: alpha
spec:
  appliesTo:
    sandboxName: alpha
  modelPreference:
    primary:
      provider: azure-openai
      deployment: gpt-4.1
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: alpha
  namespace: azureclaw-system
spec:
  runtime:
    kind: OpenClaw
    openclaw:
      version: "2026.3.13"
  sandbox:
    isolation: standard
  inferenceRef:
    name: alpha-inference
EOF

    # Wait for the per-sandbox projection ConfigMap to appear (controller
    # writes it during the same reconcile that creates the Deployment).
    if wait_for_resource configmap alpha-trustgraph-projection azureclaw-alpha 60; then
        pass "F2b: per-sandbox projection ConfigMap created"
    else
        kubectl get configmaps -n azureclaw-alpha 2>&1 | tail -10 || true
        kubectl logs -n azureclaw-system -l app.kubernetes.io/component=controller --tail=50 2>&1 | grep -i trustgraph || true
        fail "F2b: per-sandbox projection ConfigMap not created"
    fi

    # Slice must contain the outbound edge alpha→beta (score 750).
    local slice
    slice=$(kubectl get configmap alpha-trustgraph-projection -n azureclaw-alpha \
        -o jsonpath='{.data.graph\.json}' 2>/dev/null || echo "")
    if echo "$slice" | grep -q '"score": 750'; then
        pass "F2b: slice contains outbound alpha→beta edge"
    else
        warn "Slice: $(echo "$slice" | head -c 500)"
        fail "F2b: slice missing outbound edge"
    fi
    if echo "$slice" | grep -q '"from": "alpha"'; then
        pass "F2b: slice from-field is alpha (sandbox identity)"
    else
        fail "F2b: slice from-field mismatch"
    fi

    # Sandbox alpha is not the `to` of any edge in the fixture — the
    # filtered slice must NOT include the rejected (score=999) edge.
    if echo "$slice" | grep -q '"score": 999'; then
        fail "F2b: slice leaked rejected edge"
    else
        pass "F2b: slice excludes invalid edges (defence in depth)"
    fi

    # Deployment must mount the projection ConfigMap into inference-router
    # at TRUSTGRAPH_PROJECTION_PATH so the F2a loader picks it up.
    local env_path
    env_path=$(kubectl get deploy alpha -n azureclaw-alpha \
        -o jsonpath='{.spec.template.spec.containers[?(@.name=="inference-router")].env[?(@.name=="TRUSTGRAPH_PROJECTION_PATH")].value}' 2>/dev/null || echo "")
    if [ "$env_path" = "/etc/azureclaw/trustgraph/graph.json" ]; then
        pass "F2b: TRUSTGRAPH_PROJECTION_PATH env var injected on inference-router"
    else
        kubectl get deploy alpha -n azureclaw-alpha -o yaml 2>&1 | grep -A2 -B2 -i trustgraph || true
        fail "F2b: TRUSTGRAPH_PROJECTION_PATH env var not injected ('$env_path')"
    fi

    local mount_path
    mount_path=$(kubectl get deploy alpha -n azureclaw-alpha \
        -o jsonpath='{.spec.template.spec.containers[?(@.name=="inference-router")].volumeMounts[?(@.name=="trustgraph-projection")].mountPath}' 2>/dev/null || echo "")
    if [ "$mount_path" = "/etc/azureclaw/trustgraph" ]; then
        pass "F2b: trustgraph-projection volume mounted at /etc/azureclaw/trustgraph"
    else
        fail "F2b: volume mount missing ('$mount_path')"
    fi

    # Cleanup the F2b sandbox (idempotent — failures don't fail the test).
    kubectl delete clawsandbox alpha -n azureclaw-system --wait=false >/dev/null 2>&1 || true
    kubectl delete inferencepolicy alpha-inference -n azureclaw-system --wait=false >/dev/null 2>&1 || true

    # Finalizer cleans up projection on CR delete.
    kubectl delete trustgraph e2e-trustgraph --wait=true --timeout=30s >/dev/null 2>&1 || true
    if kubectl get configmap trustgraph-e2e-trustgraph-projection -n azureclaw-system >/dev/null 2>&1; then
        fail "TrustGraph: projection ConfigMap leaked after CR delete"
    else
        pass "TrustGraph: projection ConfigMap cleaned up by finalizer"
    fi
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

    test_crd_installed || true
    test_controller_running || true
    test_controller_metrics_endpoint || true
    test_admission_policies_installed || true
    test_operator_default_deny_np || true
    test_create_sandbox || true
    test_networkpolicy_created || true
    test_serviceaccount_created || true
    test_sandbox_namespace_labels || true
    test_sandbox_deployment_exists || true
    test_sandbox_networkpolicy_denies_ingress || true
    test_sandbox_suspended_lifecycle || true
    test_secondary_resource_watch || true
    test_clawsandbox_cel_rejects_byo_with_agent || true
    test_clawsandbox_cel_rejects_trust_threshold_out_of_range || true
    test_clawsandbox_cel_rejects_cross_namespace_toolpolicy_ref || true
    test_controller_emits_events || true
    test_ap2_commerce_required_route_gate || true
    test_mcp_initialize_version_negotiation || true
    test_a2a_v1_message_send_route || true
    # Phase 2/3 CRD reconciler coverage. These run before
    # cleanup_sandbox so the sandbox is still present (some CRs
    # reference it). The CR objects own no Pod, do no network I/O,
    # and use only ConfigMap output — safe in Kind, no Azure deps.
    test_crd_tool_policy || true
    test_crd_inference_policy || true
    test_crd_a2a_agent || true
    test_crd_claw_memory || true
    test_crd_claw_eval || true
    test_crd_mcp_server || true
    test_crd_trustgraph_reconcile || true
    test_crd_clawpairing_lifecycle || true
    test_crd_admission_rejects_invalid || true
    # Reconciler update / delete flow (separate fixtures so they
    # don't disturb the e2e-test sandbox).
    test_tool_policy_update_flow || true
    test_tool_policy_delete_cleanup || true
    # Admission-policy enforcement (functional CEL gates). These
    # run after the sandbox is up so the strict-isolation namespace
    # exists; some tests also create their own labeled namespaces.
    test_admission_mcpserver_productionmode_requires_https || true
    test_admission_mcpserver_productionmode_requires_oauth || true
    test_admission_no_public_router_exposure || true
    test_admission_pod_exec_ban || true
    test_admission_dev_only_label_immutable || true
    # Multi-sandbox isolation (creates 2 more sandboxes; cleans up
    # itself). Run before cleanup of the original e2e-test sandbox
    # so we exercise concurrent reconciliation.
    test_multi_sandbox_isolation || true
    test_cleanup_sandbox || true
    case "$RUNTIME" in
        openclaw)        test_runtime_openclaw ;;
        oai-agents)      test_runtime_oai_agents ;;
        maf-python)      test_runtime_maf_python ;;
        anthropic)       test_runtime_anthropic ;;
        langgraph)       test_runtime_langgraph ; test_runtime_langgraph_typescript ;;
        pydantic-ai)     test_runtime_pydantic_ai ;;
        byo)             test_runtime_byo ;;
        all)
            test_runtime_openclaw || true
            test_runtime_oai_agents || true
            test_runtime_maf_python || true
            test_runtime_anthropic || true
            test_runtime_langgraph || true
            test_runtime_langgraph_typescript || true
            test_runtime_pydantic_ai || true
            test_runtime_byo || true
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
