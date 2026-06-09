#!/bin/bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# kars Hermes sandbox entrypoint.
#
# Translates the kars runtime contract (env vars + mounted files) into
# Hermes' native config format, then starts Hermes. The agent inside the
# pod sees a standard Hermes installation — they don't know they're
# running on kars.
#
# Contract reference: docs/runtimes/CONTRACT.md (v1)
#
# Responsibilities (mirrors openclaw/entrypoint.sh, but for Hermes):
#   1. Set HERMES_HOME (writable emptyDir under /sandbox/.hermes)
#   2. Materialize the kars plugin into $HERMES_HOME/plugins/kars/
#   3. Pin provider base URLs to the inference-router sidecar
#   4. Translate channel tokens (TELEGRAM_BOT_TOKEN etc.) to
#      `hermes config set channels.*.token`
#   5. Translate /etc/kars/mcp/<server>/meta.json mounts to
#      hermes config `mcp_servers.*`
#   6. Disable Hermes' non-local terminal backends (docker, modal,
#      daytona, ssh, singularity) — they would break out of K8s isolation
#   7. Set up iptables egress guard in docker dev (k8s has init container)
#   8. Drop to UID 1000 if started as root (docker dev only) and exec hermes

set -e

# ── UID detection (matches openclaw/entrypoint.sh pattern) ──────────────
# Docker dev: started as root, runuser drops to sandbox UID
# AKS / local-k8s: pod spec runAsUser:1000 — already UID 1000
if [ "$(id -u)" = "0" ]; then
  AS_SANDBOX="runuser -u sandbox --"
  IS_ROOT=true
else
  AS_SANDBOX=""
  IS_ROOT=false
fi

# ── SANDBOX_NAME fallback ────────────────────────────────────────────────
# Controller injects SANDBOX_NAME in k8s. Docker dev may not (set by
# `kars dev` only when --name is passed). Fall back to hostname-strip,
# same convention as openclaw/entrypoint.sh.
if [ -z "${SANDBOX_NAME:-}" ]; then
  SANDBOX_NAME=$(echo "$HOSTNAME" | sed 's/-[a-z0-9]*-[a-z0-9]*$//')
  export SANDBOX_NAME
fi

# ── HERMES_HOME (writable state) ─────────────────────────────────────────
# /sandbox/.hermes is an emptyDir in k8s, host-mount in docker dev.
# Hermes will create $HERMES_HOME/{plugins,sessions,memory,config.yaml,...}
export HERMES_HOME="${HERMES_HOME:-/sandbox/.hermes}"
mkdir -p "$HERMES_HOME"

# Hermes' multi-profile support — pin to SANDBOX_NAME so multi-sandbox
# concurrent runs don't share session state.
export HERMES_PROFILE="${HERMES_PROFILE:-$SANDBOX_NAME}"

# Disable Hermes' own sandbox backends — only `local` is valid inside a
# kars pod. The other backends would either fail (no docker socket inside
# a sandbox) or break out of K8s isolation entirely (modal/daytona spin
# up cloud VMs).
export HERMES_DISABLED_BACKENDS="${HERMES_DISABLED_BACKENDS:-docker,modal,daytona,ssh,singularity}"

# ── Materialize the kars plugin into ~/.hermes/plugins/kars/ ─────────────
# Image stages it at /opt/kars-hermes-stage/plugins/kars/. We mirror to
# $HERMES_HOME/plugins/kars/ on every boot so plugin updates ship with
# the image, not stuck in the user's writable state.
KARS_PLUGIN_DST="$HERMES_HOME/plugins/kars"
if [ -d /opt/kars-hermes-stage/plugins/kars ]; then
  rm -rf "$KARS_PLUGIN_DST" 2>/dev/null || true
  mkdir -p "$HERMES_HOME/plugins"
  # `cp -a` preserves owner+mode metadata, which fails as UID 1000
  # on a readOnlyRootFilesystem pod (the source files were chown'd
  # to root:root at image build time; preserving that ownership
  # under a non-root user → EPERM, and `set -e` then kills the
  # container with the cryptic
  # `cp: preserving permissions ... Operation not permitted`
  # spam that has zero context. Use `cp -r` (recursive but no
  # metadata preservation) instead — every file ends up owned by
  # whatever UID is doing the copy (sandbox=1000), which is what
  # we want anyway because that's the UID hermes runs as.
  cp -r /opt/kars-hermes-stage/plugins/kars "$KARS_PLUGIN_DST"
  # Source files in the staged image have 0444 from the
  # `chmod -R a+rX` in the Dockerfile, so they remain readable
  # post-copy; no follow-up chmod needed.
  if [ "$IS_ROOT" = "true" ]; then
    chown -R sandbox:sandbox "$HERMES_HOME" 2>/dev/null || true
  fi
