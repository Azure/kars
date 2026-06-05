#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# tests/e2e/interop/hermes_openclaw_bidi.sh
# ----------------------------------------------------------------------
# Hermes (Python kars_agt_mesh) ↔ OpenClaw (TS @microsoft/agent-
# governance-sdk) bidirectional cross-runtime mesh proof.
#
# Prereqs (kind-kars-dev cluster up, kars deployed):
#   - kars-execbrief-hermes-multi  (Hermes sandbox, parent)
#   - kars-mesh-peer-openclaw      (OpenClaw sandbox, peer)
#   - agentmesh-relay + agentmesh-registry in `agentmesh` namespace
#   - controller version with the admin-token + agt-policy mount
#     fix for non-OpenClaw runtimes (commit 8416c0a or newer)
#   - hermes runtime image with the mesh_worker.submit_trust path
#     and registry get_agent(did) direct lookup baked in
#     (commit 8416c0a or newer)
#
# What this verifies:
#   1. OpenClaw can register on the mesh and is discoverable by
#      mesh-peer-openclaw capability.
#   2. OpenClaw → Hermes: a kars_mesh_send tool call delivers
#      a payload that the Hermes auto-responder decrypts AND
#      publishes to the router's /agt/trust store (so the operator
#      panel sees the peer with the human-readable display name).
#   3. The trust entry is keyed on 'mesh-peer-openclaw' (not the
#      raw did:mesh:<hex>), proving the registry get_agent(did)
#      reverse lookup works.
#
# Exit code: 0 = all assertions pass, 1 = any assertion fails.
# Run unattended:  bash tests/e2e/interop/hermes_openclaw_bidi.sh
# ----------------------------------------------------------------------
set -uo pipefail

# ── Config ──────────────────────────────────────────────────────────
HERMES_NS="${HERMES_NS:-kars-execbrief-hermes-multi}"
HERMES_DEPLOY="${HERMES_DEPLOY:-execbrief-hermes-multi}"
HERMES_NAME="${HERMES_NAME:-execbrief-hermes-multi}"
OPENCLAW_NS="${OPENCLAW_NS:-kars-mesh-peer-openclaw}"
OPENCLAW_DEPLOY="${OPENCLAW_DEPLOY:-mesh-peer-openclaw}"
OPENCLAW_NAME="${OPENCLAW_NAME:-mesh-peer-openclaw}"
REGISTRY_NS="${REGISTRY_NS:-agentmesh}"
HERMES_ROUTER_PORT="${HERMES_ROUTER_PORT:-29443}"
OPENCLAW_GATEWAY_PORT="${OPENCLAW_GATEWAY_PORT:-29789}"

PAYLOAD_TAG="BIDI_HARNESS_$(date +%s)"

# ── Logging ─────────────────────────────────────────────────────────
log()  { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
ok()   { printf '\033[32m  ✓\033[0m %s\n' "$*" >&2; }
fail() { printf '\033[31m  ✗\033[0m %s\n' "$*" >&2; HARNESS_FAILED=1; }
HARNESS_FAILED=0

# ── Cleanup on exit ─────────────────────────────────────────────────
PIDS_TO_KILL=()
cleanup() {
  for pid in "${PIDS_TO_KILL[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT

# ── Preflight ───────────────────────────────────────────────────────
log "=== Preflight ==="
for ns in "$HERMES_NS" "$OPENCLAW_NS" "$REGISTRY_NS"; do
  if ! kubectl get ns "$ns" >/dev/null 2>&1; then
    fail "namespace missing: $ns"
    exit 1
  fi
done
ok "namespaces present"

HERMES_POD=$(kubectl get pods -n "$HERMES_NS" -l app.kubernetes.io/instance="$HERMES_DEPLOY" -o name 2>/dev/null | head -1 | cut -d/ -f2 || true)
[[ -z "${HERMES_POD:-}" ]] && HERMES_POD=$(kubectl get pods -n "$HERMES_NS" -o name | head -1 | cut -d/ -f2)
OPENCLAW_POD=$(kubectl get pods -n "$OPENCLAW_NS" -l app.kubernetes.io/instance="$OPENCLAW_DEPLOY" -o name 2>/dev/null | head -1 | cut -d/ -f2 || true)
[[ -z "${OPENCLAW_POD:-}" ]] && OPENCLAW_POD=$(kubectl get pods -n "$OPENCLAW_NS" -o name | head -1 | cut -d/ -f2)
[[ -z "${HERMES_POD:-}" || -z "${OPENCLAW_POD:-}" ]] && { fail "missing pods (Hermes=$HERMES_POD, OpenClaw=$OPENCLAW_POD)"; exit 1; }
ok "Hermes pod: $HERMES_POD"
ok "OpenClaw pod: $OPENCLAW_POD"

# Hermes must have admin-token mount (regression guard for the
# controller mount-predicate fix in commit 8416c0a).
if kubectl get pod -n "$HERMES_NS" "$HERMES_POD" \
  -o jsonpath='{.spec.containers[?(@.name=="agent")].volumeMounts[*].mountPath}' \
  | grep -q '/etc/kars/secrets'; then
  ok "Hermes agent container has /etc/kars/secrets admin-token mount"
else
  fail "Hermes agent container missing /etc/kars/secrets mount — rebuild kars-controller with commit 8416c0a+"
  exit 1
fi

# Auto-responder must be enabled on the Hermes parent so inbound
# messages drive the submit_trust hook.
if ! kubectl exec -n "$HERMES_NS" "$HERMES_POD" -c agent -- \
    bash -c 'test "$KARS_MESH_AUTO_RESPONDER" = "1"' 2>/dev/null; then
  log "enabling KARS_MESH_AUTO_RESPONDER=1 on Hermes deployment"
  kubectl set env -n "$HERMES_NS" "deployment/$HERMES_DEPLOY" \
    KARS_MESH_AUTO_RESPONDER=1 >/dev/null
  log "waiting for Hermes rollout..."
  sleep 15
  kubectl wait -n "$HERMES_NS" --for=condition=Ready \
    pod -l app.kubernetes.io/instance="$HERMES_DEPLOY" --timeout=180s >/dev/null
  HERMES_POD=$(kubectl get pods -n "$HERMES_NS" -l app.kubernetes.io/instance="$HERMES_DEPLOY" -o name | head -1 | cut -d/ -f2)
  log "Hermes pod after rollout: $HERMES_POD"
fi
ok "KARS_MESH_AUTO_RESPONDER=1 active on Hermes"

# ── Port-forwards ───────────────────────────────────────────────────
log ""
log "=== Port-forwards ==="
kubectl port-forward -n "$HERMES_NS" "deploy/$HERMES_DEPLOY" \
  "${HERMES_ROUTER_PORT}:8443" >/dev/null 2>&1 &
PIDS_TO_KILL+=($!)
kubectl port-forward -n "$OPENCLAW_NS" "deploy/$OPENCLAW_DEPLOY" \
  "${OPENCLAW_GATEWAY_PORT}:18789" >/dev/null 2>&1 &
PIDS_TO_KILL+=($!)
sleep 5
ok "Hermes router:    localhost:${HERMES_ROUTER_PORT}"
ok "OpenClaw gateway: localhost:${OPENCLAW_GATEWAY_PORT}"

# ── Baseline ────────────────────────────────────────────────────────
log ""
log "=== Baseline ==="
TRUST_BEFORE=$(curl -sS --max-time 10 "http://127.0.0.1:${HERMES_ROUTER_PORT}/agt/trust" 2>/dev/null || echo '{}')
BEFORE_COUNT=$(echo "$TRUST_BEFORE" | python3 -c "import json,sys; d=json.load(sys.stdin); print(len(d.get('agents',[])))" 2>/dev/null || echo "0")
BEFORE_TS=$(echo "$TRUST_BEFORE" | python3 -c "
import json, sys
d = json.load(sys.stdin)
for a in d.get('agents', []):
    if a.get('agent_id') == '${OPENCLAW_NAME}':
        print(a.get('last_interaction', ''))
        sys.exit(0)
print('')
" 2>/dev/null || echo "")
log "Hermes /agt/trust agent count before: $BEFORE_COUNT"
log "Hermes /agt/trust last_interaction for ${OPENCLAW_NAME} before: ${BEFORE_TS:-<none>}"

# ── Warm OpenClaw so initAGT fires + agent registers on mesh ───────
log ""
log "=== Warming OpenClaw (first chat-completions triggers initAGT) ==="
OC_TOKEN=$(kubectl get secret -n "$OPENCLAW_NS" gateway-token -o jsonpath='{.data.token}' | base64 -d)
WARM_RESP=$(curl -sS --max-time 240 -X POST \
  -H "Authorization: Bearer $OC_TOKEN" -H "Content-Type: application/json" \
  -d '{"model":"openclaw","messages":[{"role":"user","content":"Call kars_mesh_inbox tool. Output the raw JSON tool response only."}],"stream":false,"user":"harness_warm","max_tokens":700,"temperature":0}' \
  "http://127.0.0.1:${OPENCLAW_GATEWAY_PORT}/v1/chat/completions" 2>&1 || true)
if echo "$WARM_RESP" | python3 -c "import json,sys; d=json.load(sys.stdin); exit(0 if 'choices' in d else 1)" 2>/dev/null; then
  ok "OpenClaw warm OK"
else
  log "OpenClaw warm response (truncated):"
  echo "$WARM_RESP" | head -c 400 >&2
  echo >&2
  fail "OpenClaw warmup failed"
  exit 1
fi

# Give the SDK time to register on registry + open relay WS
log "waiting 25s for OpenClaw to register on mesh..."
sleep 25

# ── Find live OpenClaw DID ─────────────────────────────────────────
log ""
log "=== Find live OpenClaw DID ==="
REG_POD=$(kubectl get pods -n "$REGISTRY_NS" -l app=agentmesh-registry -o name | head -1 | cut -d/ -f2)
[[ -z "$REG_POD" ]] && { fail "no agentmesh-registry pod found"; exit 1; }
OC_DID=$(kubectl exec -n "$REGISTRY_NS" "$REG_POD" -- python3 -c "
import urllib.request, json
from datetime import datetime, timezone
r = json.loads(urllib.request.urlopen('http://127.0.0.1:8082/v1/discover?capability=${OPENCLAW_NAME}&limit=5').read())
now = datetime.now(timezone.utc)
ranked = []
for a in r['results']:
    try:
        dt = datetime.fromisoformat(a['last_seen'].replace('Z','+00:00'))
        age = (now - dt).total_seconds()
        ranked.append((age, a['did']))
    except Exception:
        pass
ranked.sort()
if ranked and ranked[0][0] < 300:
    print(ranked[0][1])
" 2>/dev/null | tail -1)
if [[ -z "$OC_DID" ]]; then
  fail "no live OpenClaw DID registered with capability=${OPENCLAW_NAME} (age <5min)"
  exit 1
fi
ok "live OpenClaw DID: $OC_DID"

# ── Trigger OpenClaw to send to Hermes ─────────────────────────────
log ""
log "=== OC → Hermes mesh_send (payload tag: $PAYLOAD_TAG) ==="
SEND_RESP=$(curl -sS --max-time 240 -X POST \
  -H "Authorization: Bearer $OC_TOKEN" -H "Content-Type: application/json" \
  -d '{"model":"openclaw","messages":[{"role":"user","content":"Call kars_mesh_send tool with arguments {\"to_agent\":\"'"${HERMES_NAME}"'\",\"content\":\"'"${PAYLOAD_TAG}"'\"}. Output only the raw JSON tool response."}],"stream":false,"user":"harness_send","max_tokens":800,"temperature":0}' \
  "http://127.0.0.1:${OPENCLAW_GATEWAY_PORT}/v1/chat/completions" 2>&1 || true)
SEND_STATUS=$(echo "$SEND_RESP" | python3 -c "
import json, sys
try:
    d = json.loads(sys.stdin.read())
    if 'error' in d:
        print('ERROR:', d['error'])
        sys.exit(1)
    c = d['choices'][0]['message']['content']
    j = json.loads(c) if c.lstrip().startswith('{') else {'raw_content': c}
    print(j.get('status', 'unknown'))
except Exception as e:
    print(f'parse_err: {e}')
    sys.exit(1)
" 2>&1)
log "send status: $SEND_STATUS"
case "$SEND_STATUS" in
  delivered_via_agt_relay|delivered_and_replied)
    ok "OpenClaw kars_mesh_send returned $SEND_STATUS"
    ;;
  *)
    fail "OpenClaw kars_mesh_send did not deliver: $SEND_STATUS"
    log "raw response:"
    echo "$SEND_RESP" | head -c 500 >&2
    echo >&2
    ;;
esac

# Give the Hermes auto-responder time to decrypt + push trust
log "waiting 30s for Hermes auto-responder to process + push trust..."
sleep 30

# ── Assert Hermes /agt/trust shows the OpenClaw peer ──────────────
log ""
log "=== Operator-side proof: Hermes /agt/trust ==="
TRUST_AFTER=$(curl -sS --max-time 10 "http://127.0.0.1:${HERMES_ROUTER_PORT}/agt/trust" 2>/dev/null || echo '{}')
echo "$TRUST_AFTER" | python3 -m json.tool 2>&1 | head -40 | sed 's/^/    /' >&2

# Assertion 1: the OpenClaw peer's trust entry was freshly updated
# (its `last_interaction` timestamp moved forward), proving the
# auto-responder fired submit_trust on the inbound message we just
# sent. Counting agents doesn't work because the entry may already
# exist from a prior run.
AFTER_TS=$(echo "$TRUST_AFTER" | python3 -c "
import json, sys
d = json.load(sys.stdin)
for a in d.get('agents', []):
    if a.get('agent_id') == '${OPENCLAW_NAME}':
        print(a.get('last_interaction', ''))
        sys.exit(0)
print('')
" 2>/dev/null || echo "")
if [[ -n "$AFTER_TS" && "$AFTER_TS" != "$BEFORE_TS" ]]; then
  ok "trust entry refreshed for ${OPENCLAW_NAME}: ${BEFORE_TS:-<none>} → $AFTER_TS"
elif [[ -z "$BEFORE_TS" && -n "$AFTER_TS" ]]; then
  ok "trust entry created for ${OPENCLAW_NAME}: $AFTER_TS"
else
  fail "trust entry for ${OPENCLAW_NAME} not refreshed (before=$BEFORE_TS, after=$AFTER_TS) — submit_trust path broken"
fi

# Assertion 2: the entry resolves to the human-readable display name
# (mesh-peer-openclaw), not the raw did:mesh:<hex> — guards the
# registry get_agent(did) reverse-lookup fix.
RESOLVED_NAME=$(echo "$TRUST_AFTER" | python3 -c "
import json, sys
d = json.load(sys.stdin)
for a in d.get('agents', []):
    aid = a.get('agent_id', '')
    if aid == '${OPENCLAW_NAME}':
        score = a.get('score', 0)
        print(f'MATCH:{aid}:{score}')
        sys.exit(0)
    if aid.startswith('did:mesh:'):
        print(f'RAW_DID:{aid}')
print('NONE')
" 2>&1)
case "$RESOLVED_NAME" in
  "MATCH:"*)
    NAME_AND_SCORE="${RESOLVED_NAME#MATCH:}"
    SCORE="${NAME_AND_SCORE##*:}"
    ok "trust entry resolved to display name: ${OPENCLAW_NAME} (score=$SCORE)"
    # OpenClaw-convention baseline is score=500 (`Math.round(500 +
    # 0.0 * 500)`). The Hermes mesh_worker mirrors that by sending
    # score=0.5 to submit_trust (which scales 0-1.0 → 0-1000). A
    # score < 100 indicates the legacy `score=0.0` Python call that
    # used to floor to the anonymous-tier minimum (10).
    if [[ "$SCORE" -ge 500 ]]; then
      ok "trust score at expected baseline ($SCORE ≥ 500)"
    else
      fail "trust score too low: $SCORE (expected ≥500 — OpenClaw-convention baseline). submit_trust path used legacy score=0.0?"
    fi
    ;;
  "RAW_DID:"*)
    fail "trust entry shows raw DID instead of '${OPENCLAW_NAME}' — registry get_agent(did) reverse-lookup broken"
    log "  $RESOLVED_NAME"
    ;;
  *)
    fail "no trust entry for OpenClaw found in Hermes /agt/trust"
    ;;
esac

# ── Summary ─────────────────────────────────────────────────────────
log ""
log "=== Summary ==="
if [[ "$HARNESS_FAILED" -eq 0 ]]; then
  log "ALL ASSERTIONS PASSED"
  log "  payload tag: $PAYLOAD_TAG"
  log "  OpenClaw DID: $OC_DID"
  log "  Hermes trust last_interaction for ${OPENCLAW_NAME}: $AFTER_TS"
  exit 0
else
  log "HARNESS FAILED"
  exit 1
fi
