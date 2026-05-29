#!/bin/bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# kars sandbox entrypoint
# Configures OpenClaw automatically from mounted secrets and env vars.
# The user never needs to manually configure anything.
#
# UID model (mirrors AKS pod architecture):
#   UID 1001 (router)  — inference router, can reach internet
#   UID 1000 (sandbox) — agent processes, restricted to localhost + DNS

set -e

# Make pre-staged OpenClaw bundled-runtime-deps discoverable at runtime.
#
# Background. The base image bakes all bundled channel/plugin deps into
# /opt/openclaw-stage at build time (full network); at runtime UID 1000
# cannot reach npm directly because of egress-guard, so OpenClaw must
# resolve every `require()` from local disk.
#
# Why we mirror to a writable tmpfs (and not just point at /opt directly):
# OpenClaw 2026.4.x's installBundledRuntimeDeps writes a sentinel lockfile
# (.openclaw-runtime-deps.lock) into the version-hash subdir of the stage
# tree on first resolve. With readOnlyRootFilesystem=true on AKS, the
# build-time-staged tree at /opt/openclaw-stage is on the RO rootfs, so
# the lockfile write fails with EROFS and the loader falls back to npm
# install — which then 403s through the egress-guarded forward proxy and
# wedges the node-host on every bundled plugin (memory-core, acpx, etc).
#
# Mirror the staged tree to /tmp/openclaw-stage at container start and
# point OPENCLAW_PLUGIN_STAGE_DIR there. /tmp is a 1GiB tmpfs (pod spec)
# and the staged tree is ~500MiB. The cp runs once per container start
# (~3-5s). Works in both modes:
#   - dev (Docker, writable rootfs): always works.
#   - AKS (RO rootfs, hardened): /tmp is writable, lockfile + manifest writes
#     succeed, and the loader's per-plugin install path returns early because
#     deps are already satisfied in the mirrored search root.
#
# This was the original working pattern (commit 4c3094a, Apr 27). It was
# regressed in 3e4e9aa to a colon-separated path list with no mirror, which
# left installRoot pointing at an empty /tmp/openclaw-cache and triggered
# npm install on every startup.
if [ -z "${OPENCLAW_PLUGIN_STAGE_DIR:-}" ] && [ -d /opt/openclaw-stage ]; then
  if [ ! -d /tmp/openclaw-stage ]; then
    cp -r /opt/openclaw-stage /tmp/openclaw-stage
    chmod -R u+w /tmp/openclaw-stage 2>/dev/null || true
    if [ "$(id -u)" = "0" ]; then
      chown -R sandbox:sandbox /tmp/openclaw-stage 2>/dev/null || true
    fi
  fi
  export OPENCLAW_PLUGIN_STAGE_DIR=/tmp/openclaw-stage
fi

# Default SANDBOX_NAME to a clean agent name (strip pod suffix from hostname)
if [ -z "$SANDBOX_NAME" ]; then
  # K8s pod names: <deployment>-<replicaset-hash>-<pod-hash>
  # Strip the two trailing hash segments to get the deployment name
  SANDBOX_NAME=$(echo "$HOSTNAME" | sed 's/-[a-z0-9]*-[a-z0-9]*$//')
  export SANDBOX_NAME
fi

# Raise FD limit — the inference router and gateway share this process namespace.
# Default 1024 is too low for long-running containers with many HTTP connections.
ulimit -n 65536 2>/dev/null || true

# Detect if running as root (dev mode via Docker) or non-root (AKS pod spec override).
# In dev: Dockerfile has no USER directive, entrypoint runs as root, uses runuser to
#   start router as UID 1001 and everything else as UID 1000.
# In AKS: pod spec sets runAsUser:1000, so entrypoint is already UID 1000. Router runs
#   as a separate container (UID 1001). No runuser needed.
if [ "$(id -u)" = "0" ]; then
  AS_SANDBOX="runuser -u sandbox --"
  AS_ROUTER="runuser -u router --"
  IS_ROOT=true
else
  AS_SANDBOX=""
  AS_ROUTER=""
  IS_ROOT=false
fi

# ── Pre-create OpenClaw temp directories ────────────────────────────────────
# OpenClaw requires /tmp/openclaw-{UID} dirs on startup. They must exist, be
# owned by the running user, and have mode 700 (security check rejects
# world-writable dirs). Docker --tmpfs /tmp starts empty, so create them here.
_oc_tmpdir="/tmp/openclaw-$(id -u)"
mkdir -p "$_oc_tmpdir" && chmod 700 "$_oc_tmpdir" 2>/dev/null || true
if [ "$IS_ROOT" = "true" ]; then
  # Dev mode: also create dirs for sandbox (1000) and router (1001)
  for _uid in 1000 1001; do
    _dir="/tmp/openclaw-${_uid}"
    mkdir -p "$_dir" && chown "${_uid}:${_uid}" "$_dir" && chmod 700 "$_dir" 2>/dev/null || true
  done
fi

# ── Egress guard: iptables restricts UID 1000 to localhost + DNS ────────────
# Only applies when running as root (dev mode). On AKS, the egress-guard init
# container handles this before the sandbox container starts.
if [ "$IS_ROOT" = "true" ] && command -v iptables >/dev/null 2>&1; then
  # Filter table: allow established, localhost, DNS — reject everything else
  # Flush before append to prevent duplicate rules on container restart (#13)
  iptables -N KARS_EGRESS 2>/dev/null || iptables -F KARS_EGRESS
  iptables -A KARS_EGRESS -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  iptables -A KARS_EGRESS -o lo -j ACCEPT
  iptables -A KARS_EGRESS -p udp --dport 53 -j ACCEPT
  iptables -A KARS_EGRESS -p tcp --dport 53 -j ACCEPT
  # Allow traffic to the forward proxy port (redirected packets go to localhost)
  iptables -A KARS_EGRESS -p tcp --dport 8444 -j ACCEPT
  iptables -A KARS_EGRESS -j REJECT --reject-with icmp-port-unreachable
  # Remove stale jump rule before adding (idempotent)
  iptables -D OUTPUT -m owner --uid-owner 1000 -j KARS_EGRESS 2>/dev/null || true
  iptables -A OUTPUT -m owner --uid-owner 1000 -j KARS_EGRESS

  # NAT table: redirect HTTP/HTTPS from UID 1000 to the transparent forward proxy.
  # The proxy enforces blocklist, allowlist, and learn mode on every request.
  # Inference (localhost:8443) is unaffected — loopback traffic is ACCEPTed above.
  iptables -t nat -N KARS_REDIRECT 2>/dev/null || iptables -t nat -F KARS_REDIRECT
  iptables -t nat -A KARS_REDIRECT -p tcp --dport 80  -j REDIRECT --to-port 8444
  iptables -t nat -A KARS_REDIRECT -p tcp --dport 443 -j REDIRECT --to-port 8444
  iptables -t nat -D OUTPUT -m owner --uid-owner 1000 ! -o lo -j KARS_REDIRECT 2>/dev/null || true
  iptables -t nat -A OUTPUT -m owner --uid-owner 1000 ! -o lo -j KARS_REDIRECT

  echo "[kars] iptables egress guard active (UID 1000 → transparent proxy on :8444)"
fi

OPENCLAW_DIR="/sandbox/.openclaw"
OPENCLAW_CONFIG="$OPENCLAW_DIR/openclaw.json"
WORKSPACE_DIR="/sandbox/.openclaw/workspace"

# Node.js compile cache for faster startup
mkdir -p /var/tmp/openclaw-compile-cache 2>/dev/null || true
[ "$IS_ROOT" = "true" ] && chown sandbox:sandbox /var/tmp/openclaw-compile-cache 2>/dev/null || true

# Read API key from mounted secret or env var (sub-agents get it via env)
API_KEY=""
if [ -f /run/secrets/azure-openai-key ]; then
  API_KEY=$(cat /run/secrets/azure-openai-key)
elif [ -n "${AZURE_OPENAI_API_KEY:-}" ]; then
  API_KEY="$AZURE_OPENAI_API_KEY"
  # Write to the secret path so the inference router can read it
  mkdir -p /run/secrets 2>/dev/null || true
  echo -n "$API_KEY" > /run/secrets/azure-openai-key 2>/dev/null || \
    echo -n "$API_KEY" > /tmp/azure-openai-key 2>/dev/null
  # Restrict permissions on fallback key file
  chmod 400 /tmp/azure-openai-key 2>/dev/null || true
fi

# ── Workload Identity → Entra token exchange (opt-in) ────────────────────
# If Azure Workload Identity is mounted (AZURE_FEDERATED_TOKEN_FILE set by
# AKS WI webhook), exchange the Kubernetes service account token for an
# Entra ID access token and set AGT_OAUTH_TOKEN for registry verification.
# This upgrades the agent from anonymous to verified tier.
#
# Operator-level kill switch: when the cluster operator knows the
# `api://agentmesh` Entra app registration is not provisioned in the
# tenant (e.g. dev clusters, brand-new deployments, or any subscription
# without the Entra Agent ID setup), the controller injects
# AGT_SKIP_ENTRA=1 to short-circuit the entire token-exchange block.
# Without this, every sandbox would burn ~123s on doomed retries before
# falling back to anonymous tier — long enough to break parent→sub-agent
# spawn-and-message workflows because the parent's tool-call timeout
# fires before the sub-agent finishes booting.
if [ "${MESH_AUTH_BACKEND:-}" = "EntraAgentIdentity" ] && [ -z "${AGT_OAUTH_TOKEN:-}" ]; then
  # Phase 6.b path: ask the router's /v1/mesh-token endpoint for a
  # verified-tier mesh peer token from the shared auth-sidecar.
  #
  # Priority order: this branch deliberately wins over AGT_SKIP_ENTRA=1
  # because the sidecar-mediated mint does NOT have the AADSTS500011
  # tenant-config issue that AGT_SKIP_ENTRA was designed to skip — the
  # sidecar uses the controller's MI + blueprint OBO, not a WI direct
  # exchange against api://agentmesh. So when the operator opts in to
  # MESH_AUTH_BACKEND=EntraAgentIdentity, they want the sidecar path,
  # NOT the anonymous-tier fallback.
  #
  # The router listens on loopback :8443, which UID 1000 IS permitted
  # to reach by the egress-guard baseline (it's how every mesh /
  # inference call already flows).
  echo "[entrypoint] MESH_AUTH_BACKEND=EntraAgentIdentity — acquiring mesh token via /v1/mesh-token"
  _ROUTER_URL="${ROUTER_LOCAL_URL:-http://127.0.0.1:8443}"
  _MESH_RESP=""
  _MESH_STATUS=""
  _ACCESS_TOKEN=""
  # Retry loop — the openclaw entrypoint runs at pod startup BEFORE
  # the inference-router has finished booting. The router takes
  # ~5-15s on a cold pod (rust binary cold-start + cluster IP wiring
  # + blocklist load). Without a retry, the first call fails with
  # connect-refused (curl status 000) and the sandbox falls back to
  # anonymous-tier even on a healthy cluster — exactly the bug
  # observed on kars-aks 2026-05-29T10:21 first-boot.
  _DELAY=1
  _ELAPSED=0
  _MAX_WAIT="${MESH_TOKEN_MAX_WAIT:-60}"
  _ATTEMPT=0
  while [ "$_ELAPSED" -lt "$_MAX_WAIT" ]; do
    _ATTEMPT=$((_ATTEMPT + 1))
    _MESH_RESP=$(curl -s -4 --connect-timeout 3 --max-time 8 \
      -w "\n__HTTP_STATUS__%{http_code}" \
      "${_ROUTER_URL}/v1/mesh-token" 2>/dev/null || echo "")
    _MESH_STATUS=$(printf '%s\n' "$_MESH_RESP" | grep -E '^__HTTP_STATUS__' | sed 's/^__HTTP_STATUS__//')
    _MESH_BODY=$(printf '%s\n' "$_MESH_RESP" | sed '/^__HTTP_STATUS__/d')
    if [ "$_MESH_STATUS" = "200" ]; then
      _ACCESS_TOKEN=$(printf '%s' "$_MESH_BODY" | python3 -c "import sys,json; print(json.load(sys.stdin).get('access_token',''))" 2>/dev/null || echo "")
      if [ -n "$_ACCESS_TOKEN" ]; then
        echo "[entrypoint] Mesh token acquired via auth-sidecar after ${_ATTEMPT} attempt(s) (${_ELAPSED}s) — verified-tier registration"
        export AGT_OAUTH_TOKEN="$_ACCESS_TOKEN"
        break
      fi
    fi
    # 404 means MESH_AUTH_BACKEND was set but the router image doesn't
    # have the /v1/mesh-token route — no point retrying that.
    if [ "$_MESH_STATUS" = "404" ]; then
      echo "[entrypoint] /v1/mesh-token returned 404 — router image too old for Phase 6.b"
      break
    fi
    sleep "$_DELAY"
    _ELAPSED=$((_ELAPSED + _DELAY))
    if [ "$_DELAY" -lt 4 ]; then _DELAY=$((_DELAY * 2)); fi
  done
  if [ -z "${AGT_OAUTH_TOKEN:-}" ]; then
    echo "[entrypoint] /v1/mesh-token failed after ${_ELAPSED}s (${_ATTEMPT} attempts, last status=${_MESH_STATUS:-network-error}); registering as anonymous tier"
    export AGT_TRUST_THRESHOLD=0
  fi
  unset _MESH_RESP _MESH_STATUS _MESH_BODY _ROUTER_URL _ACCESS_TOKEN _DELAY _ELAPSED _MAX_WAIT _ATTEMPT
elif [ "${AGT_SKIP_ENTRA:-0}" = "1" ]; then
  echo "[entrypoint] AGT_SKIP_ENTRA=1 — Entra token exchange disabled by operator, registering as anonymous tier"
  # Trust scoring is meaningless without OAuth identity: every peer registers
  # as anonymous (registry score 0), so a non-zero AGT_TRUST_THRESHOLD would
  # reject all sibling-to-sibling KNOCKs even after a successful X3DH handshake.
  # When Entra is intentionally disabled by the operator, fail-open the trust
  # gate (threshold=0). Policy evaluation in onKnock still runs, and the SDK's
  # KNOCK/X3DH still proves cryptographic identity end-to-end.
  if [ -n "${AGT_TRUST_THRESHOLD:-}" ] && [ "${AGT_TRUST_THRESHOLD}" != "0" ]; then
    echo "[entrypoint] AGT_SKIP_ENTRA=1 overrides AGT_TRUST_THRESHOLD=${AGT_TRUST_THRESHOLD} → 0 (anonymous-tier fail-open)"
  fi
  export AGT_TRUST_THRESHOLD=0
elif [ -n "${AZURE_FEDERATED_TOKEN_FILE:-}" ] && [ -f "${AZURE_FEDERATED_TOKEN_FILE}" ] && \
   [ -n "${AZURE_CLIENT_ID:-}" ] && [ -n "${AZURE_TENANT_ID:-}" ] && \
   [ -z "${AGT_OAUTH_TOKEN:-}" ]; then
  echo "[entrypoint] Exchanging Workload Identity token for Entra ID access token..."
  # Retry with exponential-ish backoff — confidential (Kata) VMs can take
  # 20–30 s for IMDS/AAD network plumbing to come up. Keep trying for up
  # to ~2 min before giving up and falling back to anonymous tier.
  _ACCESS_TOKEN=""
  _DELAY=1
  _ELAPSED=0
  _MAX_WAIT="${ENTRA_TOKEN_MAX_WAIT:-120}"
  _ATTEMPT=0
  while [ "$_ELAPSED" -lt "$_MAX_WAIT" ]; do
    _ATTEMPT=$((_ATTEMPT + 1))
    _FED_TOKEN=$(cat "$AZURE_FEDERATED_TOKEN_FILE")
    # UID 1000 is blocked from direct egress by the egress-guard iptables rules;
    # the only working path is the router's forward proxy on 127.0.0.1:8444.
    # Use -4 to avoid IPv6 connect hangs and --connect-timeout to fail fast while
    # the forward proxy is still coming up in the first few seconds.
    _TOKEN_RESP=$(curl -s -4 --connect-timeout 3 --max-time 10 \
      -x "${ENTRA_PROXY:-http://127.0.0.1:8444}" \
      "https://login.microsoftonline.com/${AZURE_TENANT_ID}/oauth2/v2.0/token" \
      -d "client_id=${AZURE_CLIENT_ID}" \
      -d "scope=api://agentmesh/.default" \
      -d "client_assertion_type=urn:ietf:params:oauth:client-assertion-type:jwt-bearer" \
      -d "client_assertion=${_FED_TOKEN}" \
      -d "grant_type=client_credentials" 2>/dev/null || echo "")
    _ACCESS_TOKEN=$(echo "$_TOKEN_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('access_token',''))" 2>/dev/null || echo "")
    if [ -n "$_ACCESS_TOKEN" ]; then
      echo "[entrypoint] Entra ID token acquired after ${_ATTEMPT} attempt(s) (${_ELAPSED}s) — agent will register as verified tier"
      break
    fi
    # Fail-fast on unrecoverable tenant-config errors: AADSTS500011 means the
    # api://agentmesh service principal is not provisioned in this tenant.
    # No amount of retrying will fix it — break out and register as anonymous.
    if echo "$_TOKEN_RESP" | grep -q "AADSTS500011"; then
      echo "[entrypoint] Entra: api://agentmesh SP not provisioned in tenant — skipping retries, registering as anonymous tier"
      break
    fi
    sleep "$_DELAY"
    _ELAPSED=$((_ELAPSED + _DELAY))
    # Back off: 1, 2, 4, 4, 4, … (cap at 4s so we never wait >1 cycle past success)
    if [ "$_DELAY" -lt 4 ]; then _DELAY=$((_DELAY * 2)); fi
  done
  if [ -n "$_ACCESS_TOKEN" ]; then
    export AGT_OAUTH_TOKEN="$_ACCESS_TOKEN"
  else
    echo "[entrypoint] Entra token exchange failed after ${_ELAPSED}s (${_ATTEMPT} attempts) — agent will register as anonymous tier"
    # Same fail-open reasoning as the AGT_SKIP_ENTRA branch above: a non-zero
    # trust threshold would block all sibling KNOCKs in anonymous-tier mode.
    if [ -n "${AGT_TRUST_THRESHOLD:-}" ] && [ "${AGT_TRUST_THRESHOLD}" != "0" ]; then
      echo "[entrypoint] Entra exchange failed: overriding AGT_TRUST_THRESHOLD=${AGT_TRUST_THRESHOLD} → 0 (anonymous-tier fail-open)"
    fi
    export AGT_TRUST_THRESHOLD=0
  fi
  unset _FED_TOKEN _TOKEN_RESP _ACCESS_TOKEN _DELAY _ELAPSED _MAX_WAIT _ATTEMPT
fi

# Get config from env vars (set by kars dev/up)
ENDPOINT="${AZURE_OPENAI_ENDPOINT:-}"
MODEL="${OPENCLAW_MODEL:-gpt-4.1}"

# Build models list from FOUNDRY_DEPLOYMENTS (JSON array) or fall back to single MODEL.
# We register ALL Foundry deployments (chat, embedding, image-gen) under the single
# `azure-openai` provider so OpenClaw never auto-enables its bundled `openai`
# extension plugin to handle gpt-image-1 / text-embedding-3-small. Auto-enabling
# that bundled plugin triggers a ~50s synchronous require() chain across the
# 1.7GB pre-stage tree on first WebUI `models.list` call (the first-impression
# wedge that kept us debugging for hours). All Foundry deployments egress through
# the router (127.0.0.1:8443) regardless of model kind, so a single unified
# provider is all we need.
case "${KARS_PROVIDER:-}" in
  github-copilot) _PROVIDER_LABEL="Copilot via kars" ;;
  github-models)  _PROVIDER_LABEL="GH Models via kars" ;;
  *)              _PROVIDER_LABEL="Azure via kars" ;;
esac
MODELS_JSON="[{\"id\":\"${MODEL}\",\"name\":\"${MODEL} (${_PROVIDER_LABEL})\"}]"
if [ -n "${FOUNDRY_DEPLOYMENTS:-}" ]; then
  # Parse deployment names and build models array for openclaw.json
  _PARSED=$(echo "$FOUNDRY_DEPLOYMENTS" | python3 -c "
import sys, json
try:
    deps = json.load(sys.stdin)
    models = []
    for d in deps:
        name = d.get('name') or d.get('id') or ''
        if name:
            models.append({'id': name, 'name': f'{name} (Azure via kars)'})
    if not models:
        models = [{'id': '${MODEL}', 'name': '${MODEL} (Azure via kars)'}]
    print(json.dumps(models))
except:
    print('[{\"id\":\"${MODEL}\",\"name\":\"${MODEL} (Azure via kars)\"}]')
" 2>/dev/null)
  [ -n "$_PARSED" ] && MODELS_JSON="$_PARSED"
fi

# Use existing gateway token if injected by controller, or generate a new one
if [ -n "${OPENCLAW_GATEWAY_TOKEN:-}" ]; then
  GATEWAY_TOKEN="$OPENCLAW_GATEWAY_TOKEN"
else
  GATEWAY_TOKEN=$(head -c 32 /dev/urandom | base64 | tr -dc 'a-zA-Z0-9' | head -c 32)
  export OPENCLAW_GATEWAY_TOKEN="$GATEWAY_TOKEN"
fi

# Allow OpenClaw 2026.4.x's built-in image generation provider to reach the
# inference router on 127.0.0.1:8443. Upstream added an SSRF preflight that
# rejects loopback / private / special-use IPs by default to mitigate SSRF in
# desktop deployments. In this sandbox, 127.0.0.1 is the *only* valid path —
# the inference router is the proxy that mediates all egress; iptables blocks
# every other destination for UID 1000. The narrow opt-in env var below is
# upstream's documented escape hatch (extensions/openai/image-generation-
# provider.ts:shouldAllowPrivateImageEndpoint), gated to baseUrls that already
# point at http://127.0.0.1: or http://localhost:, so it can't be abused to
# reach arbitrary RFC 1918 hosts.
export OPENCLAW_QA_ALLOW_LOCAL_IMAGE_PROVIDER=1

# Skip OpenClaw's bundled-extension discovery for performance. OpenClaw 2026.4.x
# loads pi-ai's full provider catalog via models.list every time the gateway
# starts cold (xAI/Anthropic/Mistral/etc. — providers kars never uses
# because all model traffic flows through the inference router to Foundry /
# GitHub Models). Without this flag, models.list takes ~50s and blocks the
# event loop on every first WebUI connect. With this flag, OpenClaw points at
# an empty tmpdir for bundled plugins and skips the enumeration entirely
# (see resolveDisabledBundledPluginsDir() in bundled-dir.js).
#
# Side effect: OpenClaw's 3 always-required runtime cores (speech-core,
# image-generation-core, media-understanding-core) live under the bundled
# extensions tree and become unresolvable. We re-publish them as
# auto-discovered non-bundled extensions further down (search for
# "always-required runtime cores").
# OpenClaw bundled-plugins strategy:
# Instead of disabling bundled plugins (which causes the 3 always-required
# runtime cores — speech-core, image-generation-core, media-understanding-core —
# to fail to resolve), we point OPENCLAW_BUNDLED_PLUGINS_DIR at a pruned
# dist-runtime/extensions/ tree (built at image build time, see Dockerfile)
# that contains ONLY those 3 cores, hardlinked from dist/.
#
# This satisfies resolveTrustedExistingOverride (path is under
# <packageRoot>/dist-runtime/extensions), keeps manifest discovery scope
# tiny (no 50s pi-ai catalog wedge on first models.list), and the boundary
# check passes because hardlinks have realpaths inside dist-runtime/.
if [ -d "/usr/local/lib/node_modules/openclaw/dist-runtime/extensions" ]; then
  export OPENCLAW_BUNDLED_PLUGINS_DIR="/usr/local/lib/node_modules/openclaw/dist-runtime/extensions"
  unset OPENCLAW_DISABLE_BUNDLED_PLUGINS
else
  # Fallback for older base images without the pruned tree.
  export OPENCLAW_DISABLE_BUNDLED_PLUGINS=1
fi

# Always (re)generate config + workspace seed files on every container start.
#
# Previously this block was guarded by `[ ! -f "$OPENCLAW_CONFIG" ]` for "idempotency",
# but on AKS `/sandbox` is a persistent volume and OpenClaw's runtime workspace
# bootstrap silently rewrites AGENTS.md / SOUL.md with its default scaffold ~minutes
# after first chat. After a pod restart the guard would skip our write block, leaving
# the OpenClaw stock scaffold in place — losing the kars welcome policy and
# producing the "just says hey" symptom.
#
# The config is fully env-driven and deterministic, so regenerating every boot is
# safe and cheap. The systemPromptOverride field below makes the welcome policy
# authoritative even if OpenClaw later rewrites the workspace markdown files.
if true; then
  # Create OpenClaw directories (owned by sandbox user)
  mkdir -p "$OPENCLAW_DIR" "$WORKSPACE_DIR"
  [ "$IS_ROOT" = "true" ] && chown -R sandbox:sandbox "$OPENCLAW_DIR"

  # Build the kars system-prompt override. This is the AUTHORITATIVE source
  # of agent identity + welcome policy: openclaw config takes precedence over
  # workspace AGENTS.md, so even when OpenClaw's runtime workspace bootstrap
  # rewrites AGENTS.md/SOUL.md with its default scaffold, this prompt remains.
  if [ -n "${TELEGRAM_BOT_TOKEN:-}" ]; then
    TELEGRAM_PROMPT_BLOCK="A Telegram bot is connected to this sandbox. When the user explicitly asks for status updates on Telegram (or kicks off a long-running multi-agent workflow they want to watch on their phone), use the \`telegram_status\` tool to post short, terse milestone messages (≤240 chars each) — e.g. \"🔍 analyst: searching 2026 sources…\". Skip Telegram pings for ordinary single-turn requests; this is opt-in by user intent. Avoid posting sensitive data, secrets, or full document content to Telegram — milestone summaries only."
  else
    TELEGRAM_PROMPT_BLOCK="No Telegram bot is configured for this sandbox. Avoid mentioning Telegram capabilities to the user; if they ask for Telegram status pings, reply that the channel isn't set up and proceed without them — do not attempt to call \`telegram_status\`."
  fi

  SYSTEM_PROMPT_FILE="$OPENCLAW_DIR/kars-system-prompt.txt"
  if [ "${KARS_PROVIDER:-}" = "github-copilot" ]; then
    # Copilot mode: large-context provider (Claude / GPT-5 / Gemini), no
    # Foundry tools, full mesh + governance, native Anthropic Messages
    # passthrough through the router. Same trim-down as github-models for
    # the Foundry-only feature claims, but no 16k-cap warning.
    cat > "$SYSTEM_PROMPT_FILE" << PROMPTEOF
You are **kars** — a sandboxed AI assistant powered by **GitHub Copilot**,
running inside an isolated container. Your inference is proxied through the
kars inference router which enforces egress policy and AGT governance.

Provider: GitHub Copilot (\`${ENDPOINT}\`)
Model: ${MODEL}
Sandbox ID: ${HOSTNAME:-dev-agent}

## On First Message (Welcome)

When a user starts a new conversation, include the following in your greeting
for security transparency:

1. Header: "🔒 kars Sandbox — Local Dev (GitHub Copilot)"
2. Provider: \`GitHub Copilot\`
3. Model: \`${MODEL}\`
4. Sandbox ID: \`${HOSTNAME:-dev-agent}\`
5. Security summary: mention that the environment is sandboxed (isolated
   container, non-root user, AGT policy gating, token budgets, egress
   blocklist + allowlist). Do NOT claim Content Safety / Prompt Shields —
   those run only when an Azure Content Safety resource is wired in.
6. Capabilities list: chat, code execution, sub-agent orchestration via the
   AGT mesh, secure inter-agent messaging, analysis, writing, general
   problem-solving. Do NOT advertise Foundry-only capabilities (Foundry web
   search, Foundry memory, Foundry knowledge index, Foundry agents,
   evaluations, deployments) — those tools are NOT available in this mode.
7. Invitation: ask how you can help

## Personality

Be warm and welcoming on first contact, then concise and technically
excellent. Don't be robotic. When you don't know something, say so. When
you can use a tool to help, use it proactively without asking permission.
Skip filler like "Great question!" — just help.

## Tool Posture

Copilot mode runs with a focused tool set: HTTP fetch, kars mesh
tools (spawn / send / await / inbox), and AGT governance hooks. The
Foundry tool catalog is intentionally NOT loaded here because there's no
Foundry project bound to this provider. If a user asks for Foundry-style
features (managed memory, knowledge indexes, deployments, evaluations),
tell them to switch to Azure AI Foundry by running \`kars credentials\`
and re-running \`kars dev\`.

## Web Research (IMPORTANT — do this, don't refuse)

You DO have web access in this mode via the \`http_fetch\` tool. Foundry's
managed Bing tool isn't loaded, but you can fetch arbitrary URLs through the
egress-controlled proxy. **Never tell a user "live web search isn't available"
— that is wrong.** Use \`http_fetch\` to:

- Fetch news / RSS feeds directly (BBC, HN, Ars, etc.)
- Hit DuckDuckGo's HTML endpoint: \`https://duckduckgo.com/html/?q=<query>\`
- Fetch Wikipedia REST API: \`https://en.wikipedia.org/api/rest_v1/page/summary/<topic>\`
- Hit any public JSON API directly (GitHub, Hacker News, etc.)

Pick the most appropriate source for the question, fetch it with
\`http_fetch\`, then summarize. If a domain is blocked by the egress policy,
\`http_fetch\` will return an error — try a different source. Be proactive,
not apologetic.

## Sub-Agent Orchestration (AGT Mesh)

You can spawn sub-agents and route work over the encrypted AgentMesh.
Use \`mesh_spawn\` to create a sibling, \`mesh_send\` / \`mesh_await\` to
exchange messages, and \`mesh_inbox\` to drain pending replies. Always
include a peer roster in TASK messages when delegating to multiple
siblings — the LLM should never have to guess which name maps to which
role. See the \`kars-spawn\` skill for the canonical template.

${TELEGRAM_PROMPT_BLOCK}

## Sandbox Environment

- Non-root (UID 1000), seccomp-confined
- Inference: routed via inference-router on localhost:8443
- Mesh: AGT relay on agentmesh.svc, E2E encrypted (Signal Protocol)
- Egress: blocklist (51K+ domains) + allowlist; learn mode → enforce
- Content Safety / Prompt Shields are NOT active in this mode (require an
  Azure Content Safety resource — switch to Foundry to enable them)
PROMPTEOF
  elif [ "${KARS_PROVIDER:-}" = "github-models" ]; then
    # Slim system prompt for GitHub Models mode: no Foundry tools registered,
    # no Azure-specific safety stack, and a 16k input-token cap upstream.
    # Keep the welcome policy + AGT mesh guidance; drop everything Foundry.
    cat > "$SYSTEM_PROMPT_FILE" << PROMPTEOF
You are **kars** — a sandboxed AI assistant powered by **GitHub Models**,
running inside an isolated container. Your inference is proxied through the
kars inference router which enforces egress policy and AGT governance.

Provider: GitHub Models (\`${ENDPOINT}\`)
Model: ${MODEL}
Sandbox ID: ${HOSTNAME:-dev-agent}

## On First Message (Welcome)

When a user starts a new conversation, include the following in your greeting
for security transparency:

1. Header: "🔒 kars Sandbox — Local Dev (GitHub Models)"
2. Provider: \`GitHub Models\`
3. Model: \`${MODEL}\`
4. Sandbox ID: \`${HOSTNAME:-dev-agent}\`
5. Security summary: mention that the environment is sandboxed (isolated
   container, non-root user, AGT policy gating, token budgets, egress
   blocklist + allowlist). Do NOT claim Content Safety / Prompt Shields —
   those run only when an Azure Content Safety resource is wired in.
6. Capabilities list: chat, code execution, sub-agent orchestration via the
   AGT mesh, secure inter-agent messaging, analysis, writing, general
   problem-solving. Do NOT advertise Foundry-only capabilities (Foundry web
   search, Foundry memory, Foundry knowledge index, Foundry agents,
   evaluations, deployments) — those tools are NOT available in this mode.
7. Invitation: ask how you can help

## Personality

Be warm and welcoming on first contact, then concise and technically
excellent. Don't be robotic. When you don't know something, say so. When
you can use a tool to help, use it proactively without asking permission.
Skip filler like "Great question!" — just help.

## Tool Posture

GitHub Models mode runs with a minimal tool set: HTTP fetch, kars mesh
tools (spawn / send / await / inbox), and AGT governance hooks. The full
Foundry tool catalog is intentionally NOT loaded here because GitHub Models
caps every request at 16,000 input tokens. If a user asks for Foundry-style
features (managed memory, knowledge indexes, deployments, evaluations), tell
them to switch to Azure AI Foundry by running \`kars credentials\` and
re-running \`kars dev\`.

## Web Research (IMPORTANT — do this, don't refuse)

You DO have web access in this mode via the \`http_fetch\` tool. Foundry's
managed Bing tool isn't loaded, but you can fetch arbitrary URLs through the
egress-controlled proxy. **Never tell a user "live web search isn't available"
— that is wrong.** Instead, use \`http_fetch\` to:

- Fetch news / RSS feeds directly: e.g. \`https://feeds.bbci.co.uk/news/rss.xml\`,
  \`https://hnrss.org/frontpage\`, \`https://feeds.arstechnica.com/arstechnica/index\`
- Hit DuckDuckGo's HTML endpoint:
  \`https://duckduckgo.com/html/?q=<query>\` and parse results
- Fetch Wikipedia REST API: \`https://en.wikipedia.org/api/rest_v1/page/summary/<topic>\`
- Hit any public JSON API directly (GitHub, Hacker News, etc.)

Pick the most appropriate source for the question, fetch it with
\`http_fetch\`, then summarize. If a domain is blocked by the egress policy,
\`http_fetch\` will return an error — try a different source. Be proactive,
not apologetic.

## Inter-Agent Communication

Sub-agent traffic is E2E encrypted via Signal Protocol over the AGT mesh.
Read sub-agent replies from \`kars_mesh_inbox\` rather than guessing at
them.

**When you receive a task from another agent via mesh, EXECUTE IT using your
available tools.** Do not reply with "I can't because of mode limitations" or
ask the parent to switch to Foundry — the parent already knows the mode. Use
\`http_fetch\` for anything web-related, run code, do the analysis, and return
the comprehensive result. The parent is waiting on you.

## Multi-Agent Orchestration — Use Server-Side Blocking

When you spawn multiple sub-agents in parallel (via \`kars_spawn\`) and
the workflow requires assembling outputs from several of them, prefer
server-side blocking over polling \`kars_mesh_inbox\` in a loop:

- \`kars_mesh_await(senders=["analyst","viz"], timeout_seconds=300)\`
  blocks in a single tool call until all listed senders have delivered at
  least one message (or until timeout returns a partial result).
- \`kars_mesh_inbox(block_until_message=true, timeout_seconds=180)\`
  blocks until at least one new message arrives.

After mesh_await resolves, call \`kars_mesh_inbox(mark_read=true)\` to
read the actual content.

## Telegram Status Updates (when configured)

${TELEGRAM_PROMPT_BLOCK}

## Security Context

- Non-root user (sandbox:1000), read-only rootfs, seccomp filtered
- Inference proxied through kars router: AGT policy gating on tool
  calls, per-sandbox token budgets, request audit logging
- Egress: blocklist (51K+ domains) + allowlist; learn mode → enforce
- Content Safety / Prompt Shields are NOT active in this mode (require an
  Azure Content Safety resource — switch to Foundry to enable them)
PROMPTEOF
  else
    cat > "$SYSTEM_PROMPT_FILE" << PROMPTEOF
You are **kars** — a secure, sandboxed AI assistant powered by Azure AI Foundry,
running inside an isolated container on Azure Kubernetes Service (AKS). Your inference
is routed through the kars inference router which provides Content Safety,
Prompt Shields, token budgets, and egress control.

Connected Foundry project: ${FOUNDRY_PROJECT_ENDPOINT:-${ENDPOINT}}
Primary model: ${MODEL}
Sandbox ID: ${HOSTNAME:-dev-agent}

## On First Message (Welcome)

When a user starts a new conversation, include the following in your greeting
for security transparency:

1. Header: "🔒 kars Sandbox — Secure AI Runtime on Azure"
2. Foundry Project: \`${FOUNDRY_PROJECT_ENDPOINT:-${ENDPOINT}}\`
3. Model: \`${MODEL}\`
4. Sandbox ID: \`${HOSTNAME:-dev-agent}\`
5. Security summary: mention that the environment is sandboxed (isolated
   container, read-only rootfs, seccomp, egress policy, Content Safety +
   Prompt Shields)
6. Capabilities list: briefly list what you can do (code execution, web search,
   document search, persistent memory, sub-agent orchestration, secure mesh
   messaging, analysis, writing, general problem-solving)
7. Invitation: ask how you can help

Format the header as a bold or prominent line. The Foundry project endpoint
and model should be visible so the user knows which backend they are connected
to. Include the Foundry project line even when it says "Not configured".

## Personality

Be warm and welcoming on first contact, then concise and technically excellent.
Don't be robotic. When you don't know something, say so. When you can use a tool
to help, use it proactively without asking permission. Skip filler like "Great
question!" — just help.

## Tooling Posture

When the user asks about the Foundry project, deployed models, connections, indexes,
agents, or anything discoverable, call the relevant tool (e.g. \`foundry_deployments\`)
and show LIVE data. Prefer real-time tool calls over static knowledge whenever the
information can be fetched dynamically.

## Inter-Agent Communication

Sub-agent traffic is E2E encrypted via Signal Protocol over the AGT mesh. Read
sub-agent replies from \`kars_mesh_inbox\` rather than guessing at them.
When you receive a task from another agent, execute it autonomously using your
full toolset and return a comprehensive result.

## Multi-Agent Orchestration — Use Server-Side Blocking

When you spawn multiple sub-agents in parallel (via \`kars_spawn\`) and the
workflow requires assembling outputs from several of them, prefer server-side
blocking over polling \`kars_mesh_inbox\` in a loop (which wastes LLM turns
and looks like the demo has stalled):

- \`kars_mesh_await(senders=["analyst","viz"], timeout_seconds=300)\` blocks
  in a single tool call until all listed senders have delivered at least one
  message (or until timeout returns a partial result). This is the right tool
  for fan-out then wait then assemble patterns.
- \`kars_mesh_inbox(block_until_message=true, timeout_seconds=180)\` blocks
  until at least one new message arrives. Use when waiting on a single peer.

After mesh_await resolves, call \`kars_mesh_inbox(mark_read=true)\` to read
the actual content. The \`<downloaded_files>\` JSON tail block returned by
\`foundry_code_execute\` lists local paths — pass them directly to
\`kars_mesh_transfer_file\`. Avoid copying files inside Python; the Foundry
container cannot see your local /sandbox.

If a Foundry artifact is missing, retry with \`foundry_download_file(file_id, container_id)\`.

## Telegram Status Updates (when configured)

${TELEGRAM_PROMPT_BLOCK}

## Security Context

- Non-root user (sandbox:1000), read-only rootfs, seccomp filtered
- Inference routed through Content Safety + Prompt Shields, token budgets enforced
- Egress: blocklist (51K+ domains) + allowlist; learn mode → enforce promotion
PROMPTEOF
  fi
  chmod 600 "$SYSTEM_PROMPT_FILE" 2>/dev/null || true

  # JSON-encode the prompt as a single string for embedding in openclaw.json.
  # `jq -Rs .` reads raw input (-R) as a single string (-s) and emits JSON.
  SYSTEM_PROMPT_JSON=$(jq -Rs . < "$SYSTEM_PROMPT_FILE")

  # Provider selection — when running on Copilot with a Claude model, route
  # Claude inference through the native Anthropic Messages API
  # (`/v1/messages`) instead of OpenAI chat completions. This preserves
  # extended-thinking signatures end-to-end (Copilot translates OpenAI-shape
  # thinking blocks lossily, breaking signatures and triggering 400s on
  # multi-turn conversations). Image generation + embeddings always go via
  # `azure-openai` shape — Copilot has no image/embedding endpoints, but the
  # router transparently rejects/forwards those.
  _PRIMARY_MODEL_REF="azure-openai/${MODEL}"
  _ANTHROPIC_PROVIDER_BLOCK=""
  case "$MODEL" in
    claude-*)
      if [ "${KARS_PROVIDER:-}" = "github-copilot" ]; then
        _PRIMARY_MODEL_REF="anthropic/${MODEL}"
        # Drop the model from the azure-openai block — pi-ai's PiModelRegistry
        # otherwise registers two entries with the same id and the OpenAI-shape
        # one (registered first) wins at dispatch, sending /v1/chat/completions
        # with `stream_options.include_usage: true` to Copilot which routes it
        # to Anthropic Vertex → 400 "stream_options: Extra inputs are not
        # permitted". Leaving azure-openai with an empty models array keeps
        # the provider available for image-generation/embeddings shimming
        # (router transparently 404s those for Copilot).
        MODELS_JSON="[]"
        # Anthropic SDK appends /v1/messages to baseUrl. Set baseUrl to the
        # router root so it hits POST http://127.0.0.1:8443/v1/messages,
        # which `routes/anthropic_messages.rs::forward_anthropic_passthrough`
        # forwards verbatim to Copilot's /v1/messages with our cached JWT.
        # `reasoning: true` enables extended thinking; signatures round-trip
        # through the router → Copilot → Anthropic without modification.
        _ANTHROPIC_PROVIDER_BLOCK=$(cat <<ANTHEOF
,
      "anthropic": {
        "baseUrl": "http://127.0.0.1:8443",
        "apiKey": "routed-via-inference-router",
        "headers": { "x-kars-sandbox": "${HOSTNAME:-dev-agent}" },
        "models": [{"id":"${MODEL}","name":"${MODEL} (${_PROVIDER_LABEL})","api":"anthropic-messages","baseUrl":"http://127.0.0.1:8443","reasoning":true}]
      }
ANTHEOF
)
      fi
      ;;
  esac

  # OpenClaw 2026.4.x has a "config drift correction" feature that compares the
  # on-disk config against a `.bak` snapshot+`config-health.json` and silently
  # restores the backup if our entrypoint-written file lacks gateway metadata
  # ("missing-meta-before-write" → restoredFromBackup). Wipe stale backups so
  # our freshly-rendered config wins on every start. See logs/config-audit.jsonl.
  rm -f "${OPENCLAW_CONFIG}.bak" "${OPENCLAW_CONFIG}".clobbered.* \
        "$OPENCLAW_DIR/logs/config-health.json" 2>/dev/null || true

  # Build the `mcp.servers` block from KARS_MCP_SERVERS (comma-separated
  # list of McpServer names projected by the controller). Each entry points
  # at the loopback router (`127.0.0.1:8443/mcp`) and carries an
  # `x-kars-mcp-server` header naming the registered McpServer. The
  # router resolves the header → registered server, signs the JWT with the
  # mounted per-server signing key (mounted at /etc/kars/mcp-signing/<name>),
  # filters by allowedTools, and forwards to the upstream URL. This is what
  # gives the McpServer CRD true E2E semantics: an OpenClaw `tool.*` invocation
  # against `<name>` is governed, signed, allow-listed and audited by the
  # router before ever leaving the pod.
  _MCP_BLOCK=""
  if [ -n "${KARS_MCP_SERVERS:-}" ]; then
    _MCP_ENTRIES=""
    _MCP_SEP=""
    OLDIFS="$IFS"; IFS=','
    for _mcp_name in $KARS_MCP_SERVERS; do
      _mcp_name=$(echo "$_mcp_name" | tr -d ' ')
      [ -z "$_mcp_name" ] && continue
      _MCP_ENTRIES="${_MCP_ENTRIES}${_MCP_SEP}\"${_mcp_name}\": { \"transport\": \"streamable-http\", \"url\": \"http://127.0.0.1:8443/mcp\", \"headers\": { \"x-kars-mcp-server\": \"${_mcp_name}\", \"x-kars-sandbox\": \"${HOSTNAME:-dev-agent}\" } }"
      _MCP_SEP=", "
    done
    IFS="$OLDIFS"
    if [ -n "$_MCP_ENTRIES" ]; then
      _MCP_BLOCK=",
  \"mcp\": {
    \"servers\": { ${_MCP_ENTRIES} }
  }"
    fi
  fi

  # Write openclaw.json (2026.4.x config format — routed through inference router)
  cat > "$OPENCLAW_CONFIG" << EOF
{
  "models": {
    "providers": {
      "azure-openai": {
        "baseUrl": "http://127.0.0.1:8443/v1",
        "apiKey": "routed-via-inference-router",
        "api": "openai-completions",
        "authHeader": false,
        "headers": { "x-kars-sandbox": "${HOSTNAME:-dev-agent}" },
        "models": ${MODELS_JSON}
      }${_ANTHROPIC_PROVIDER_BLOCK}
    }
  },
  "tools": {
    "deny": ["sessions_spawn", "sessions_send"],
    "exec": {
      "security": "full"
    }
  },
  "commands": {
    "mcp": true
  },
  "plugins": {
    "allow": [PLUGINS_ALLOW_PLACEHOLDER],
    "entries": {PLUGINS_ENTRIES_PLACEHOLDER}
  },
  "channels": {
    CHANNELS_PLACEHOLDER
  },
  "ui": {
    "assistant": {
      "name": "${KARS_DISPLAY_NAME:-kars}",
      "avatar": "🐾"
    }
  },
  "agents": {
    "defaults": {
      "model": { "primary": "${_PRIMARY_MODEL_REF}" },
      "imageGenerationModel": "azure-openai/gpt-image-1",
      "timeoutSeconds": 1500,
      "systemPromptOverride": ${SYSTEM_PROMPT_JSON},
      "memorySearch": {
        "enabled": true,
        "provider": "azure-openai",
        "model": "text-embedding-3-small",
        "remote": {
          "baseUrl": "http://127.0.0.1:8443/v1/",
          "apiKey": "routed-via-inference-router"
        }
      }
    }
  },
  "gateway": {
    "mode": "local",
    "bind": "lan",
    "auth": {
      "mode": "token"
    },
    "http": {
      "endpoints": {
        "chatCompletions": { "enabled": true },
        "responses": { "enabled": true }
      }
    },
    "controlUi": {
      "enabled": true,
      "dangerouslyDisableDeviceAuth": true
    }
  }${_MCP_BLOCK}
}
EOF
  chmod 600 "$OPENCLAW_CONFIG" 2>/dev/null || true

  # Seed auth-profiles.json for the "main" agent. Required by the embedded-mode
  # lane of `openclaw agent --message ...` subprocesses (spawned from the
  # kars plugin's delegateToNativeAgent / processTaskWithTools paths).
  # Without this file, the subprocess's embedded fallback errors out with
  # "No API key found for provider 'openai'" even though the gateway itself
  # routes through the inference router just fine.
  #
  # The key value is a routing stub — the inference router at 127.0.0.1:8443
  # doesn't validate it; real auth happens upstream via IMDS/WI. No real
  # credential material is written here.
  AGENT_DIR="$OPENCLAW_DIR/agents/main/agent"
  mkdir -p "$AGENT_DIR"
  cat > "$AGENT_DIR/auth-profiles.json" << 'AUTHPROFEOF'
{
  "version": 1,
  "profiles": {
    "openai:default": { "type": "api_key", "provider": "openai", "key": "routed-via-inference-router", "displayName": "router-stub" },
    "azure-openai:default": { "type": "api_key", "provider": "azure-openai", "key": "routed-via-inference-router", "displayName": "router-stub" }
  },
  "defaults": { "openai": "openai:default", "azure-openai": "azure-openai:default" }
}
AUTHPROFEOF
  chmod 600 "$AGENT_DIR/auth-profiles.json" 2>/dev/null || true
  # In dev mode we run as root; the sandbox user needs to own + read this file.
  # (AKS pods already run as UID 1000, so chown is a no-op / skipped.)
  if [ "$IS_ROOT" = "true" ]; then
    chown -R sandbox:sandbox "$OPENCLAW_DIR/agents" 2>/dev/null || true
  fi

  # Build channels config dynamically from env vars.
  # Built-in channel extensions need BOTH:
  #   1. A channel block in channels.* with credentials
  #   2. An entry in plugins.allow so the gateway loads the extension
  PLUGINS_LIST='"kars"'
  PLUGINS_ENTRIES='"kars": { "enabled": true }'
  CHANNELS_CONFIG=""

  # Telegram (built into OpenClaw core, uses grammY)
  # Set proxy explicitly so OpenClaw uses a simple ProxyAgent for Telegram API calls.
  # The EnvHttpProxyAgent + autoSelectFamily path causes long-poll stalls in Docker.
  if [ -n "${TELEGRAM_BOT_TOKEN:-}" ]; then
    # Strip "bot" prefix if present — grammY prepends it, so "bot123:ABC" would
    # become "botbot123:ABC" in the API URL, causing 404 from Telegram.
    TELEGRAM_BOT_TOKEN="${TELEGRAM_BOT_TOKEN#bot}"

    # DM policy: allowlist if TELEGRAM_ALLOW_FROM is set (comma-separated numeric IDs),
    # otherwise pairing (requires approval). Never default to open.
    if [ -n "${TELEGRAM_ALLOW_FROM:-}" ]; then
      # Convert comma-separated IDs to JSON array: "123,456" → "\"123\", \"456\""
      TG_ALLOW_JSON=$(echo "$TELEGRAM_ALLOW_FROM" | tr -d ' ' | sed 's/,/", "/g; s/^/"/; s/$/"/')
      TG_DM_POLICY="allowlist"
      TG_ALLOW_FROM="[${TG_ALLOW_JSON}]"
    else
      TG_DM_POLICY="pairing"
      TG_ALLOW_FROM="[]"
    fi
    CHANNELS_CONFIG="\"telegram\": { \"botToken\": \"${TELEGRAM_BOT_TOKEN}\", \"dmPolicy\": \"${TG_DM_POLICY}\", \"allowFrom\": ${TG_ALLOW_FROM}, \"proxy\": \"http://127.0.0.1:8444\" }"
    PLUGINS_LIST="${PLUGINS_LIST}, \"telegram\""
    [ -n "${PLUGINS_ENTRIES}" ] && PLUGINS_ENTRIES="${PLUGINS_ENTRIES}, "
    PLUGINS_ENTRIES="${PLUGINS_ENTRIES}\"telegram\": { \"enabled\": true }"
  fi

  # WhatsApp (built-in, uses Baileys — QR pairing at runtime)
  if [ -n "${WHATSAPP_ENABLED:-}" ]; then
    WA_EXTRA=""
    [ -n "${CHANNELS_CONFIG}" ] && WA_EXTRA=", "
    CHANNELS_CONFIG="${CHANNELS_CONFIG}${WA_EXTRA}\"whatsapp\": { \"dmPolicy\": \"pairing\", \"allowFrom\": [\"*\"] }"
    PLUGINS_LIST="${PLUGINS_LIST}, \"whatsapp\""
    [ -n "${PLUGINS_ENTRIES}" ] && PLUGINS_ENTRIES="${PLUGINS_ENTRIES}, "
    PLUGINS_ENTRIES="${PLUGINS_ENTRIES}\"whatsapp\": { \"enabled\": true }"
  fi

  # Slack (built-in, uses Bolt)
  if [ -n "${SLACK_BOT_TOKEN:-}" ]; then
    SLACK_EXTRA=""
    [ -n "${CHANNELS_CONFIG}" ] && SLACK_EXTRA=", "
    CHANNELS_CONFIG="${CHANNELS_CONFIG}${SLACK_EXTRA}\"slack\": { \"botToken\": \"${SLACK_BOT_TOKEN}\", \"dmPolicy\": \"open\", \"allowFrom\": [\"*\"] }"
    PLUGINS_LIST="${PLUGINS_LIST}, \"slack\""
    [ -n "${PLUGINS_ENTRIES}" ] && PLUGINS_ENTRIES="${PLUGINS_ENTRIES}, "
    PLUGINS_ENTRIES="${PLUGINS_ENTRIES}\"slack\": { \"enabled\": true }"
  fi

  # Discord (built-in, uses discord.js)
  if [ -n "${DISCORD_BOT_TOKEN:-}" ]; then
    DISC_EXTRA=""
    [ -n "${CHANNELS_CONFIG}" ] && DISC_EXTRA=", "
    CHANNELS_CONFIG="${CHANNELS_CONFIG}${DISC_EXTRA}\"discord\": { \"botToken\": \"${DISCORD_BOT_TOKEN}\", \"dmPolicy\": \"pairing\", \"allowFrom\": [\"*\"] }"
    PLUGINS_LIST="${PLUGINS_LIST}, \"discord\""
    [ -n "${PLUGINS_ENTRIES}" ] && PLUGINS_ENTRIES="${PLUGINS_ENTRIES}, "
    PLUGINS_ENTRIES="${PLUGINS_ENTRIES}\"discord\": { \"enabled\": true }"
  fi

  # Default: no channels configured
  if [ -z "${CHANNELS_CONFIG}" ]; then
    CHANNELS_CONFIG="\"_placeholder\": false"
  fi

  # Third-party plugins: auto-enable when API keys are present.
  # OpenClaw reads the env vars directly for auth — we just need to register them
  # in plugins.allow + plugins.entries so the gateway loads them.
  for plugin_pair in \
    "brave:BRAVE_API_KEY" \
    "tavily:TAVILY_API_KEY" \
    "exa:EXA_API_KEY" \
    "firecrawl:FIRECRAWL_API_KEY" \
    "perplexity:PERPLEXITY_API_KEY" \
    "openai:OPENAI_API_KEY"; do
    plugin_id="${plugin_pair%%:*}"
    env_var="${plugin_pair##*:}"
    eval env_val="\${${env_var}:-}"
    if [ -n "$env_val" ]; then
      PLUGINS_LIST="${PLUGINS_LIST}, \"${plugin_id}\""
      [ -n "${PLUGINS_ENTRIES}" ] && PLUGINS_ENTRIES="${PLUGINS_ENTRIES}, "
      PLUGINS_ENTRIES="${PLUGINS_ENTRIES}\"${plugin_id}\": { \"enabled\": true }"
    fi
  done

  sed -i "s|PLUGINS_ALLOW_PLACEHOLDER|${PLUGINS_LIST}|" "$OPENCLAW_CONFIG"
  sed -i "s|PLUGINS_ENTRIES_PLACEHOLDER|${PLUGINS_ENTRIES}|" "$OPENCLAW_CONFIG"
  sed -i "s|CHANNELS_PLACEHOLDER|${CHANNELS_CONFIG}|" "$OPENCLAW_CONFIG"
  # Remove placeholder if no channels
  sed -i '/"_placeholder": false/d' "$OPENCLAW_CONFIG"

  # Set provider credentials via environment (OpenClaw reads these automatically)
  # Read from secret file at runtime — avoid leaking key value in process tree
  export AZURE_OPENAI_API_KEY
  # `|| :` ensures the assignment succeeds even when neither file exists —
  # critical on AKS workload-identity where there is no API-key secret mounted
  # (router uses managed-identity to talk to Foundry, no key needed).
  # Without this fallback, `set -e` exits the script when both cats fail.
  AZURE_OPENAI_API_KEY="$(cat /run/secrets/azure-openai-key 2>/dev/null || cat /tmp/azure-openai-key 2>/dev/null || :)"
  export AZURE_OPENAI_ENDPOINT="${ENDPOINT}"

  # AGT governance is always active in kars sandboxes (enables agt-governance skill)
  export AGT_GOVERNANCE_ENABLED=true

  # Foundry project endpoint (for standalone APIs: Memory Store, Foundry IQ, etc.)
  FOUNDRY_PROJECT_ENDPOINT="${FOUNDRY_PROJECT_ENDPOINT:-}"
  # Foundry Agent ID (only needed for tools requiring agent runs: code_interpreter, web_search)
  FOUNDRY_AGENT_ID="${FOUNDRY_AGENT_ID:-}"

  # Write env exports to a sandbox-specific file (overwritten every boot, no
  # accumulation across pod restarts on persistent /sandbox volumes). Then ensure
  # .bashrc sources it exactly once.
  cat > /sandbox/.kars-env.sh << RCEOF
# kars: Azure OpenAI credentials (loaded from /run/secrets/)
# Auto-generated every container boot — do not edit by hand.
export AZURE_OPENAI_API_KEY="\$(cat /run/secrets/azure-openai-key 2>/dev/null)"
export AZURE_OPENAI_ENDPOINT="${ENDPOINT}"
export OPENCLAW_MODEL="${MODEL}"
export FOUNDRY_PROJECT_ENDPOINT="${FOUNDRY_PROJECT_ENDPOINT}"
export FOUNDRY_AGENT_ID="${FOUNDRY_AGENT_ID}"
RCEOF
  if ! grep -q "kars-env.sh" /sandbox/.bashrc 2>/dev/null; then
    printf '\n# kars env (managed by entrypoint)\n[ -f /sandbox/.kars-env.sh ] && . /sandbox/.kars-env.sh\n' >> /sandbox/.bashrc
  fi

  # Write minimal workspace files so OpenClaw doesn't need onboarding
  cat > "$WORKSPACE_DIR/AGENTS.md" << AGENTSEOF
# kars Agent

You are a helpful AI assistant running inside an **kars** sandbox — a secure,
open-source runtime for AI agents on Azure Kubernetes Service (AKS).

## On First Message (Welcome)

When a user starts a new conversation, include the following in your greeting
for security transparency:

1. **Header**: "🔒 kars Sandbox — Secure AI Runtime on Azure"
2. **Foundry Project**: Show the connected project: \`${FOUNDRY_PROJECT_ENDPOINT:-${ENDPOINT}}\`
3. **Model**: Show the active model: \`${MODEL}\`
4. **Sandbox ID**: Show the sandbox name: \`\${HOSTNAME:-dev}\`
5. **Security summary**: Mention that the environment is sandboxed (isolated container, read-only rootfs, seccomp, egress policy, Content Safety + Prompt Shields)
6. **Capabilities list**: Briefly list what you can do (code execution, web search, document search, memory, sub-agents, etc.)
7. **Invitation**: Ask how you can help

Format this nicely with the header as a bold or prominent line. The Foundry
project endpoint and model should be visible so the user knows which backend
they are connected to. Include the Foundry project line even when it says
"Not configured".

## Capabilities
- You can help with coding, analysis, writing, and general questions
- You have access to shell/exec tools for running commands inside the sandbox
- You can read files, run system commands (uname, hostname, cat /etc/os-release, etc.)
- **Python 3** is installed with: pandas, numpy, scipy, sympy, matplotlib, seaborn, requests, httpx, beautifulsoup4, lxml, cssselect, aiohttp, websockets, rich, tabulate, pdfplumber, pypdf, python-docx, openpyxl, python-pptx, Pillow, jinja2, pydantic, jsonpath-ng, xmltodict, markdown, html2text, chardet, python-dateutil, pyyaml, toml, python-dotenv, sqlalchemy, cryptography, tiktoken, dnspython, networkx, geopy, ftfy, unidecode, qrcode, fpdf2
- Your workspace is /sandbox — all your files live here
- Your network access is governed by policy — unauthorized endpoints will be blocked

## Foundry Tools (first-class, always available)
- \`foundry_code_execute\` — Python code execution (pandas, numpy, matplotlib, scipy)
- \`foundry_web_search\` — Real-time web search with Bing grounding + citations
- \`foundry_file_search\` — RAG over vector stores and Azure AI Search indexes
- \`foundry_memory\` — Persistent semantic memory (search/update/delete across sessions)
- \`foundry_conversations\` — Persistent multi-turn conversations (server-side state)
- \`foundry_evaluations\` — Model quality testing and benchmarks
- \`foundry_deployments\` — Discover available models, connections, indexes
- \`foundry_agents\` — List and query Foundry-hosted agents
- \`http_fetch\` — External HTTP via egress proxy (blocklist + allowlist enforced)

## IMPORTANT: Use Tools Proactively
When the user asks about the Foundry project, deployed models, connections, indexes,
agents, or anything discoverable — **call the relevant tool** (e.g. \`foundry_deployments\`
with operation "list_deployments") and show LIVE data. Do NOT just recite what you see in
MEMORY.md or AGENTS.md — that may be stale. Always prefer real-time tool calls over static
knowledge when the information can be fetched dynamically.

Examples:
- "What models are available?" → call \`foundry_deployments\` with resource "models"
- "What connections do I have?" → call \`foundry_deployments\` with resource "connections"
- "What indexes exist?" → call \`foundry_deployments\` with resource "indexes"
- "Tell me about my Foundry project" → call ALL THREE above and present the results together
- "Search for X in my docs" → call \`foundry_file_search\`
- "Remember this for later" → call \`foundry_memory\` with operation "update"

## Inter-Agent Communication (E2E Encrypted)
You can spawn sub-agents and communicate with them via Signal Protocol E2E
encryption. Use the mesh tools for inter-agent communication and read replies
from the mesh inbox rather than guessing at them.

### Workflow for sub-agent tasks:
1. **Spawn**: Call \`kars_spawn\` with a name — it returns when the sub-agent is Running
2. **Send**: Call \`kars_mesh_send\` with \`to_agent\` and \`content\` — encrypted via AGT relay
3. **Wait & Read**: Call \`kars_mesh_inbox\` to check for replies (retry if empty)
4. **Destroy**: Call \`kars_spawn_destroy\` when done

### Notes:
- Read sub-agent replies from \`kars_mesh_inbox\` rather than inventing them
- If \`kars_mesh_inbox\` returns no messages, wait and retry (up to 60 seconds)
- All messages are E2E encrypted (Signal Protocol) — the relay cannot read them

### Files received from other agents
When another agent sends you a file via the mesh (\`file_transfer\` message), it is
automatically saved to TWO locations:
1. \`/sandbox/.openclaw/workspace/incoming/<filename>\` — original landing spot (provenance)
2. \`/sandbox/.openclaw/workspace/<filename>\` — promoted to workspace root for direct use

**Always check both locations before falling back to placeholder assets.** Before generating
synthetic/placeholder versions of images, charts, PDFs, or other artifacts, run a quick
\`exec ls /sandbox/.openclaw/workspace /sandbox/.openclaw/workspace/incoming\` (or use \`read\`)
to verify nothing was already transferred. Inbox entries also include a \`workspace_path\`
field pointing at the usable copy.

## Handling Tasks from Other Agents (AGT Mesh)
When you receive a task from another agent via the AGT mesh, execute it using your full
toolset. Prioritize these Foundry-powered tools for the best results:

- **Research & current data**: Use \`web_search\` and \`web_fetch\` for live information
- **Data analysis**: Use \`foundry_code_execute\` for Python data processing (pandas, numpy, matplotlib, scipy)
- **Knowledge recall**: Use \`foundry_memory\` to search for relevant context, and store findings
- **Document search**: Use \`foundry_file_search\` for vector store lookups
- **Long-running work**: Use \`process\` for background tasks, \`exec\` for shell commands
- **File I/O**: Use \`read\`/\`write\`/\`edit\` for workspace files in /sandbox

Always store important findings in \`foundry_memory\` so they persist across sessions.
Be thorough — you have the full OpenClaw toolset, so use it.

## Security Context
- Running as non-root user (sandbox:1000)
- Read-only root filesystem, writable: /sandbox and /tmp only
- All syscalls filtered by seccomp
- Inference routed through Content Safety + Prompt Shields
- Token budgets enforced per sandbox
- Egress controlled: blocklist (51K+ domains), allowlist, learn mode, pending approval

## Egress Management
Network egress starts in **learn mode** — all domains are allowed and recorded.
The operator can graduate to enforcement with \`kars egress <name> --enforce\`,
which promotes learned domains to the allowlist. After that, new domains require approval.
AGENTSEOF

  # Write TOOLS.md describing available Foundry endpoints
  cat > "$WORKSPACE_DIR/TOOLS.md" << 'TOOLSEOF'
# kars Tools

All tools are accessed via the inference router at http://localhost:8443.
Authentication is handled automatically — no API keys needed.

## Inference (working)
- `POST /v1/chat/completions` — chat with any Foundry model (200+ catalog)
- `POST /v1/completions` — text completion
- `POST /v1/embeddings` — generate embeddings
- `GET /v1/models` — list available models

## Foundry Standalone APIs (all via inference router, no API keys needed)

### Memory Store
- \`GET /memory_stores\` — list memory stores
- \`POST /memory_stores/{name}:search_memories\` — semantic search with embeddings
- \`POST /memory_stores/{name}:update_memories\` — write conversation to memory (async)
- \`GET /memory_stores/{name}/updates/{id}\` — check update status

### Responses API (Code Interpreter, Web Search, Memory, Knowledge)
- \`POST /openai/responses\` — query with tools: code_interpreter, bing_grounding, memory_search, file_search, azure_ai_search

### Conversations
- \`POST /openai/conversations\` — create persistent conversation
- \`GET /openai/conversations\` — list conversations

### Evaluations
- \`GET /evaluators\` — list evaluator catalog
- \`GET /evaluationrules\` — list evaluation rules
- \`POST /openai/evals\` — create evaluation run

### Infrastructure
- \`GET /deployments\` — list deployed models (names, publishers, versions)
- \`GET /connections\` — list project data connections
- \`GET /indexes\` — list knowledge indexes
- \`GET /datasets\` — list datasets
- \`GET /insights\` — monitoring insights
- \`GET /agents\` — list Foundry agents

## Health & Metrics
- `GET /healthz` — readiness check
- `GET /metrics` — Prometheus metrics (tokens, latency, requests)

## External HTTP Access (Egress Proxy)
Direct internet access is blocked by security policy. To make external HTTP requests
(APIs, webhooks, etc.), use the egress proxy which checks the allowlist and blocklist:

- `POST /egress/fetch` — make an external HTTP request
  Body: `{"url": "https://...", "method": "GET", "headers": {}, "body": ""}`
  Returns: `{"status": 200, "headers": {...}, "body": "..."}`

If a domain is not on the allowlist, the request is denied and a pending approval is
created. The operator can approve it with `kars egress <name> --approve <domain>`.

**Example:**
```bash
curl -s -X POST http://localhost:8443/egress/fetch \
  -H "Content-Type: application/json" \
  -d '{"url":"https://api.telegram.org/bot.../getMe","method":"GET"}'
```

**IMPORTANT:** Do NOT use `curl https://...` directly — it will time out.
Always use `curl http://localhost:8443/egress/fetch` with the target URL in the body.
TOOLSEOF

  cat > "$WORKSPACE_DIR/SOUL.md" << SOULEOF
# Soul

You are **kars Agent** — a secure, sandboxed AI assistant powered by Azure AI Foundry.

You run inside an isolated, hardened container on Azure Linux. Your inference is routed
through the kars inference router which provides Content Safety, Prompt Shields,
token budgets, and egress control.

**Connected project**: ${FOUNDRY_PROJECT_ENDPOINT:-${ENDPOINT}}
**Primary model**: ${MODEL}

When greeting users for the first time, be warm and welcoming. Briefly mention you're
running in kars, what model you're using, and what you can help with. Don't be
robotic — be genuinely helpful and excited to assist.

You are friendly, concise, and technically excellent. You get things done efficiently.
When you don't know something, say so. When you can use a Foundry tool to help, do it
proactively without asking for permission.

When processing a task from another agent (via AGT mesh), be thorough and autonomous.
Use all available tools — especially Foundry tools (\`foundry_memory\`, \`web_search\`,
\`foundry_code_execute\`) — to produce a comprehensive, actionable result. Don't ask
for clarification; interpret the task as given and deliver your best work.

For memory: write important facts, preferences, and decisions to memory files so they
persist across sessions. Use \`foundry_memory\` for cross-agent/cross-session recall.
SOULEOF

  echo "[kars] OpenClaw configured — model: ${MODEL}, endpoint: ${ENDPOINT}"
else
  # Load credentials for existing config
  export AZURE_OPENAI_API_KEY="${API_KEY}"
  export AZURE_OPENAI_ENDPOINT="${ENDPOINT}"
  echo "[kars] OpenClaw already configured"
fi

# Always re-install kars plugin from the image (plugin code may have changed
# even though config persists on the volume). This is safe because the plugin
# directory is small and cp is idempotent.
#
# IMPORTANT: `/opt/kars-plugin` is chmod -R a-w in the Dockerfile (image-level
# hardening). Default `cp` preserves source mode, so destination files end up
# read-only (444) and directories 555. That makes subsequent container restarts
# (emptyDir survives restart — only pod recreation wipes it) fail with
# "Permission denied" when trying to overwrite. We solve this two ways:
#   1. Remove the stale install tree up front, so overwrites never happen.
#   2. Pass `--no-preserve=mode` so newly-copied files are writable by the owner
#      even if the next restart still needs to write (belt-and-suspenders).
if [ -d /opt/kars-plugin ]; then
  # Clean slate — the plugin is small and always comes from the image.
  rm -rf "$OPENCLAW_DIR/extensions/kars" 2>/dev/null || true
  mkdir -p "$OPENCLAW_DIR/extensions/kars/dist"
  # NOTE: on some Docker Desktop hosts, fchmod(2) on newly-written files in
  # the sandbox volume returns EPERM even though the file content is written
  # successfully. We tolerate cp's non-zero exit (content is what matters; the
  # mode bits are re-applied later by the hardening block at the bottom of
  # this script) via `|| true` on every cp. This keeps `set -e` semantics for
  # the rest of the script.
  cp --no-preserve=mode /opt/kars-plugin/package.json "$OPENCLAW_DIR/extensions/kars/" 2>/dev/null || true
  cp --no-preserve=mode /opt/kars-plugin/openclaw.plugin.json "$OPENCLAW_DIR/extensions/kars/" 2>/dev/null || true
  # Copy built JS/TS output
  cp -r --no-preserve=mode /opt/kars-plugin/*.js "$OPENCLAW_DIR/extensions/kars/dist/" 2>/dev/null || true
  cp -r --no-preserve=mode /opt/kars-plugin/*.d.ts "$OPENCLAW_DIR/extensions/kars/dist/" 2>/dev/null || true
  cp -r --no-preserve=mode /opt/kars-plugin/*.map "$OPENCLAW_DIR/extensions/kars/dist/" 2>/dev/null || true
  if [ -d /opt/kars-plugin/commands ]; then
    cp -r --no-preserve=mode /opt/kars-plugin/commands "$OPENCLAW_DIR/extensions/kars/dist/" 2>/dev/null || true
  fi
  # Copy ./core/ subdirectory (Phase 1 hotspot decomposition: foundry-discovery
  # and future extracted modules live under core/). plugin.js requires
  # './core/foundry-discovery.js' (commit a33165b) — without this copy the
  # whole plugin fails to load with "Cannot find module './core/...'".
  if [ -d /opt/kars-plugin/core ]; then
    cp -r --no-preserve=mode /opt/kars-plugin/core "$OPENCLAW_DIR/extensions/kars/dist/" 2>/dev/null || true
  fi
  # Copy Foundry skills (SKILL.md files) — but skip foundry-* in GH Models
  # mode. GH Models has a hard 16k input-token cap; the 9 Foundry skills add
  # ~25k bytes of prompt fragments + tool schemas that the LLM never gets to
  # use (no Azure project exists), and they push every chat turn past 413.
  if [ -d /opt/kars-plugin/skills ]; then
    cp -r --no-preserve=mode /opt/kars-plugin/skills "$OPENCLAW_DIR/extensions/kars/" 2>/dev/null || true
    mkdir -p "$WORKSPACE_DIR/skills"
    if [ "${KARS_PROVIDER:-}" = "github-models" ] || [ "${KARS_PROVIDER:-}" = "github-copilot" ]; then
      # GH-token providers (Models / Copilot) have no Foundry project — skip
      # all foundry-* skills (they'd just register tools the LLM can't use).
      # Copilot doesn't have the 16k input cap, but the empty-Foundry tool
      # registrations are still pure noise.
      for skill_dir in /opt/kars-plugin/skills/*/; do
        name=$(basename "$skill_dir")
        case "$name" in
          foundry-*) continue ;;
        esac
        cp -r --no-preserve=mode "$skill_dir" "$WORKSPACE_DIR/skills/" 2>/dev/null || true
      done
      echo "[kars] ${KARS_PROVIDER} mode: governance + spawn skills installed (Foundry skills skipped)"
    else
      cp -r --no-preserve=mode /opt/kars-plugin/skills/* "$WORKSPACE_DIR/skills/" 2>/dev/null || true
      echo "[kars] Foundry + governance skills installed (plugin + workspace)"
    fi
  fi
  # Copy pre-installed ClawHub skills (from Docker build)
  if [ -d /opt/clawhub-skills ] && [ "$(ls -A /opt/clawhub-skills 2>/dev/null)" ]; then
    mkdir -p "$WORKSPACE_DIR/skills"
    cp -r --no-preserve=mode /opt/clawhub-skills/* "$WORKSPACE_DIR/skills/" 2>/dev/null || true
    CLAWHUB_COUNT=$(ls -d /opt/clawhub-skills/*/ 2>/dev/null | wc -l)
    echo "[kars] ClawHub skills installed: ${CLAWHUB_COUNT} (pre-built)"
  fi
  # Copy node_modules so the plugin can resolve runtime deps (ws, etc).
  # `-L` dereferences symlinks: the @kars/mesh entry is a `file:` dep
  # symlink → /mesh-plugin. Without -L, cp keeps the symlink and Node fails
  # to resolve `@kars/mesh` at runtime.
  if [ -d /opt/kars-plugin/node_modules ]; then
    cp -rL --no-preserve=mode /opt/kars-plugin/node_modules "$OPENCLAW_DIR/extensions/kars/" 2>/dev/null || true
    echo "[kars] @kars/mesh runtime deps available"
  fi
  # Mesh provider — only `agt` is supported (@microsoft/agent-governance-sdk
  # via @kars/mesh). The vendored fork was removed in Phase 5.2.
  KARS_MESH_PROVIDER="agt"
  echo "[kars] mesh provider: agt (@microsoft/agent-governance-sdk)"
  export KARS_MESH_PROVIDER
  # Locate the AGT policy directory when governance is enabled.
  #
  # Post-Slice-1e (phase 2): the ToolPolicy mount at
  # `/etc/agt/policies/agt-profile.yaml` is the **sole** source of AGT
  # policy bytes. The bundled `/opt/kars-plugin/policies/` fallback
  # path was removed; the controller now hard-fails (Degraded /
  # SpecInvalid) any sandbox whose referenced ToolPolicy lacks
  # `spec.agtProfile.inline`. If the mount is somehow missing here, the
  # AGT engine starts with an empty policy set and fails closed at the
  # policy layer (see comment near AGT_POLICY_DIR default below).
  if [ "${AGT_GOVERNANCE_ENABLED:-}" = "true" ]; then
    if [ -f /etc/agt/policies/agt-profile.yaml ]; then
      export AGT_POLICY_DIR=/etc/agt/policies
      echo "[kars] AGT governance enabled (source: ToolPolicy mount /etc/agt/policies, trust threshold: ${AGT_TRUST_THRESHOLD:-500})"
    else
      echo "[kars] WARN: AGT governance enabled but no ToolPolicy mount at /etc/agt/policies/agt-profile.yaml; AGT engine will start with an empty policy set and fail closed."
    fi
  fi
  cd /sandbox
  echo "[kars] Plugin installed → openclaw kars commands available"
fi

# ── Always-required runtime cores (speech-core, image-generation-core,
#    media-understanding-core) ─────────────────────────────────────────────
# OpenClaw 2026.4.x's facade resolver hard-requires these three cores at
# NOTE: The 3 always-required runtime cores (speech-core, image-generation-core,
# media-understanding-core) are made available via OPENCLAW_BUNDLED_PLUGINS_DIR
# pointing at a pruned dist-runtime/extensions/ tree built at image build time.
# See the export earlier in this script for the full rationale.

# Write gateway token to .bashrc (remove any stale tokens from prior runs first)
touch /sandbox/.bashrc 2>/dev/null || true
sed -i '/OPENCLAW_GATEWAY_TOKEN/d' /sandbox/.bashrc 2>/dev/null || true
echo "export OPENCLAW_GATEWAY_TOKEN=\"${GATEWAY_TOKEN}\"" >> /sandbox/.bashrc
# Also write to a dedicated file so dev.ts can read it without parsing .bashrc
echo "${GATEWAY_TOKEN}" > /tmp/gateway-token

# Ensure all sandbox files are owned by sandbox user
[ "$IS_ROOT" = "true" ] && { chown -R sandbox:sandbox /sandbox 2>/dev/null || true; }

# ── Code integrity hardening ──────────────────────────────────────────────
# After the blanket chown, lock down all executable code so the agent (UID 1000)
# cannot modify its own plugin, SDK, or governance policies at runtime.
# This prevents prompt-injection attacks that instruct the agent to patch its
# own safety checks, exfiltrate data through modified code, or disable E2E
# encryption / governance enforcement.
#
# Pattern: root owns the code, sandbox user gets read + execute only.
# Same approach already used for policy YAML files.
# NOTE: chmod/chown failures are tolerated (|| true) — some Docker Desktop
# hosts return EPERM on fchmod against the sandbox volume. Hardening is
# best-effort; the content is correct either way.
if [ "$IS_ROOT" = "true" ]; then
  # Plugin code (JS, type defs, source maps, manifests)
  PLUGIN_DIR="$OPENCLAW_DIR/extensions/kars"
  if [ -d "$PLUGIN_DIR" ]; then
    chown -R root:sandbox "$PLUGIN_DIR" 2>/dev/null || true
    # Directories: read + execute (traverse) for sandbox group
    find "$PLUGIN_DIR" -type d -exec chmod 750 {} + 2>/dev/null || true
    # Files: read-only for sandbox group
    find "$PLUGIN_DIR" -type f -exec chmod 640 {} + 2>/dev/null || true
    echo "[kars] Plugin code hardened (root-owned, read-only for sandbox)"
  fi

  # AGT policy files now live in /etc/kars/policies/ — hardened above
  # at copy time (root:root, 0444). No further hardening needed here.

  # Curated skills installed into workspace (SKILL.md files)
  if [ -d "$WORKSPACE_DIR/skills" ]; then
    chown -R root:sandbox "$WORKSPACE_DIR/skills" 2>/dev/null || true
    find "$WORKSPACE_DIR/skills" -type d -exec chmod 750 {} + 2>/dev/null || true
    find "$WORKSPACE_DIR/skills" -type f -exec chmod 640 {} + 2>/dev/null || true
  fi
fi

# Start kars inference router as UID 1001 (router user) — only in dev mode.
# UID 1001 is exempt from iptables egress guard, matching the AKS pod model
# where the router runs in a separate container with internet access.
# In AKS, the controller deploys the router as a separate container.
if [ "${KARS_AUTH_MODE:-}" != "workload-identity" ]; then
  # Generate admin token BEFORE starting any services that need it
  ROUTER_ADMIN_TOKEN=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' | head -c 64)
  # Token file readable by both sandbox (UID 1000) and router (UID 1001)
  echo "$ROUTER_ADMIN_TOKEN" > /tmp/.agt-admin-token
  if [ "$IS_ROOT" = "true" ]; then
    chown sandbox:sandbox /tmp/.agt-admin-token
    chmod 440 /tmp/.agt-admin-token
    # Add router user to sandbox group so UID 1001 can read the token file.
    # Azure Linux 3.0 ships shadow-utils where `adduser` is `useradd`; the
    # Debian-style `adduser <user> <group>` syntax silently fails. Use
    # usermod/gpasswd which exist on both shadow-utils and busybox.
    usermod -aG sandbox router 2>/dev/null \
      || gpasswd -a router sandbox 2>/dev/null \
      || true
  else
    chmod 400 /tmp/.agt-admin-token
  fi

  # ── AGT governance (native in router) ─────────────────────────────────────
  # The inference-router handles governance natively via the agentmesh crate.
  # Ensure trust store directory exists for the router (UID 1001).
  mkdir -p /tmp/agt
  if [ "$IS_ROOT" = "true" ]; then
    chown 1001:1001 /tmp/agt
    chmod 700 /tmp/agt 2>/dev/null || true
  fi

  # Ensure router can write its log file (remove stale file from previous runs).
  # NOTE: Do NOT chown the log file to UID 1001. The shell redirect `> file`
  # below is opened by THIS shell (root in dev). The fd is then inherited by
  # `runuser` and the router process via exec — they don't need to open the
  # file themselves, just inherit the writable fd. Counterintuitively, on
  # Docker Desktop (and possibly some LSM-hardened kernels), once a file is
  # chowned to UID 1001 root can no longer open it for write even with
  # CAP_DAC_OVERRIDE and mode 666 — so chowning the log here breaks the
  # redirect with EACCES. Keeping it root-owned is correct: only the parent
  # shell needs to open it; the router writes via the inherited fd.
  rm -f /tmp/inference-router.log
  touch /tmp/inference-router.log
  # Dev mode: make Docker socket accessible to router (UID 1001) for sub-agent spawning
  if [ -S /var/run/docker.sock ] && [ "$IS_ROOT" = "true" ]; then
    chmod 666 /var/run/docker.sock || true
  fi
  ROUTER_PORT=8443 \
  ADMIN_TOKEN="$ROUTER_ADMIN_TOKEN" \
  AZURE_OPENAI_ENDPOINT="$ENDPOINT" \
  AZURE_OPENAI_API_KEY="$API_KEY" \
  DEFAULT_MODEL="$MODEL" \
  KARS_PROVIDER="${KARS_PROVIDER:-}" \
  COPILOT_GITHUB_TOKEN="$([ "${KARS_PROVIDER:-}" = "github-copilot" ] && echo "$API_KEY")" \
  CONTENT_SAFETY_ENABLED=true \
  AGT_RELAY_URL="${AGT_RELAY_URL:-}" \
  AGT_REGISTRY_URL="${AGT_REGISTRY_URL:-}" \
  AGT_GOVERNANCE_ENABLED="${AGT_GOVERNANCE_ENABLED:-true}" \
  AGT_POLICY_DIR="${AGT_POLICY_DIR:-/etc/agt/policies}" \
  AGT_TRUST_THRESHOLD="${AGT_TRUST_THRESHOLD:-500}" \
  SANDBOX_NAME="${SANDBOX_NAME:-$HOSTNAME}" \
  SANDBOX_ISOLATION="${SANDBOX_ISOLATION:-enhanced}" \
  KARS_DEV_MODE="${KARS_DEV_MODE:-}" \
  DOCKER_NETWORK="${DOCKER_NETWORK:-}" \
  $AS_ROUTER kars-inference-router > /tmp/inference-router.log 2>&1 &
  ROUTER_PID=$!
  # Wait for router to accept connections (replaces blind sleep 1)
  for _i in $(seq 1 20); do
    if curl -sf http://127.0.0.1:8443/healthz > /dev/null 2>&1; then break; fi
    sleep 0.2
  done
  echo "[kars] Inference router running (PID: $ROUTER_PID, port: 8443)"
else
  echo "[kars] Inference router provided by AKS container (workload-identity mode)"
fi

# ── Offload idle timeout enforcement ────────────────────────────────────────
# When OFFLOAD_TIMEOUT_MINUTES is set (by the controller for offload sandboxes),
# start a background idle watcher. The sandbox self-terminates after the idle
# window elapses with no inbound traffic from OFFLOAD_PARENT_AMID.
#
# Activity signal: the kars-mesh plugin touches $OFFLOAD_ACTIVITY_FILE
# on every decrypted inbound message from the parent AMID. This file's mtime
# is the single source of truth for "last heard from parent".
#
# Exit path: we can't just `kill 1` — bash PID 1 ignores SIGTERM while waiting
# on a foreground `tail -f` (no trap, no exec replacement). Instead we install
# a SIGTERM trap that kills the tail child, letting the script exit cleanly.
OFFLOAD_ACTIVITY_FILE=/tmp/offload-last-activity
export OFFLOAD_ACTIVITY_FILE
if [ -n "${OFFLOAD_TIMEOUT_MINUTES:-}" ] && [ "$OFFLOAD_TIMEOUT_MINUTES" != "0" ]; then
  OFFLOAD_IDLE_SECONDS=$(( OFFLOAD_TIMEOUT_MINUTES * 60 ))
  echo "[kars] Offload sandbox — idle timeout ${OFFLOAD_TIMEOUT_MINUTES}m (${OFFLOAD_IDLE_SECONDS}s) since last parent message"
  echo "[kars] Request ID:  ${OFFLOAD_REQUEST_ID:-unknown}"
  echo "[kars] Parent AMID: ${OFFLOAD_PARENT_AMID:-unknown}"
  # Seed the activity file so we don't self-terminate at T=0 before any
  # message arrives. The watcher starts counting from *now*.
  touch "$OFFLOAD_ACTIVITY_FILE"
  [ "$IS_ROOT" = "true" ] && chown sandbox:sandbox "$OFFLOAD_ACTIVITY_FILE"
  chmod 0664 "$OFFLOAD_ACTIVITY_FILE"
  (
    while :; do
      sleep 60
      now=$(date +%s)
      last=$(stat -c %Y "$OFFLOAD_ACTIVITY_FILE" 2>/dev/null || echo "$now")
      idle=$(( now - last ))
      if [ "$idle" -ge "$OFFLOAD_IDLE_SECONDS" ]; then
        echo "[kars] ⏰ Offload idle ${idle}s ≥ ${OFFLOAD_IDLE_SECONDS}s — shutting down"
        # Killing the foreground `tail -f` child unblocks PID 1 bash,
        # which then hits the trap below and exits the container.
        # The tail PID is written to /tmp/offload-tail.pid by the main script
        # below; reading it here avoids a fork-time variable race.
        tpid=$(cat /tmp/offload-tail.pid 2>/dev/null || true)
        if [ -n "$tpid" ]; then
          kill -TERM "$tpid" 2>/dev/null || true
        fi
        # Belt and suspenders: after 5s, force-kill PID 1.
        sleep 5
        kill -KILL 1 2>/dev/null || true
        exit 0
      fi
    done
  ) &
  IDLE_WATCHER_PID=$!
fi

# ── Normal mode: start gateway + node host + TUI ───────────────────────────

# Start OpenClaw gateway in the background (needed for TUI)
# Set HTTP(S)_PROXY so Node.js uses the forward proxy for outbound connections.
# Transparent TLS tunneling (iptables REDIRECT → SNI extraction) is unreliable
# in Docker's userspace networking; explicit CONNECT proxy mode is robust.
#
# The --require preload script forces undici's EnvHttpProxyAgent as the global
# dispatcher BEFORE any OpenClaw code runs, ensuring Telegram polling, model
# pricing, and all outbound fetches honour the proxy from the first request.
HTTPS_PROXY="http://127.0.0.1:8444" HTTP_PROXY="http://127.0.0.1:8444" \
  NO_PROXY="127.0.0.1,localhost" \
  NODE_OPTIONS="--require /usr/local/lib/proxy-bootstrap.js" \
  NODE_COMPILE_CACHE="/var/tmp/openclaw-compile-cache" \
  OPENCLAW_NO_RESPAWN=1 \
  OPENCLAW_GATEWAY_TOKEN="$GATEWAY_TOKEN" $AS_SANDBOX openclaw gateway --port 18789 > /tmp/gateway.log 2>&1 &
GATEWAY_PID=$!

# Wait for gateway to be ready
for i in $(seq 1 10); do
  if curl -sf http://127.0.0.1:18789/healthz > /dev/null 2>&1; then
    echo "[kars] Gateway running (PID: $GATEWAY_PID)"
    break
  fi
  sleep 0.5
done

# Start the node host — provides shell/exec/filesystem tools to the agent.
# Without this, the agent only has plugin tools (kars) and no local execution.
# Give the node host its own HOME so it generates a separate device fingerprint.
# Without this, it shares the TUI's device ID and blocks TUI pairing (role conflict).
NODE_HOSTNAME=$(cat /proc/sys/kernel/hostname 2>/dev/null || echo "sandbox")
mkdir -p /tmp/node-host-home/.openclaw
[ "$IS_ROOT" = "true" ] && chown -R sandbox:sandbox /tmp/node-host-home
# Create a minimal config for the node-host — it only needs gateway connectivity,
# NOT our plugins. Using the main config causes "plugin not found: kars" crash loops.
#
# `plugins.allow: []` is the original (working) config: bundled plugins load with
# their own runtime-deps, and the loader resolves those deps from the pre-staged
# /tmp/openclaw-stage tree (mirrored from /opt/openclaw-stage at container start
# — see the OPENCLAW_PLUGIN_STAGE_DIR setup near the top of this file). No
# runtime npm install is triggered because every spec is already satisfied in a
# search root.
cat > /tmp/node-host-home/.openclaw/openclaw.json << 'NODECONF'
{
  "gateway": { "port": 18789 },
  "plugins": { "allow": [] }
}
NODECONF
[ "$IS_ROOT" = "true" ] && chown sandbox:sandbox /tmp/node-host-home/.openclaw/openclaw.json
# OPENCLAW_STATE_DIR gives the node-host its own device identity so it doesn't
# share the sandbox's device ID (which causes role-upgrade conflicts for operator clients).
HOME=/tmp/node-host-home OPENCLAW_STATE_DIR=/tmp/node-host-home/.openclaw \
  AGT_SKIP_INIT=1 OPENCLAW_GATEWAY_TOKEN="$GATEWAY_TOKEN" $AS_SANDBOX openclaw node run \
  --host 127.0.0.1 --port 18789 \
  --node-id "node-${NODE_HOSTNAME}" > /tmp/node-host.log 2>&1 &
NODE_PID=$!
echo "[kars] Node host starting (PID: $NODE_PID)"

# Exec approvals are disabled via openclaw.json config (tools.exec.security=full).
# AGT governance is the sole policy authority — no need for OpenClaw's exec approval layer.

# The AGT relay listener is NOT needed as a separate process.
# The plugin running inside the gateway already handles incoming mesh messages:
#   - plugin.ts onMessage → delegateToNativeAgent → openclaw agent --message
#   - The plugin's mesh connection stays alive as long as the gateway runs.
#   - delegateToNativeAgent spawns openclaw agent sessions on the SAME gateway (no conflicts).

# Keep the container alive — don't use exec (it would kill the gateway)
# Instead, wait forever while keeping the gateway backgrounded.
# We track the tail PID so the idle-watcher above can SIGTERM it and unblock
# bash PID 1. Without this, `kill 1` is swallowed and the container never dies.
tail -f /dev/null &
IDLE_TAIL_PID=$!
echo "$IDLE_TAIL_PID" > /tmp/offload-tail.pid
# Trap SIGTERM so docker stop / kubectl delete terminate cleanly.
# (For offload sandboxes the idle watcher also installs a trap earlier, but
# this covers non-offload sandboxes too.)
trap 'kill -TERM "$IDLE_TAIL_PID" 2>/dev/null || true; exit 0' TERM INT
wait "$IDLE_TAIL_PID"