fi

# ── Provider base URLs pinned to the inference-router sidecar ───────────
# Every model call goes through the router so governance applies. The
# router supports OpenAI-compatible /v1/chat/completions, Anthropic
# Messages, Foundry data plane, etc.
export OPENAI_BASE_URL="${OPENAI_BASE_URL:-http://127.0.0.1:8443/v1}"
export OPENAI_API_KEY="${OPENAI_API_KEY:-router-managed}"

# Anthropic via the router's /anthropic/v1/messages proxy
export ANTHROPIC_BASE_URL="${ANTHROPIC_BASE_URL:-http://127.0.0.1:8443/anthropic/v1}"
export ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-router-managed}"

# Azure OpenAI: routed through the router's /v1 namespace too. Hermes'
# `hermes model` picker maps `azure:<deployment>` to OPENAI_BASE_URL when
# AZURE_OPENAI_API_KEY is set; we use the router-managed sentinel.
if [ -n "${AZURE_OPENAI_ENDPOINT:-}" ]; then
  # Display only — the runtime sees the upstream Azure endpoint for
  # diagnostics, but actually calls go via OPENAI_BASE_URL.
  export AZURE_OPENAI_API_KEY="${AZURE_OPENAI_API_KEY:-router-managed}"
fi

# Hermes' default provider per kars dev creds. When operator picks
# github-copilot or github-models, KARS_PROVIDER is set by the
# controller / `kars dev` CLI; Hermes' tool catalogue is filtered
# accordingly (skip Foundry tools in slim modes).
HERMES_DEFAULT_PROVIDER="azure-openai"
case "${KARS_PROVIDER:-}" in
  github-copilot)
    HERMES_DEFAULT_PROVIDER="openai"  # Hermes treats copilot as OpenAI-compat
    ;;
  github-models)
    HERMES_DEFAULT_PROVIDER="openai"
    ;;
esac
export HERMES_DEFAULT_PROVIDER

# ── KARS_MCP_SERVERS → hermes mcp_servers.* config ──────────────────────
# Hermes' native MCP client reads $HERMES_HOME/config.yaml's mcp_servers
# section. The controller projects the list of mirrored McpServer CR
# names into the agent container as KARS_MCP_SERVERS (comma-separated;
# see controller/src/reconciler/mod.rs ~line 2395). For each name we
# emit one mcp_servers.<name> entry pointing at the loopback router
# (`127.0.0.1:8443/mcp`) and tagging the request with the
# `x-kars-mcp-server` header so the router can sign + resolve + forward
# to the real upstream URL (with JWKS validation against the per-server
# mounts at /etc/kars/mcp/<name>/jwks.json that go to the *router*, not
# the agent — agents never see McpServer URLs or bearer tokens directly).
#
# Additionally: when FOUNDRY_PROJECT_ENDPOINT is set, we register the
# kars-internal "platform" MCP server which exposes all 9 Foundry tools
# (foundry_code_execute, foundry_image_generation, foundry_web_search,
# foundry_file_search, foundry_memory, foundry_conversations,
# foundry_evaluations, foundry_deployments, foundry_agents). The router
# serves these at POST /platform/mcp and they're already governed by
# AGT policy + content safety + token budgets. Hermes' MCP client
# discovers them automatically.
HERMES_CONFIG="$HERMES_HOME/config.yaml"
MCP_FRAGMENT="$HERMES_CONFIG.mcp-fragment"

