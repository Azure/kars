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
  cp -a /opt/kars-hermes-stage/plugins/kars "$KARS_PLUGIN_DST"
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

# ── /etc/kars/mcp/<server>/meta.json → hermes mcp_servers.* config ──────
# Hermes' native MCP client reads $HERMES_HOME/config.yaml's mcp_servers
# section. The controller materializes per-McpServer ConfigMaps into
# /etc/kars/mcp/<server>/meta.json. We translate at every container start
# so config updates (new McpServer CR applied) take effect on pod restart.
#
# Additionally: when FOUNDRY_PROJECT_ENDPOINT is set, we register the
# kars-internal "platform" MCP server which exposes all 9 Foundry tools
# (foundry_code_execute, foundry_image_generation, foundry_web_search,
# foundry_file_search, foundry_memory, foundry_conversations,
# foundry_evaluations, foundry_deployments, foundry_agents). The router
# serves these at POST /platform/mcp and they're already governed by
# AGT policy + content safety + token budgets. Hermes' MCP client
# discovers them automatically.
MCP_BASE="${MCP_BASE:-/etc/kars/mcp}"
HERMES_CONFIG="$HERMES_HOME/config.yaml"
MCP_FRAGMENT="$HERMES_CONFIG.mcp-fragment"

echo "[kars-hermes] Building MCP server config in $HERMES_CONFIG"
{
  echo "# Auto-generated by kars-hermes-entrypoint at $(date -u +%FT%TZ)"
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
  # Translate kars-published McpServer CRs (controller mounts each as
  # /etc/kars/mcp/<server>/meta.json — see controller/src/mcp_server.rs).
  if [ -d "$MCP_BASE" ] && [ -n "$(ls -A "$MCP_BASE" 2>/dev/null)" ]; then
    for meta in "$MCP_BASE"/*/meta.json; do
      [ -f "$meta" ] || continue
      server_name=$(basename "$(dirname "$meta")")
      # Skip the reserved 'platform' name to avoid colliding with the
      # built-in we just registered above.
      [ "$server_name" = "platform" ] && continue
      url=$(jq -r '.url // empty' "$meta")
      bearer_env=$(jq -r '.bearerFromEnv // empty' "$meta")
      [ -z "$url" ] && continue
      echo "  $server_name:"
      echo "    url: \"$url\""
      if [ -n "$bearer_env" ]; then
        # Hermes config supports env var substitution; ${ENV_VAR} expands
        # at config-load time. We don't materialize the token here (avoids
        # leaking it into a writable file in the container).
        echo "    headers:"
        echo "      Authorization: \"Bearer \${$bearer_env}\""
      fi
      allowed=$(jq -r '.allowedTools // [] | join(",")' "$meta" 2>/dev/null || true)
      if [ -n "$allowed" ] && [ "$allowed" != "" ]; then
        echo "    allowed_tools: [$allowed]"
      fi
    done
  fi
} > "$MCP_FRAGMENT"

# Merge into config.yaml. If config.yaml exists, replace any prior
# mcp_servers block then append fresh one. Else create.
if [ -f "$HERMES_CONFIG" ]; then
  awk '/^mcp_servers:/{skip=1} /^[a-z]/{if (skip && !/^mcp_servers:/) skip=0} !skip' \
      "$HERMES_CONFIG" > "$HERMES_CONFIG.tmp"
  cat "$MCP_FRAGMENT" >> "$HERMES_CONFIG.tmp"
  mv "$HERMES_CONFIG.tmp" "$HERMES_CONFIG"
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
    $AS_SANDBOX hermes config set "$key=$value" 2>/dev/null || true
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
# If gateway tokens are present, start the gateway (background-friendly
# for messaging platforms). Else start the standard hermes CLI / TUI.
WANT_GATEWAY=false
for t in TELEGRAM_BOT_TOKEN SLACK_BOT_TOKEN DISCORD_BOT_TOKEN; do
  if [ -n "${!t:-}" ]; then
    WANT_GATEWAY=true
    break
  fi
done

# CMD from Dockerfile is `hermes` by default. Operators override with
# `docker run … kars-sandbox-hermes:dev python /sandbox/agent/main.py`
# to run user-supplied agent code instead of the TUI.
if [ "$WANT_GATEWAY" = "true" ] && [ "$1" = "hermes" ]; then
  echo "[kars-hermes] Channels detected — starting hermes gateway"
  exec $AS_SANDBOX hermes gateway start --foreground
elif [ "$1" = "hermes" ]; then
  echo "[kars-hermes] No channels — starting hermes (TUI mode)"
  exec $AS_SANDBOX hermes
else
  echo "[kars-hermes] Operator override: $*"
  exec $AS_SANDBOX "$@"
fi
