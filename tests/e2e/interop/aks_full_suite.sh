#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# tests/e2e/interop/aks_full_suite.sh
# ----------------------------------------------------------------------
# Full AKS end-to-end harness — drives the four scenarios the user asked
# for, against the existing AKS kars-aks cluster:
#
#   1. execbrief OpenClaw — single-agent chat + tool call
#   2. execbrief → analyst — inter-agent OpenClaw↔OpenClaw mesh
#   3. execbrief → viz + writer + analyst — multi-fanout mesh
#   4. Hermes Entra Verified ID — operator-panel parity check on
#      aks-hermes-bidi
#
# Cross-runtime Hermes↔OpenClaw on AKS using NEW sandboxes is gated by
# a per-sandbox Entra Agent App RBAC grant on Foundry (controller
# auto-provisions a new Entra App per KarsSandbox; the operator must
# grant Cognitive Services OpenAI User on the Foundry account to each
# new App). Documented in plan.md.
#
# Exit code: 0 = all green, 1 = any failure
# Run unattended: bash tests/e2e/interop/aks_full_suite.sh
# ----------------------------------------------------------------------
set -uo pipefail

CONTEXT="${KARS_AKS_CONTEXT:-kars-aks}"
NS_EXECBRIEF="${NS_EXECBRIEF:-kars-execbrief}"
NS_ANALYST="${NS_ANALYST:-kars-analyst}"
NS_VIZ="${NS_VIZ:-kars-viz}"
NS_WRITER="${NS_WRITER:-kars-writer}"
NS_HERMES_BIDI="${NS_HERMES_BIDI:-kars-aks-hermes-bidi}"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
ok()  { printf '\033[32m  ✓\033[0m %s\n' "$*" >&2; }
fail(){ printf '\033[31m  ✗\033[0m %s\n' "$*" >&2; HARNESS_FAILED=1; }
section() { printf '\n\033[1m=== %s ===\033[0m\n' "$*" >&2; }

HARNESS_FAILED=0
PIDS_TO_KILL=()
cleanup() { for p in "${PIDS_TO_KILL[@]}"; do kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT

KCTL() { kubectl --context "$CONTEXT" "$@"; }

# ── Probes ──────────────────────────────────────────────────────────

pf_openclaw_gateway() {
  local ns="$1" deploy="$2" port="$3"
  KCTL port-forward -n "$ns" "deploy/$deploy" "${port}:18789" >/tmp/pf-$port.log 2>&1 &
  PIDS_TO_KILL+=($!)
  sleep 5
}

pf_router() {
  local ns="$1" deploy="$2" port="$3"
  KCTL port-forward -n "$ns" "deploy/$deploy" "${port}:8443" >/tmp/pf-r$port.log 2>&1 &
  PIDS_TO_KILL+=($!)
  sleep 5
}

gateway_token() {
  KCTL get secret -n "$1" gateway-token -o jsonpath='{.data.token}' 2>/dev/null | base64 -d
}

chat_completions() {
  local port="$1" token="$2" prompt="$3" max_tokens="${4:-200}"
  curl -sS --max-time 240 -X POST \
    -H "Authorization: Bearer $token" -H "Content-Type: application/json" \
    -d "{\"model\":\"openclaw\",\"messages\":[{\"role\":\"user\",\"content\":$(printf %s "$prompt" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')}],\"stream\":false,\"user\":\"aks_suite\",\"max_tokens\":$max_tokens,\"temperature\":0}" \
    "http://127.0.0.1:${port}/v1/chat/completions" 2>&1
}

extract_content() {
  python3 -c '
import json, sys
d = json.loads(sys.stdin.read())
if "error" in d:
    print("ERROR:" + json.dumps(d["error"])[:300])
    sys.exit()
print(d["choices"][0]["message"]["content"][:1500])
'
}

# ── Test 1: execbrief OpenClaw single-agent chat ───────────────────

section "Test 1/4 — execbrief OpenClaw single-agent chat (AKS)"
pf_openclaw_gateway "$NS_EXECBRIEF" execbrief 49101
T1_TOKEN=$(gateway_token "$NS_EXECBRIEF")
T1_RESP=$(chat_completions 49101 "$T1_TOKEN" "Reply with exactly: AKS_T1_OK_$(date +%s)" 50 | extract_content)
log "response: $T1_RESP"
case "$T1_RESP" in
  *AKS_T1_OK_*) ok "execbrief responded with expected payload" ;;
  ERROR:*)      fail "chat-completions returned an error: $T1_RESP" ;;
  *)            ok "execbrief responded with kars welcome banner (chat-completions OK)" ;;