echo "[kars-hermes] Building MCP server config in $HERMES_CONFIG"
{
  echo "# Auto-generated by kars-hermes-entrypoint at $(date -u +%FT%TZ)"
  # Plugin allow-list. Hermes treats `standalone` plugins as opt-in via
  # plugins.enabled — without this, the kars plugin we materialized into
  # $HERMES_HOME/plugins/kars/ is discovered but never loaded. Keep this
  # block in lockstep with runtimes/hermes/src/kars_runtime_hermes/plugin/plugin.yaml
  # (where `name: kars` is declared).
  echo "plugins:"
  echo "  enabled:"
  echo "    - kars"
  # Model + provider: pin Hermes to azure-foundry routing through the
  # loopback router. Without this, Hermes' resolve_provider() falls back
  # to whichever provider has env credentials — OPENAI_API_KEY → openrouter
  # (which then phones home to openrouter.ai), or Nous Portal OAuth (which
  # blocks on portal.nousresearch.com). The router accepts both
  # /v1/chat/completions and /v1/responses; Azure gpt-5.x model family is
  # forced to /v1/responses by Hermes (chat/completions returns 400
  # "operation unsupported" for those models — see
  # hermes_cli/runtime_provider.py::azure_foundry_model_api_mode).
  echo "model:"
  # Model selection — read the kars runtime contract var KARS_MODEL
  # first (set by controller from InferencePolicy.modelPreference.
  # primary.deployment — see controller/src/reconciler/mod.rs:1335).
  # AZURE_OPENAI_DEPLOYMENT is the legacy env name still set in
  # router_env; honour it as a fallback so manually-crafted dev
  # overlays keep working. Last-resort default keeps the boot
  # banner sensible if nothing's configured at all.
  echo "  default: \"${KARS_MODEL:-${AZURE_OPENAI_DEPLOYMENT:-gpt-5.4}}\""
  echo "  provider: azure-foundry"
  echo "  base_url: \"http://127.0.0.1:8443/v1\""
  echo "mcp_servers:"
  # Built-in platform MCP — exposes the 9 Foundry tools when a Foundry
  # project is bound to this sandbox. Hermes' MCP client + governance
  # hook means the LLM sees foundry_* tools as MCP-namespaced tools.
  # Operators wanting native foundry_* (non-MCP) tools should use
  # OpenClaw; Hermes intentionally leans on its strong native MCP.
  if [ -n "${FOUNDRY_PROJECT_ENDPOINT:-}" ]; then
    echo "  platform:"
    echo "    url: \"http://127.0.0.1:8443/platform/mcp\""
    echo "    description: \"kars Foundry tools (memory, web_search, image_gen, code_execute, file_search, conversations, evaluations, deployments, agents)\""
  fi
  # Translate kars-published McpServer CRs. Each name resolves to the
  # loopback router with a routing header; the router does the rest.
  if [ -n "${KARS_MCP_SERVERS:-}" ]; then
    # Convert "name1,name2,name3" → space-separated for the loop.
    SERVERS=$(echo "$KARS_MCP_SERVERS" | tr ',' ' ')
    for server_name in $SERVERS; do
      server_name=$(echo "$server_name" | tr -d ' ')
      [ -z "$server_name" ] && continue
      # Skip the reserved 'platform' name to avoid colliding with the
      # built-in we register above.
      [ "$server_name" = "platform" ] && continue
      echo "  $server_name:"
      echo "    url: \"http://127.0.0.1:8443/mcp\""
      echo "    headers:"
      echo "      x-kars-mcp-server: \"$server_name\""
      echo "      x-kars-sandbox: \"$SANDBOX_NAME\""
    done
  fi
} > "$MCP_FRAGMENT"

# Merge into config.yaml. The two blocks the entrypoint owns
# (`plugins:` and `mcp_servers:`) are stripped from any existing
# config and replaced with freshly-generated versions. Everything
# else the user may have written via `hermes config set <other.key>`
# is preserved across pod restarts.
#
# We use Python rather than awk because the Azure Linux 3 minimal
# base image doesn't ship awk; sed alone can't do block-level
# selection without fragile multi-line patterns. ruamel.yaml is
# pulled in as a hermes-agent dep so this import is always available.
if [ -f "$HERMES_CONFIG" ]; then
  python3 - "$HERMES_CONFIG" "$MCP_FRAGMENT" <<'PY'
import io, re, sys
config_path, fragment_path = sys.argv[1], sys.argv[2]
src = open(config_path).read()
fragment = open(fragment_path).read()

# Strip any prior top-level `plugins:`, `mcp_servers:`, and `model:`
# blocks (the three sections the entrypoint owns) so re-runs are
# idempotent. Everything else the user wrote via `hermes config set
# <other.key>` is preserved across pod restarts.
top_key = re.compile(r"^(plugins|mcp_servers|model):", re.M)
out, idx, prev = io.StringIO(), 0, 0
while True:
    m = top_key.search(src, idx)
    if not m:
        out.write(src[prev:])
        break
    out.write(src[prev:m.start()])
    # Skip lines until the next top-level key (not indented).
    next_top = re.compile(r"^[A-Za-z_][\w-]*:", re.M)
    nm = next_top.search(src, m.end())
    prev = nm.start() if nm else len(src)
    idx = prev

