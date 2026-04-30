#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# AzureClaw Infrastructure E2E Test Suite
#
# Runs against a LIVE AKS cluster with Azure AI Foundry connectivity.
# Validates all Foundry API routes through the inference router.
#
# Prerequisites:
#   - kubectl configured with target AKS cluster
#   - A running sandbox pod in azureclaw-foundry-test namespace
#   - Port-forward or direct pod access to inference router (port 8443)
#
# Usage:
#   bash tests/e2e/infra-e2e.sh [--namespace <ns>] [--port <local-port>]

set -euo pipefail

NAMESPACE="${NAMESPACE:-azureclaw-foundry-test}"
LOCAL_PORT="${LOCAL_PORT:-8892}"
API_VERSION="api-version=2025-11-15-preview"
PASS=0
FAIL=0
SKIP=0

# ─── Colors ───────────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

pass() { echo -e "  ${GREEN}✓${NC} $1"; PASS=$((PASS + 1)); }
fail() { echo -e "  ${RED}✗${NC} $1"; FAIL=$((FAIL + 1)); }
skip() { echo -e "  ${YELLOW}○${NC} $1 (regional/config)"; SKIP=$((SKIP + 1)); }
info() { echo -e "${BLUE}[INFO]${NC} $1"; }

# Parse args
while [[ $# -gt 0 ]]; do
  case $1 in
    --namespace) NAMESPACE="$2"; shift 2 ;;
    --port) LOCAL_PORT="$2"; shift 2 ;;
    *) echo "Unknown arg: $1"; exit 1 ;;
  esac
done

BASE="http://localhost:${LOCAL_PORT}"

# ─── Helpers ──────────────────────────────────────────────────────────────────

api_get() {
  local path="$1"
  curl -sf "${BASE}/${path}?${API_VERSION}" -H 'Accept-Encoding: identity' 2>/dev/null
}

api_post() {
  local path="$1"
  local body="$2"
  curl -sf -X POST "${BASE}/${path}?${API_VERSION}" \
    -H 'Content-Type: application/json' \
    -H 'Accept-Encoding: identity' \
    -d "$body" 2>/dev/null
}

test_api() {
  local name="$1" method="$2" path="$3"
  local body="${4:-}"
  local status

  if [[ "$method" == "GET" ]]; then
    status=$(curl -s -o /dev/null -w '%{http_code}' "${BASE}/${path}?${API_VERSION}" -H 'Accept-Encoding: identity' 2>/dev/null || echo "000")
  else
    status=$(curl -s -o /dev/null -w '%{http_code}' -X POST "${BASE}/${path}?${API_VERSION}" \
      -H 'Content-Type: application/json' -H 'Accept-Encoding: identity' -d "$body" 2>/dev/null || echo "000")
  fi

  if [[ "$status" == "200" || "$status" == "201" || "$status" == "202" ]]; then
    pass "$name (${status})"
  elif [[ "$status" == "404" ]]; then
    skip "$name (404 — not available in this region)"
  elif [[ "$status" == "000" ]]; then
    fail "$name (connection refused — is port-forward active?)"
  else
    fail "$name (${status})"
  fi
}

# ─── Setup: find pod and port-forward ────────────────────────────────────────