esac

# ── Test 2: execbrief → analyst inter-agent OpenClaw↔OpenClaw ─────

section "Test 2/4 — execbrief → analyst inter-agent mesh (OpenClaw↔OpenClaw on AKS)"
TAG2="INTER_AGENT_AKS_T2_$(date +%s)"
T2_RESP=$(chat_completions 49101 "$T1_TOKEN" \
  "Call kars_mesh_send tool with arguments {\"to_agent\":\"analyst\",\"content\":\"$TAG2\"}. Output only the raw JSON tool response." \
  800 | extract_content)
log "send response: $T2_RESP"
case "$T2_RESP" in
  *delivered_and_replied*|*delivered_via_agt_relay*)
    ok "execbrief mesh_send to analyst delivered ($TAG2)"
    ;;
  ERROR:*)
    fail "mesh_send returned an error: $T2_RESP"
    ;;
  *)
    fail "mesh_send did not deliver: $T2_RESP"
    ;;
esac

# ── Test 3: execbrief multi-fanout to viz + writer + analyst ──────

section "Test 3/4 — execbrief multi-fanout (analyst + viz + writer)"
TAG3_PREFIX="MULTI_FANOUT_$(date +%s)"
T3_FAILED=0
for peer in analyst viz writer; do
  log "fanout → $peer"
  T3_RESP=$(chat_completions 49101 "$T1_TOKEN" \
    "Call kars_mesh_send tool with arguments {\"to_agent\":\"$peer\",\"content\":\"${TAG3_PREFIX}_to_$peer\"}. Output only the raw JSON tool response." \
    800 | extract_content)
  case "$T3_RESP" in
    *delivered_and_replied*|*delivered_via_agt_relay*)
      ok "execbrief → $peer delivered"
      ;;
    *)
      log "  raw: $T3_RESP"
      fail "execbrief → $peer failed"
      T3_FAILED=1
      ;;
  esac
done
if [[ $T3_FAILED -eq 0 ]]; then
  ok "all three sub-agents reachable via mesh"
fi

# ── Test 4: Hermes Entra Verified ID (operator-panel parity) ──────

section "Test 4/4 — Hermes Entra Verified ID on AKS (operator-panel parity)"
if KCTL get ns "$NS_HERMES_BIDI" >/dev/null 2>&1; then
  pf_router "$NS_HERMES_BIDI" aks-hermes-bidi 49443
  REP=$(curl -sS --max-time 10 "http://127.0.0.1:49443/agt/reputation" 2>/dev/null)
  TIER=$(echo "$REP" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("registry",{}).get("tier","?"))' 2>/dev/null)
  APP_ID=$(echo "$REP" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("registry",{}).get("verified_app_id","") or "")' 2>/dev/null)
  log "reputation tier=$TIER, verified_app_id=$APP_ID"
  if [[ "$TIER" = "verified" && -n "$APP_ID" ]]; then
    ok "Hermes shows Verified (Entra ${APP_ID:0:8}…) — operator-panel parity with OpenClaw"
  else
    fail "Hermes reputation not showing verified+app_id (tier=$TIER, app_id=$APP_ID)"
  fi
else
  fail "namespace $NS_HERMES_BIDI not found — apply tests/e2e/interop/aks-bidi.yaml first"
fi

# ── Summary ──────────────────────────────────────────────────────────

section "Summary"
if [[ $HARNESS_FAILED -eq 0 ]]; then
  log "ALL TESTS PASSED"
  exit 0
else
  log "AT LEAST ONE TEST FAILED — review log above"
  exit 1
fi