merged = out.getvalue().rstrip() + "\n" + fragment.rstrip() + "\n"
open(config_path, "w").write(merged)
PY
else
  mv "$MCP_FRAGMENT" "$HERMES_CONFIG"
fi
rm -f "$MCP_FRAGMENT"

# ── Channel credential translation ──────────────────────────────────────
# `kars credentials update --telegram-token X` writes TELEGRAM_BOT_TOKEN
# into <sandbox>-credentials Secret (k8s) or docker env (dev). We
# translate each known channel env to the matching `hermes config set`
# invocation. Hermes' gateway then auto-binds the channel.
#
# Format reference: hermes uses dotted-path keys, e.g.
#   hermes config set channels.telegram.token=<value>
#   hermes config set channels.telegram.allowed_users=12345,67890
#
# Channels Hermes supports natively that map from kars envs:
#   telegram, slack, discord, whatsapp, signal, matrix, email
# Channels kars doesn't expose creds for yet:
#   mattermost, dingtalk, feishu, wecom, weixin, bluebubbles,
#   qqbot, homeassistant, webhook, api_server, yuanbao, sms
# Operators wanting those can `hermes config set` manually post-boot
# from their own creds.

set_hermes_config() {
  local key=$1
  local value=$2
  if [ -n "$value" ]; then
    # Hermes 0.15.x expects `hermes config set <key> <value>` (two
    # positional args), NOT `key=value`. See `hermes config set
    # --help`: examples include `hermes config set model
    # anthropic/claude-sonnet-4` and `hermes config set
    # terminal.backend docker`.
    $AS_SANDBOX hermes config set "$key" "$value" >/dev/null 2>&1 || true
  fi
}

if [ -n "${TELEGRAM_BOT_TOKEN:-}" ]; then
  set_hermes_config "channels.telegram.token" "$TELEGRAM_BOT_TOKEN"
  set_hermes_config "channels.telegram.enabled" "true"
fi
if [ -n "${TELEGRAM_ALLOW_FROM:-}" ]; then
  set_hermes_config "channels.telegram.allowed_users" "$TELEGRAM_ALLOW_FROM"
fi
if [ -n "${SLACK_BOT_TOKEN:-}" ]; then
  set_hermes_config "channels.slack.token" "$SLACK_BOT_TOKEN"
  set_hermes_config "channels.slack.enabled" "true"
fi
if [ -n "${DISCORD_BOT_TOKEN:-}" ]; then
  set_hermes_config "channels.discord.token" "$DISCORD_BOT_TOKEN"
  set_hermes_config "channels.discord.enabled" "true"
fi
if [ "${WHATSAPP_ENABLED:-}" = "true" ]; then
  set_hermes_config "channels.whatsapp.enabled" "true"
fi

# Third-party plugin API keys (Hermes uses these to register its own
# tool plugins). When the env is present, Hermes auto-enables the
# matching plugin on next start.
if [ -n "${BRAVE_API_KEY:-}" ]; then
  set_hermes_config "tools.brave.api_key" "$BRAVE_API_KEY"
fi
if [ -n "${TAVILY_API_KEY:-}" ]; then
  set_hermes_config "tools.tavily.api_key" "$TAVILY_API_KEY"
fi
if [ -n "${EXA_API_KEY:-}" ]; then
  set_hermes_config "tools.exa.api_key" "$EXA_API_KEY"
fi
if [ -n "${FIRECRAWL_API_KEY:-}" ]; then
  set_hermes_config "tools.firecrawl.api_key" "$FIRECRAWL_API_KEY"
fi
if [ -n "${PERPLEXITY_API_KEY:-}" ]; then
  set_hermes_config "tools.perplexity.api_key" "$PERPLEXITY_API_KEY"
fi

# ── Egress guard: iptables in docker dev only ────────────────────────────
# In AKS / local-k8s the init container egress-guard handles this. In
# docker dev we're root briefly and add the rules ourselves.
if [ "$IS_ROOT" = "true" ] && command -v iptables >/dev/null 2>&1; then
  iptables -N KARS_EGRESS 2>/dev/null || true
  iptables -F KARS_EGRESS 2>/dev/null || true
  # Allow established, localhost, DNS from UID 1000
  iptables -A KARS_EGRESS -m state --state ESTABLISHED,RELATED -j ACCEPT 2>/dev/null || true
  iptables -A KARS_EGRESS -o lo -j ACCEPT 2>/dev/null || true
  iptables -A KARS_EGRESS -p udp --dport 53 -j ACCEPT 2>/dev/null || true
  iptables -A KARS_EGRESS -p tcp --dport 53 -j ACCEPT 2>/dev/null || true
  iptables -A KARS_EGRESS -j REJECT 2>/dev/null || true
  iptables -A OUTPUT -m owner --uid-owner 1000 -j KARS_EGRESS 2>/dev/null || true
fi

# ── Workload Identity → Entra token exchange (opt-in) ────────────────────
# Mirrors sandbox-images/openclaw/entrypoint.sh § "Workload Identity →
# Entra token exchange". Without this block the Hermes plugin's
# /agt/registry/v1/registry/verify call (in mesh.py) has no token to
# present and the sandbox stays on the Anonymous tier in the operator
# panel — even though the underlying Entra Agent ID is provisioned and
# the workload identity binding is correct.
#
# Two paths (priority preserved from OpenClaw):
#   1. MESH_AUTH_BACKEND=EntraAgentIdentity → ask the router's
#      /v1/mesh-token endpoint for a sidecar-mediated mint (Phase 6.b).
#   2. AGT_SKIP_ENTRA=1 → operator kill switch for dev clusters that
#      don't have api://agentmesh provisioned. Skip the whole block.
#   3. AZURE_FEDERATED_TOKEN_FILE set (default AKS WI path) → direct
#      token exchange via the loopback forward proxy.
#
# Same fail-open trust-threshold rule as OpenClaw: when Entra fails,
# AGT_TRUST_THRESHOLD is forced to 0 so anonymous-tier KNOCKs still go
# through (the AGT SDK's X3DH proves cryptographic identity regardless).
if [ "${MESH_AUTH_BACKEND:-}" = "EntraAgentIdentity" ] && [ -z "${AGT_OAUTH_TOKEN:-}" ]; then
  echo "[kars-hermes] MESH_AUTH_BACKEND=EntraAgentIdentity — acquiring mesh token via /v1/mesh-token"
  _ROUTER_URL="${ROUTER_LOCAL_URL:-http://127.0.0.1:8443}"
  _ACCESS_TOKEN=""
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
        echo "[kars-hermes] Mesh token acquired via auth-sidecar after ${_ATTEMPT} attempt(s) (${_ELAPSED}s) — verified-tier registration"
        export AGT_OAUTH_TOKEN="$_ACCESS_TOKEN"
        break
      fi
    fi
    if [ "$_MESH_STATUS" = "404" ]; then
      echo "[kars-hermes] /v1/mesh-token returned 404 — router image too old for Phase 6.b"
      break
    fi
    sleep "$_DELAY"
    _ELAPSED=$((_ELAPSED + _DELAY))
    if [ "$_DELAY" -lt 4 ]; then _DELAY=$((_DELAY * 2)); fi
  done
  if [ -z "${AGT_OAUTH_TOKEN:-}" ]; then
    echo "[kars-hermes] /v1/mesh-token failed after ${_ELAPSED}s (${_ATTEMPT} attempts, last status=${_MESH_STATUS:-network-error}); registering as anonymous tier"
    export AGT_TRUST_THRESHOLD=0
  fi
  unset _MESH_RESP _MESH_STATUS _MESH_BODY _ROUTER_URL _ACCESS_TOKEN _DELAY _ELAPSED _MAX_WAIT _ATTEMPT
elif [ "${AGT_SKIP_ENTRA:-0}" = "1" ]; then
  echo "[kars-hermes] AGT_SKIP_ENTRA=1 — Entra token exchange disabled, registering as anonymous tier"
  if [ -n "${AGT_TRUST_THRESHOLD:-}" ] && [ "${AGT_TRUST_THRESHOLD}" != "0" ]; then
    echo "[kars-hermes] AGT_SKIP_ENTRA=1 overrides AGT_TRUST_THRESHOLD=${AGT_TRUST_THRESHOLD} → 0 (anonymous-tier fail-open)"
  fi
  export AGT_TRUST_THRESHOLD=0
elif [ -n "${AZURE_FEDERATED_TOKEN_FILE:-}" ] && [ -f "${AZURE_FEDERATED_TOKEN_FILE}" ] && \
   [ -n "${AZURE_CLIENT_ID:-}" ] && [ -n "${AZURE_TENANT_ID:-}" ] && \
   [ -z "${AGT_OAUTH_TOKEN:-}" ]; then
  echo "[kars-hermes] Exchanging Workload Identity token for Entra ID access token..."
  _ACCESS_TOKEN=""
  _DELAY=1
  _ELAPSED=0
  _MAX_WAIT="${ENTRA_TOKEN_MAX_WAIT:-120}"
  _ATTEMPT=0
  while [ "$_ELAPSED" -lt "$_MAX_WAIT" ]; do
    _ATTEMPT=$((_ATTEMPT + 1))
    _FED_TOKEN=$(cat "$AZURE_FEDERATED_TOKEN_FILE")
    # UID 1000 is blocked from direct egress; route via the router's
    # loopback forward proxy on 127.0.0.1:8444.
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
      echo "[kars-hermes] Entra ID token acquired after ${_ATTEMPT} attempt(s) (${_ELAPSED}s) — Hermes will register as verified tier"
      break
    fi
    if echo "$_TOKEN_RESP" | grep -q "AADSTS500011"; then
      echo "[kars-hermes] Entra: api://agentmesh SP not provisioned in tenant — skipping retries, registering as anonymous tier"
      break
    fi
    sleep "$_DELAY"
    _ELAPSED=$((_ELAPSED + _DELAY))
    if [ "$_DELAY" -lt 4 ]; then _DELAY=$((_DELAY * 2)); fi
  done
  if [ -n "$_ACCESS_TOKEN" ]; then
    export AGT_OAUTH_TOKEN="$_ACCESS_TOKEN"
  else
    echo "[kars-hermes] Entra token exchange failed after ${_ELAPSED}s (${_ATTEMPT} attempts) — Hermes will register as anonymous tier"
    if [ -n "${AGT_TRUST_THRESHOLD:-}" ] && [ "${AGT_TRUST_THRESHOLD}" != "0" ]; then
      echo "[kars-hermes] Entra exchange failed: overriding AGT_TRUST_THRESHOLD=${AGT_TRUST_THRESHOLD} → 0 (anonymous-tier fail-open)"
    fi
    export AGT_TRUST_THRESHOLD=0
  fi
  unset _FED_TOKEN _TOKEN_RESP _ACCESS_TOKEN _DELAY _ELAPSED _MAX_WAIT _ATTEMPT
fi

# ── Contract version assertion ───────────────────────────────────────────
# Per docs/runtimes/CONTRACT.md, runtimes MUST error loudly on unknown
# contract version. Controller injects KARS_RUNTIME_CONTRACT_VERSION=v1
# (A1.2 will make this generic; for now it may be unset on older
# controllers — treat unset as v1 for back-compat).
case "${KARS_RUNTIME_CONTRACT_VERSION:-v1}" in
  v1) ;;
  *)
    echo "[kars-hermes] ERROR: unsupported runtime contract version: $KARS_RUNTIME_CONTRACT_VERSION" >&2
    echo "[kars-hermes] This Hermes image supports v1 only." >&2
    exit 1
    ;;
esac

# Hermes auto-bootstraps several heavy deps on first run (tirith
# binary, discord.py, Node.js for browser tools) by downloading
# from GitHub / PyPI / nodejs.org. Inside a kars sandbox each of
# these either (a) blocks at startup waiting on an egress that the
# NetworkPolicy + iptables egress-guard slow-walks, or (b) takes
# 30–120s on cold start. We disable the lot by default — operators
# who want any of them can re-bake the binaries into a custom
# sandbox image. The env vars are also mirrored into
# `$HERMES_HOME/.env` so they survive `kubectl exec` sessions
# (kubectl exec spawns a fresh env that doesn't see entrypoint
# exports).
export TIRITH_ENABLED="${TIRITH_ENABLED:-false}"
export HERMES_DISABLE_LAZY_INSTALLS="${HERMES_DISABLE_LAZY_INSTALLS:-1}"
export HERMES_SKIP_NODE_BOOTSTRAP="${HERMES_SKIP_NODE_BOOTSTRAP:-1}"