POD=$(kubectl get pods -n "$NAMESPACE" -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
if [[ -z "$POD" ]]; then
  echo -e "${RED}ERROR: No pods found in namespace $NAMESPACE${NC}"
  echo "Run: azureclaw up --foundry-test first."
  exit 1
fi

info "Pod: $POD (namespace: $NAMESPACE)"
info "Setting up port-forward on localhost:${LOCAL_PORT} → 8443"

# Start port-forward in background
kubectl port-forward -n "$NAMESPACE" "$POD" "${LOCAL_PORT}:8443" &>/dev/null &
PF_PID=$!
trap "kill $PF_PID 2>/dev/null; wait $PF_PID 2>/dev/null" EXIT
sleep 3

# Verify connectivity
if ! curl -sf "${BASE}/healthz" &>/dev/null; then
  echo -e "${RED}ERROR: Router not reachable at ${BASE}/healthz${NC}"
  exit 1
fi

# ─── Test Suite ───────────────────────────────────────────────────────────────

echo ""
echo -e "${BLUE}══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}  AZURECLAW INFRASTRUCTURE E2E TEST SUITE${NC}"
echo -e "${BLUE}  Pod: ${POD}${NC}"
echo -e "${BLUE}  Namespace: ${NAMESPACE}${NC}"
echo -e "${BLUE}  Router: localhost:${LOCAL_PORT} → 8443${NC}"
echo -e "${BLUE}══════════════════════════════════════════════════════════════${NC}"
echo ""

# ── Section 1: Health ────────────────────────────────────────────────────────
echo -e "${BLUE}  ── Health ───────────────────────────────────────────────────${NC}"
test_api "Router healthz" GET "healthz"
echo ""

# ── Section 2: Foundry Project APIs ──────────────────────────────────────────
echo -e "${BLUE}  ── Foundry Project APIs ─────────────────────────────────────${NC}"
test_api "Memory Stores (list)" GET "memory_stores"
test_api "Memory Store (search)" POST "memory_stores/azureclaw-memory:search_memories" \
  '{"scope":"default","options":{"max_memories":5}}'
test_api "Indexes" GET "indexes"
test_api "Evaluators" GET "evaluators"
test_api "Evaluation Rules" GET "evaluationrules"
test_api "Evaluation Taxonomies" GET "evaluationtaxonomies"
test_api "Connections" GET "connections"
test_api "Deployments" GET "deployments"
test_api "Agents" GET "agents"
test_api "Datasets" GET "datasets"
test_api "Insights" GET "insights"
test_api "OpenAI Evals" GET "openai/evals"
test_api "OpenAI Conversations" GET "openai/conversations"
test_api "OpenAI Responses" GET "openai/responses"
test_api "Schedules" GET "schedules"
test_api "Red Teams" GET "redTeams/runs"
test_api "Fine-tuning Jobs" GET "openai/fine-tuning/jobs"
echo ""

# ── Section 3: Inference ─────────────────────────────────────────────────────
echo -e "${BLUE}  ── Inference APIs ───────────────────────────────────────────${NC}"
test_api "Chat Completions (gpt-4.1)" POST "v1/chat/completions" \
  '{"model":"gpt-4.1","messages":[{"role":"user","content":"Say hi"}],"max_tokens":5}'
echo ""

# ── Section 4: Responses API (Code Interpreter, Memory Search) ───────────────
echo -e "${BLUE}  ── Responses API (Foundry Tools) ────────────────────────────${NC}"
test_api "Code Interpreter" POST "openai/responses" \
  '{"model":"gpt-4.1","input":"Use Python: print(2+2)","tools":[{"type":"code_interpreter","container":{"type":"auto"}}],"store":false}'
test_api "Memory Search" POST "openai/responses" \
  '{"model":"gpt-4.1","input":"What does the user like?","tools":[{"type":"memory_search","memory_store_name":"azureclaw-memory","scope":"raw-curl-proof"}],"store":false}'
echo ""

# ── Section 5: Write operations ──────────────────────────────────────────────
echo -e "${BLUE}  ── Write Operations ─────────────────────────────────────────${NC}"
test_api "Memory Store (update_memories)" POST "memory_stores/azureclaw-memory:update_memories" \
  '{"items":[{"role":"user","content":"e2e test datum","type":"message"}],"scope":"e2e-infra-test","update_delay":0}'
test_api "Create Conversation" POST "openai/conversations" '{}'
echo ""

# ── Section 6: Skills deployed ───────────────────────────────────────────────
echo -e "${BLUE}  ── Skills in Container ──────────────────────────────────────${NC}"
SKILL_COUNT=$(kubectl exec -n "$NAMESPACE" "$POD" -c openclaw -- \
  sh -c 'find /sandbox/.openclaw/extensions/azureclaw/skills/ -name "SKILL.md" 2>/dev/null | wc -l' 2>/dev/null || echo "0")
SKILL_COUNT=$(echo "$SKILL_COUNT" | tr -d '[:space:]')
if [[ "$SKILL_COUNT" -ge 9 ]]; then
  pass "Skills deployed: ${SKILL_COUNT} SKILL.md files"
elif [[ "$SKILL_COUNT" -gt 0 ]]; then
  fail "Only ${SKILL_COUNT}/9 skills deployed"
else
  fail "No skills found in container"
fi

# Check FOUNDRY_PROJECT_ENDPOINT
FPE=$(kubectl exec -n "$NAMESPACE" "$POD" -c openclaw -- \
  sh -c 'echo $FOUNDRY_PROJECT_ENDPOINT' 2>/dev/null || echo "")
if [[ -n "$FPE" ]]; then
  pass "FOUNDRY_PROJECT_ENDPOINT set: ${FPE}"
else
  fail "FOUNDRY_PROJECT_ENDPOINT not set"
fi
echo ""

# ── Section 7: Router logs verification ──────────────────────────────────────
echo -e "${BLUE}  ── Router Logs ──────────────────────────────────────────────${NC}"
LOG_COUNT=$(kubectl logs -n "$NAMESPACE" "$POD" -c inference-router --since=5m 2>/dev/null \
  | grep -c "Proxying Foundry Agent API\|Forwarding inference\|Forwarding SSE" || echo "0")
if [[ "$LOG_COUNT" -gt 0 ]]; then
  pass "Router forwarded ${LOG_COUNT} requests (last 5 min)"
else
  skip "No recent router forwards in logs"
fi
echo ""

# ─── Summary ──────────────────────────────────────────────────────────────────
TOTAL=$((PASS + FAIL + SKIP))
echo -e "${BLUE}══════════════════════════════════════════════════════════════${NC}"
echo -e "  ${GREEN}PASS: ${PASS}${NC}  ${RED}FAIL: ${FAIL}${NC}  ${YELLOW}SKIP: ${SKIP}${NC}  TOTAL: ${TOTAL}"
echo -e "${BLUE}══════════════════════════════════════════════════════════════${NC}"
echo ""

if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi
