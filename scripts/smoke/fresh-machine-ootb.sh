#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# scripts/smoke/fresh-machine-ootb.sh
#
# Out-of-the-box smoke gate for the local-k8s + operator flow.
#
# Reproduces what a brand-new contributor does on a fresh machine:
#   1. wipe every piece of carried state (kind cluster, AGT clone,
#      ~/.kars credentials, npm-link)
#   2. fresh git clone of the current branch HEAD
#   3. cd cli && npm ci && npm run build && npm link
#   4. kars dev --target local-k8s (accept all defaults)
#   5. spawn one OpenClaw + one Hermes sandbox via the same
#      kars-add path the operator TUI uses
#   6. wait for both pods to reach Running 2/2
#
# Exits 0 iff every step succeeds. On failure prints the precise
# command that failed AND the diagnostic the operator would see
# in the activity log, so the regression is reproducible from
# the script output alone.
#
# This is the smoke gate referenced by the ootb-fresh-machine-gate
# todo. Designed for human use today; can be lifted into a GHA lane
# once a docker-enabled hosted runner with kind is wired up.
#
# Usage:
#   bash scripts/smoke/fresh-machine-ootb.sh                  # default
#   bash scripts/smoke/fresh-machine-ootb.sh --no-wipe        # reuse existing state
#   bash scripts/smoke/fresh-machine-ootb.sh --branch <name>  # checkout other branch
#   bash scripts/smoke/fresh-machine-ootb.sh --keep           # don't tear down on success
#
# Env (override defaults):
#   KARS_OOTB_WORKDIR    /tmp/kars-ootb-smoke
#   KARS_OOTB_CLUSTER    kars-ootb
#   KARS_OOTB_TIMEOUT    300  (seconds for each pod to reach Running)

set -uo pipefail

# ── Args ────────────────────────────────────────────────────────────
WIPE=true
KEEP=false
BRANCH=""
while [ $# -gt 0 ]; do
  case "$1" in
    --no-wipe) WIPE=false ;;
    --keep)    KEEP=true ;;
    --branch)  BRANCH="${2:-}"; shift ;;
    --help|-h) sed -n '3,30p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
  shift
done

WORKDIR="${KARS_OOTB_WORKDIR:-/tmp/kars-ootb-smoke}"
CLUSTER="${KARS_OOTB_CLUSTER:-kars-ootb}"
TIMEOUT="${KARS_OOTB_TIMEOUT:-300}"
CTX="kind-${CLUSTER}"
KCTL="kubectl --context $CTX"

# ── Logging ─────────────────────────────────────────────────────────
log()   { printf '\n\033[1;36m[%s]\033[0m %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
ok()    { printf '  \033[32m✓\033[0m %s\n' "$*" >&2; }
fail()  { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; }
section() { printf '\n\033[1;33m═══ %s ═══\033[0m\n' "$*" >&2; }
die()   { fail "$*"; exit 1; }

# ── Step 0: tool check ───────────────────────────────────────────────
section "Tool prerequisites"
for t in git docker kind kubectl helm node npm; do
  if ! command -v "$t" >/dev/null 2>&1; then
    die "missing required tool: $t"
  fi
done
ok "git docker kind kubectl helm node npm all present"

# ── Step 1: wipe (unless --no-wipe) ──────────────────────────────────
if [ "$WIPE" = true ]; then
  section "Wiping prior state"
  log "deleting kind cluster '$CLUSTER' if it exists"
  kind delete cluster --name "$CLUSTER" 2>/dev/null || true
  ok "kind cluster gone"

  log "removing ~/agent-governance-toolkit (auto-cloned by kars dev)"
  rm -rf "$HOME/agent-governance-toolkit" 2>/dev/null || true
  ok "AGT clone removed"

  log "removing $WORKDIR (fresh clone target)"
  rm -rf "$WORKDIR" 2>/dev/null || true
  ok "workdir removed"

  log "removing ~/.kars (credentials, identity, context) — backed up to ~/.kars.ootb-bak"
  if [ -d "$HOME/.kars" ]; then
    rm -rf "$HOME/.kars.ootb-bak" 2>/dev/null || true
    mv "$HOME/.kars" "$HOME/.kars.ootb-bak"
    ok "~/.kars backed up to ~/.kars.ootb-bak"
  else
    ok "no ~/.kars to back up"
  fi
else
  log "skipping wipe (--no-wipe)"
fi

# ── Step 2: fresh clone ──────────────────────────────────────────────
section "Fresh clone + checkout"
if [ ! -d "$WORKDIR/.git" ]; then
  log "git clone Azure/kars → $WORKDIR"
  git clone --quiet https://github.com/Azure/kars.git "$WORKDIR" \
    || die "git clone failed — check network + repo access"
  ok "cloned to $WORKDIR"
fi
cd "$WORKDIR"

if [ -n "$BRANCH" ]; then
  log "checking out $BRANCH"
  git fetch --quiet origin "$BRANCH" || die "fetch $BRANCH failed"
  git checkout --quiet "$BRANCH" || die "checkout $BRANCH failed"
  ok "on branch $BRANCH at $(git rev-parse --short HEAD)"
else
  ok "using default branch at $(git rev-parse --short HEAD)"
fi

# ── Step 3: build + link CLI ─────────────────────────────────────────
section "Build + link CLI"
cd cli
log "npm ci (clean install)"
npm ci --silent || die "npm ci failed"
ok "deps installed"
log "npm run build"
npm run build --silent || die "build failed"
ok "TypeScript compiled"
log "npm link (global kars binary points at this checkout)"
npm link --silent || die "npm link failed"
ok "kars binary linked: $(which kars)"
cd "$WORKDIR"

# ── Step 4: kars dev --target local-k8s ──────────────────────────────
section "kars dev --target local-k8s (NON-INTERACTIVE)"
# The interactive prompts (provider picker, model picker, MCP, channels)
# are skipped by passing --no-prompt and a stubbed creds file. We need
# Copilot creds for this to actually exchange a JWT — caller can either
# pre-seed ~/.kars/secrets.json or set KARS_OOTB_COPILOT_TOKEN before
# running this script.
if [ -n "${KARS_OOTB_COPILOT_TOKEN:-}" ]; then
  log "seeding ~/.kars with KARS_OOTB_COPILOT_TOKEN from env"
  mkdir -p "$HOME/.kars"
  cat > "$HOME/.kars/config.json" <<JSON
{ "provider": "github-copilot",
  "model":    "claude-opus-4.7",
  "endpoint": "https://api.githubcopilot.com",
  "name":     "ootb-smoke" }
JSON
  cat > "$HOME/.kars/secrets.json" <<JSON
{ "copilot_github_token": "${KARS_OOTB_COPILOT_TOKEN}" }
JSON
  ok "creds seeded"
else
  fail "KARS_OOTB_COPILOT_TOKEN env not set"
  die "set it to a 'gho_*' GitHub OAuth token with Copilot scope, or seed ~/.kars manually before running"
fi

log "running: kars dev --target local-k8s --cluster $CLUSTER (background, log → /tmp/kars-ootb-dev.log)"
# kars dev expects to be interactive for the agent-creation prompt at
# the end. We bypass that by Ctrl+C-ing it after the helm install
# completes — the smoke gate creates its own sandbox in step 5.
( kars dev --target local-k8s --cluster "$CLUSTER" </dev/null >/tmp/kars-ootb-dev.log 2>&1 ) &
DEV_PID=$!
log "kars dev pid: $DEV_PID — waiting for chart-applied stepper line"
for i in $(seq 1 30); do
  sleep 10
  if grep -q "chart applied" /tmp/kars-ootb-dev.log; then
    ok "chart applied (after ~${i}0s)"
    kill -INT $DEV_PID 2>/dev/null || true
    wait $DEV_PID 2>/dev/null || true
    break
  fi
  if ! kill -0 $DEV_PID 2>/dev/null; then
    fail "kars dev exited early — last lines:"
    tail -20 /tmp/kars-ootb-dev.log >&2
    die "kars dev failed during chart install"
  fi
done

# ── Step 5: spawn one OpenClaw + one Hermes via kars add ─────────────
section "Spawn OpenClaw + Hermes sandboxes"

run_kars_add() {
  local name="$1" runtime="$2" model="$3"
  log "kars add $name --runtime $runtime --model $model --isolation enhanced"
  if kars add "$name" --runtime "$runtime" --model "$model" \
       --isolation enhanced --learn-egress 2>&1 | tee /tmp/kars-ootb-add-$name.log | tail -10; then
    ok "kars add $name completed (exit 0)"
  else
    fail "kars add $name failed (exit $?)"
    return 1
  fi
}

run_kars_add openclaw-smoke openclaw      claude-opus-4.7 || die "OpenClaw spawn failed"
run_kars_add hermes-smoke   hermes        claude-opus-4.7 || die "Hermes spawn failed"

# ── Step 6: wait for both pods to reach Running 2/2 ─────────────────
section "Pod readiness (timeout ${TIMEOUT}s each)"

wait_for_running() {
  local name="$1"
  local ns="kars-$name"
  local deadline=$((SECONDS + TIMEOUT))
  while [ $SECONDS -lt $deadline ]; do
    local ready
    ready=$($KCTL get pods -n "$ns" -l app.kubernetes.io/instance="$name" \
              -o jsonpath='{.items[*].status.containerStatuses[*].ready}' 2>/dev/null \
              | tr ' ' '\n' | grep -c '^true$' || echo 0)
    local total
    total=$($KCTL get pods -n "$ns" -l app.kubernetes.io/instance="$name" \
              -o jsonpath='{.items[*].status.containerStatuses[*].ready}' 2>/dev/null \
              | tr ' ' '\n' | grep -c . || echo 0)
    if [ "$total" -ge 2 ] && [ "$ready" -eq "$total" ]; then
      ok "$name: Running $ready/$total"
      return 0
    fi
    printf "    \033[2m%s: %d/%d ready, %ds remaining\033[0m\r" \
      "$name" "$ready" "$total" $((deadline - SECONDS)) >&2
    sleep 3
  done
  fail "$name did not reach Running 2/2 in ${TIMEOUT}s"
  log "diagnostic — pod state:"
  $KCTL get pods -n "kars-$name" -o wide >&2
  log "diagnostic — pod events:"
  $KCTL describe pod -n "kars-$name" -l app.kubernetes.io/instance="$name" 2>&1 | tail -30 >&2
  return 1
}

wait_for_running openclaw-smoke || die "OpenClaw pod did not become ready"
wait_for_running hermes-smoke   || die "Hermes pod did not become ready"

# ── Done ────────────────────────────────────────────────────────────
section "OOTB SMOKE PASSED"
ok "Fresh-clone → kars dev → spawn OpenClaw + Hermes → Running 2/2"
ok "Branch: $(git -C "$WORKDIR" rev-parse --abbrev-ref HEAD) @ $(git -C "$WORKDIR" rev-parse --short HEAD)"

if [ "$KEEP" = true ]; then
  log "leaving cluster '$CLUSTER' up for inspection (--keep)"
  log "to clean up: kind delete cluster --name $CLUSTER && rm -rf $WORKDIR"
else
  log "tearing down (use --keep to skip)"
  kind delete cluster --name "$CLUSTER" 2>/dev/null || true
  ok "cluster deleted"
fi

exit 0