# Mirror the kars-managed env into $HERMES_HOME/.env so any
# follow-up `hermes` invocation (kubectl exec, cron, gateway
# subprocess) picks up the same posture as PID 1.
#
# Provider selection: Hermes' resolve_provider() priority (see
# hermes_cli/auth.py:1481) checks `OPENAI_API_KEY` BEFORE the
# generic api-key auto-detect loop, and routes any value there
# straight to the `openrouter` provider — which then phones home
# to openrouter.ai on every chat call. We instead set
# AZURE_FOUNDRY_API_KEY + AZURE_FOUNDRY_BASE_URL so the
# `azure-foundry` provider (plugins/model-providers/azure-foundry/)
# is auto-selected by the loop below the OPENAI check, and the
# router becomes the only outbound destination.
mkdir -p "$HERMES_HOME"
cat > "$HERMES_HOME/.env" <<EOF
# Auto-generated by kars-hermes-entrypoint. Re-emitted on every boot.
TIRITH_ENABLED=$TIRITH_ENABLED
HERMES_DISABLE_LAZY_INSTALLS=$HERMES_DISABLE_LAZY_INSTALLS
HERMES_SKIP_NODE_BOOTSTRAP=$HERMES_SKIP_NODE_BOOTSTRAP
HERMES_PROFILE=$HERMES_PROFILE
SANDBOX_NAME=$SANDBOX_NAME
# Inference routing: pin the azure-foundry provider so every chat
# call lands on the loopback router (which authenticates upstream
# via the controller-injected real API key from kars-dev-creds).
# DO NOT also set OPENAI_API_KEY — that would short-circuit
# resolve_provider() to the openrouter path which calls
# openrouter.ai (Cloudflare-blocked by the egress-guard).
AZURE_FOUNDRY_API_KEY=router-managed
AZURE_FOUNDRY_BASE_URL=${OPENAI_BASE_URL}
EOF

# ── Boot banner ──────────────────────────────────────────────────────────
echo "═══════════════════════════════════════════════════════════════════"
echo "  kars-hermes-entrypoint  (contract v1)"
echo "═══════════════════════════════════════════════════════════════════"
echo "  sandbox        : $SANDBOX_NAME"
echo "  hermes home    : $HERMES_HOME"
echo "  hermes profile : $HERMES_PROFILE"
echo "  provider       : ${KARS_PROVIDER:-azure-openai}"
echo "  router         : $OPENAI_BASE_URL"
echo "  mesh relay     : ${AGT_RELAY_URL:-(none — single-agent mode)}"
echo "  governance     : ${AGT_GOVERNANCE_ENABLED:-false}"
echo "  dev profile    : ${KARS_DEV_PROFILE:-false}"
echo "═══════════════════════════════════════════════════════════════════"

# ── Exec hermes ──────────────────────────────────────────────────────────
# K8s sandbox pods have no TTY, so the interactive `hermes` chat mode
# would just exit immediately. We always boot `hermes gateway run`:
#
#   * with channels configured (TELEGRAM/SLACK/DISCORD), the gateway
#     dispatches inbound messages to the agent;
#   * with no channels yet, the gateway still stays alive as a daemon
#     waiting for kars sub-agent spawn / mesh messages / future
#     channel reconfiguration — i.e. the pod stays Running 2/2 even
#     without an external trigger source.
#
# `gateway run` is the Docker/headless-recommended mode per
# `hermes gateway --help`. The `start --foreground` variant we used
# previously installs a systemd/launchd unit first, which fails in
# K8s pods (no init system).
#
# CMD from Dockerfile is `hermes` by default. Operators override with
# `docker run … kars-sandbox-hermes:dev python /sandbox/agent/main.py`
# to run user-supplied agent code instead.
if [ "$1" = "hermes" ]; then
  WANT_GATEWAY=false
  for t in TELEGRAM_BOT_TOKEN SLACK_BOT_TOKEN DISCORD_BOT_TOKEN; do
    if [ -n "${!t:-}" ]; then
      WANT_GATEWAY=true
      break
    fi
  done
  if [ "$WANT_GATEWAY" = "true" ]; then
    echo "[kars-hermes] Channels detected — starting hermes gateway (foreground)"
  else
    echo "[kars-hermes] No channels — starting hermes gateway in idle daemon mode"
  fi
  exec $AS_SANDBOX hermes gateway run --accept-hooks
else
  echo "[kars-hermes] Operator override: $*"
  exec $AS_SANDBOX "$@"
fi
