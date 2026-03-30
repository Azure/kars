#!/bin/bash
# AzureClaw sandbox entrypoint
# Configures OpenClaw automatically from mounted secrets and env vars.
# The user never needs to manually configure anything.
#
# UID model (mirrors AKS sidecar architecture):
#   UID 1001 (router)  — inference router, can reach internet
#   UID 1000 (sandbox) — agent processes, restricted to localhost + DNS

set -e

# Raise FD limit — the inference router and gateway share this process namespace.
# Default 1024 is too low for long-running containers with many HTTP connections.
ulimit -n 65536 2>/dev/null || true

# Detect if running as root (dev mode via Docker) or non-root (AKS pod spec override).
# In dev: Dockerfile has no USER directive, entrypoint runs as root, uses runuser to
#   start router as UID 1001 and everything else as UID 1000.
# In AKS: pod spec sets runAsUser:1000, so entrypoint is already UID 1000. Router runs
#   as a separate sidecar (UID 1001). No runuser needed.
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
mkdir -p "$_oc_tmpdir" && chmod 700 "$_oc_tmpdir"
if [ "$IS_ROOT" = "true" ]; then
  # Dev mode: also create dirs for sandbox (1000) and router (1001)
  for _uid in 1000 1001; do
    _dir="/tmp/openclaw-${_uid}"
    mkdir -p "$_dir" && chown "${_uid}:${_uid}" "$_dir" && chmod 700 "$_dir"
  done
fi

# ── Egress guard: iptables restricts UID 1000 to localhost + DNS ────────────
# Only applies when running as root (dev mode). On AKS, the egress-guard init
# container handles this before the sandbox container starts.
if [ "$IS_ROOT" = "true" ] && command -v iptables >/dev/null 2>&1; then
  # Filter table: allow established, localhost, DNS — reject everything else
  # Flush before append to prevent duplicate rules on container restart (#13)
  iptables -N AZURECLAW_EGRESS 2>/dev/null || iptables -F AZURECLAW_EGRESS
  iptables -A AZURECLAW_EGRESS -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  iptables -A AZURECLAW_EGRESS -o lo -j ACCEPT
  iptables -A AZURECLAW_EGRESS -p udp --dport 53 -j ACCEPT
  iptables -A AZURECLAW_EGRESS -p tcp --dport 53 -j ACCEPT
  # Allow traffic to the forward proxy port (redirected packets go to localhost)
  iptables -A AZURECLAW_EGRESS -p tcp --dport 8444 -j ACCEPT
  iptables -A AZURECLAW_EGRESS -j REJECT --reject-with icmp-port-unreachable
  # Remove stale jump rule before adding (idempotent)
  iptables -D OUTPUT -m owner --uid-owner 1000 -j AZURECLAW_EGRESS 2>/dev/null || true
  iptables -A OUTPUT -m owner --uid-owner 1000 -j AZURECLAW_EGRESS

  # NAT table: redirect HTTP/HTTPS from UID 1000 to the transparent forward proxy.
  # The proxy enforces blocklist, allowlist, and learn mode on every request.
  # Inference (localhost:8443) is unaffected — loopback traffic is ACCEPTed above.
  iptables -t nat -N AZURECLAW_REDIRECT 2>/dev/null || iptables -t nat -F AZURECLAW_REDIRECT
  iptables -t nat -A AZURECLAW_REDIRECT -p tcp --dport 80  -j REDIRECT --to-port 8444
  iptables -t nat -A AZURECLAW_REDIRECT -p tcp --dport 443 -j REDIRECT --to-port 8444
  iptables -t nat -D OUTPUT -m owner --uid-owner 1000 ! -o lo -j AZURECLAW_REDIRECT 2>/dev/null || true
  iptables -t nat -A OUTPUT -m owner --uid-owner 1000 ! -o lo -j AZURECLAW_REDIRECT

  echo "[azureclaw] iptables egress guard active (UID 1000 → transparent proxy on :8444)"
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
fi

# Get config from env vars (set by azureclaw dev/up)
ENDPOINT="${AZURE_OPENAI_ENDPOINT:-}"
MODEL="${OPENCLAW_MODEL:-gpt-4.1}"

# Build models list from FOUNDRY_DEPLOYMENTS (JSON array) or fall back to single MODEL
MODELS_JSON="[{\"id\":\"${MODEL}\",\"name\":\"${MODEL} (Azure via AzureClaw)\"}]"
if [ -n "${FOUNDRY_DEPLOYMENTS:-}" ]; then
  # Parse deployment names and build models array for openclaw.json
  _PARSED=$(echo "$FOUNDRY_DEPLOYMENTS" | python3 -c "
import sys, json
try:
    deps = json.load(sys.stdin)
    models = []
    for d in deps:
        name = d.get('name') or d.get('id') or ''
        if name and 'embedding' not in name.lower():
            models.append({'id': name, 'name': f'{name} (Azure via AzureClaw)'})
    if not models:
        models = [{'id': '${MODEL}', 'name': '${MODEL} (Azure via AzureClaw)'}]
    print(json.dumps(models))
except:
    print('[{\"id\":\"${MODEL}\",\"name\":\"${MODEL} (Azure via AzureClaw)\"}]')
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

# Only configure if not already done (idempotent)
if [ ! -f "$OPENCLAW_CONFIG" ]; then
  # Create OpenClaw directories (owned by sandbox user)
  mkdir -p "$OPENCLAW_DIR" "$WORKSPACE_DIR"
  [ "$IS_ROOT" = "true" ] && chown -R sandbox:sandbox "$OPENCLAW_DIR"

  # Write openclaw.json (2026.3.x config format — routed through inference router)
  cat > "$OPENCLAW_CONFIG" << EOF
{
  "models": {
    "providers": {
      "azure-openai": {
        "baseUrl": "http://127.0.0.1:8443/v1",
        "apiKey": "routed-via-inference-router",
        "api": "openai-completions",
        "authHeader": false,
        "headers": { "x-azureclaw-sandbox": "${HOSTNAME:-dev-agent}" },
        "models": ${MODELS_JSON}
      }
    }
  },
  "tools": {
    "deny": ["sessions_spawn", "sessions_send"]
  },
  "plugins": {
    "allow": [PLUGINS_ALLOW_PLACEHOLDER],
    "entries": {PLUGINS_ENTRIES_PLACEHOLDER}
  },
  "channels": {
    CHANNELS_PLACEHOLDER
  },
  "agents": {
    "defaults": {
      "model": { "primary": "azure-openai/${MODEL}" },
      "timeoutSeconds": 300,
      "memorySearch": {
        "enabled": true,
        "provider": "openai",
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
    "controlUi": {
      "enabled": true,
      "dangerouslyDisableDeviceAuth": true
    }
  }
}
EOF
  chmod 600 "$OPENCLAW_CONFIG"

  # Build channels config dynamically from env vars.
  # Built-in channel extensions need BOTH:
  #   1. A channel block in channels.* with credentials
  #   2. An entry in plugins.allow so the gateway loads the extension
  PLUGINS_LIST='"azureclaw"'
  PLUGINS_ENTRIES=""
  CHANNELS_CONFIG=""

  # Telegram (built into OpenClaw core, uses grammY)
  # Set proxy explicitly so OpenClaw uses a simple ProxyAgent for Telegram API calls.
  # The EnvHttpProxyAgent + autoSelectFamily path causes long-poll stalls in Docker.
  if [ -n "${TELEGRAM_BOT_TOKEN:-}" ]; then
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
    PLUGINS_ENTRIES="\"telegram\": { \"enabled\": true }"
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
  # These are exported so openclaw tui/agent picks them up
  export AZURE_OPENAI_API_KEY="${API_KEY}"
  export AZURE_OPENAI_ENDPOINT="${ENDPOINT}"

  # AGT governance is always active in AzureClaw sandboxes (enables agt-governance skill)
  export AGT_GOVERNANCE_ENABLED=true

  # Foundry project endpoint (for standalone APIs: Memory Store, Foundry IQ, etc.)
  FOUNDRY_PROJECT_ENDPOINT="${FOUNDRY_PROJECT_ENDPOINT:-}"
  # Foundry Agent ID (only needed for tools requiring agent runs: code_interpreter, web_search)
  FOUNDRY_AGENT_ID="${FOUNDRY_AGENT_ID:-}"

  # Write a .bashrc snippet so credentials are available in interactive shells too
  cat >> /sandbox/.bashrc << RCEOF

# AzureClaw: Azure OpenAI credentials (loaded from /run/secrets/)
export AZURE_OPENAI_API_KEY="\$(cat /run/secrets/azure-openai-key 2>/dev/null)"
export AZURE_OPENAI_ENDPOINT="${ENDPOINT}"
export OPENCLAW_MODEL="${MODEL}"
export FOUNDRY_PROJECT_ENDPOINT="${FOUNDRY_PROJECT_ENDPOINT}"
export FOUNDRY_AGENT_ID="${FOUNDRY_AGENT_ID}"
RCEOF

  # Write minimal workspace files so OpenClaw doesn't need onboarding
  cat > "$WORKSPACE_DIR/AGENTS.md" << AGENTSEOF
# AzureClaw Agent

You are a helpful AI assistant running inside an **AzureClaw** sandbox — a secure,
open-source runtime for AI agents on Azure Kubernetes Service (AKS).

## On First Message (Welcome) — MANDATORY

When a user starts a new conversation, you MUST include ALL of the following in your greeting.
Do NOT skip any of these items — they are required for security transparency:

1. **Header**: "🔒 AzureClaw Sandbox — Secure AI Runtime on Azure"
2. **Foundry Project**: Show the connected project: \`${FOUNDRY_PROJECT_ENDPOINT:-${ENDPOINT}}\`
3. **Model**: Show the active model: \`${MODEL}\`
4. **Sandbox ID**: Show the sandbox name: \`\${HOSTNAME:-dev}\`
5. **Security summary**: Mention that the environment is sandboxed (isolated container, read-only rootfs, seccomp, egress policy, Content Safety + Prompt Shields)
6. **Capabilities list**: Briefly list what you can do (code execution, web search, document search, memory, sub-agents, etc.)
7. **Invitation**: Ask how you can help

Format this nicely with the header as a bold/prominent line. The Foundry project endpoint
and model MUST be visible — this is how the user knows which backend they're connected to.
Never omit the Foundry project line even if it says "Not configured".

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
You can spawn sub-agents and communicate with them via Signal Protocol E2E encryption.
**You MUST use these tools for inter-agent communication — never fabricate sub-agent responses.**

### Workflow for sub-agent tasks:
1. **Spawn**: Call \`azureclaw_spawn\` with a name — it returns when the sub-agent is Running
2. **Send**: Call \`azureclaw_mesh_send\` with \`to_agent\` and \`content\` — encrypted via AGT relay
3. **Wait & Read**: Call \`azureclaw_mesh_inbox\` to check for replies (retry if empty)
4. **Destroy**: Call \`azureclaw_spawn_destroy\` when done

### Rules:
- NEVER generate or invent a sub-agent's response — always read it from \`azureclaw_mesh_inbox\`
- If \`azureclaw_mesh_inbox\` returns no messages, wait and retry (up to 60 seconds)
- All messages are E2E encrypted (Signal Protocol) — the relay cannot read them

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
The operator can graduate to enforcement with \`azureclaw egress <name> --enforce\`,
which promotes learned domains to the allowlist. After that, new domains require approval.
AGENTSEOF

  # Write TOOLS.md describing available Foundry endpoints
  cat > "$WORKSPACE_DIR/TOOLS.md" << 'TOOLSEOF'
# AzureClaw Tools

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
created. The operator can approve it with `azureclaw egress <name> --approve <domain>`.

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

You are **AzureClaw Agent** — a secure, sandboxed AI assistant powered by Azure AI Foundry.

You run inside an isolated, hardened container on Azure Linux. Your inference is routed
through the AzureClaw inference router which provides Content Safety, Prompt Shields,
token budgets, and egress control.

**Connected project**: ${FOUNDRY_PROJECT_ENDPOINT:-${ENDPOINT}}
**Primary model**: ${MODEL}

When greeting users for the first time, be warm and welcoming. Briefly mention you're
running in AzureClaw, what model you're using, and what you can help with. Don't be
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

  echo "[azureclaw] OpenClaw configured — model: ${MODEL}, endpoint: ${ENDPOINT}"
else
  # Load credentials for existing config
  export AZURE_OPENAI_API_KEY="${API_KEY}"
  export AZURE_OPENAI_ENDPOINT="${ENDPOINT}"
  echo "[azureclaw] OpenClaw already configured"
fi

# Always re-install AzureClaw plugin from the image (plugin code may have changed
# even though config persists on the volume). This is safe because the plugin
# directory is small and cp is idempotent.
if [ -d /opt/azureclaw-plugin ]; then
  mkdir -p "$OPENCLAW_DIR/extensions/azureclaw/dist"
  cp /opt/azureclaw-plugin/package.json "$OPENCLAW_DIR/extensions/azureclaw/"
  cp /opt/azureclaw-plugin/openclaw.plugin.json "$OPENCLAW_DIR/extensions/azureclaw/"
  # Copy built JS/TS output
  cp -r /opt/azureclaw-plugin/*.js "$OPENCLAW_DIR/extensions/azureclaw/dist/" 2>/dev/null || true
  cp -r /opt/azureclaw-plugin/*.d.ts "$OPENCLAW_DIR/extensions/azureclaw/dist/" 2>/dev/null || true
  cp -r /opt/azureclaw-plugin/*.map "$OPENCLAW_DIR/extensions/azureclaw/dist/" 2>/dev/null || true
  if [ -d /opt/azureclaw-plugin/commands ]; then
    cp -r /opt/azureclaw-plugin/commands "$OPENCLAW_DIR/extensions/azureclaw/dist/"
  fi
  # Copy Foundry skills (SKILL.md files)
  if [ -d /opt/azureclaw-plugin/skills ]; then
    cp -r /opt/azureclaw-plugin/skills "$OPENCLAW_DIR/extensions/azureclaw/"
    mkdir -p "$WORKSPACE_DIR/skills"
    cp -r /opt/azureclaw-plugin/skills/* "$WORKSPACE_DIR/skills/" 2>/dev/null || true
    echo "[azureclaw] Foundry + governance skills installed (plugin + workspace)"
  fi
  # Copy pre-installed ClawHub skills (from Docker build)
  if [ -d /opt/clawhub-skills ] && [ "$(ls -A /opt/clawhub-skills 2>/dev/null)" ]; then
    mkdir -p "$WORKSPACE_DIR/skills"
    cp -r /opt/clawhub-skills/* "$WORKSPACE_DIR/skills/" 2>/dev/null || true
    CLAWHUB_COUNT=$(ls -d /opt/clawhub-skills/*/ 2>/dev/null | wc -l)
    echo "[azureclaw] ClawHub skills installed: ${CLAWHUB_COUNT} (pre-built)"
  fi
  # Copy node_modules for AGT SDK (@agentmesh/sdk) and other runtime deps
  if [ -d /opt/azureclaw-plugin/node_modules ]; then
    cp -r /opt/azureclaw-plugin/node_modules "$OPENCLAW_DIR/extensions/azureclaw/"
    echo "[azureclaw] AGT SDK (@agentmesh/sdk) available"
  fi
  # Copy AGT policies if governance enabled
  if [ "${AGT_GOVERNANCE_ENABLED:-}" = "true" ] && [ -d /opt/azureclaw-plugin/policies ]; then
    mkdir -p "$OPENCLAW_DIR/policies"
    cp /opt/azureclaw-plugin/policies/*.yaml "$OPENCLAW_DIR/policies/" 2>/dev/null || true
    # AGT sidecar runs as UID 1002 (router) — needs read access to policies
    # Grant traverse (o+x) on parent dirs and read on policy files
    chmod o+x "$OPENCLAW_DIR"
    chmod 755 "$OPENCLAW_DIR/policies"
    chmod 644 "$OPENCLAW_DIR/policies"/*.yaml 2>/dev/null || true
    export AGT_POLICY_DIR="$OPENCLAW_DIR/policies"
    echo "[azureclaw] AGT governance enabled (policy: ${AGT_POLICY_PROFILE:-default}, trust threshold: ${AGT_TRUST_THRESHOLD:-500})"
  fi
  cd /sandbox
  echo "[azureclaw] Plugin installed → openclaw azureclaw commands available"
fi

# Write gateway token to .bashrc (remove any stale tokens from prior runs first)
sed -i '/OPENCLAW_GATEWAY_TOKEN/d' /sandbox/.bashrc
echo "export OPENCLAW_GATEWAY_TOKEN=\"${GATEWAY_TOKEN}\"" >> /sandbox/.bashrc
# Also write to a dedicated file so dev.ts can read it without parsing .bashrc
echo "${GATEWAY_TOKEN}" > /tmp/gateway-token

# Ensure all sandbox files are owned by sandbox user
[ "$IS_ROOT" = "true" ] && chown -R sandbox:sandbox /sandbox

# Start AzureClaw inference router as UID 1001 (router user) — only in dev mode.
# UID 1001 is exempt from iptables egress guard, matching the AKS sidecar model
# where the router runs in a separate container with internet access.
# In AKS, the controller deploys the router as a separate sidecar container.
if [ "${AZURECLAW_AUTH_MODE:-}" != "workload-identity" ]; then
  # ── AGT governance sidecar (dev mode) ─────────────────────────────────────
  # In AKS, the controller injects this as a separate container (UID 1002).
  # In dev mode, we start it here inside the sandbox container.
  if [ "${AGT_GOVERNANCE_ENABLED:-false}" = "true" ] && [ -f /opt/agt-governance/server.py ]; then
    rm -f /tmp/agt-governance.log
    touch /tmp/agt-governance.log
    mkdir -p /tmp/agt
    [ "$IS_ROOT" = "true" ] && chown 1002:1002 /tmp/agt-governance.log /tmp/agt
    AGT_POLICY_DIR="${AGT_POLICY_DIR:-/etc/agt/policies}" \
    POLICY_DIR="${AGT_POLICY_DIR:-/etc/agt/policies}" \
    AGT_PORT=8081 \
    AGT_METRICS_PORT=9091 \
    SANDBOX_NAME="${SANDBOX_NAME:-$HOSTNAME}" \
    AGT_TRUST_THRESHOLD="${AGT_TRUST_THRESHOLD:-500}" \
    AGT_TRUST_DB="/tmp/agt/trust_scores.json" \
    $AS_ROUTER python3 /opt/agt-governance/server.py > /tmp/agt-governance.log 2>&1 &
    AGT_PID=$!
    echo "[azureclaw] AGT governance sidecar running (PID: $AGT_PID, port: 8081)"
  fi

  # Ensure router can write its log file (remove stale file from previous runs)
  rm -f /tmp/inference-router.log
  touch /tmp/inference-router.log
  [ "$IS_ROOT" = "true" ] && chown 1001:1001 /tmp/inference-router.log
  # Dev mode: make Docker socket accessible to router (UID 1001) for sub-agent spawning
  if [ -S /var/run/docker.sock ] && [ "$IS_ROOT" = "true" ]; then
    chmod 666 /var/run/docker.sock || true
  fi
  # Generate a random admin token for the router's protected endpoints
  ROUTER_ADMIN_TOKEN=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' | head -c 64)
  ROUTER_PORT=8443 \
  ADMIN_TOKEN="$ROUTER_ADMIN_TOKEN" \
  AZURE_OPENAI_ENDPOINT="$ENDPOINT" \
  AZURE_OPENAI_API_KEY="$API_KEY" \
  DEFAULT_MODEL="$MODEL" \
  CONTENT_SAFETY_ENABLED=true \
  AGT_RELAY_URL="${AGT_RELAY_URL:-}" \
  AGT_REGISTRY_URL="${AGT_REGISTRY_URL:-}" \
  AGT_GOVERNANCE_ENABLED="${AGT_GOVERNANCE_ENABLED:-false}" \
  AGT_SIDECAR_URL="http://127.0.0.1:8081" \
  SANDBOX_NAME="${SANDBOX_NAME:-$HOSTNAME}" \
  SANDBOX_ISOLATION="${SANDBOX_ISOLATION:-enhanced}" \
  AZURECLAW_DEV_MODE="${AZURECLAW_DEV_MODE:-}" \
  DOCKER_NETWORK="${DOCKER_NETWORK:-}" \
  $AS_ROUTER azureclaw-inference-router > /tmp/inference-router.log 2>&1 &
  ROUTER_PID=$!
  sleep 1
  echo "[azureclaw] Inference router running (PID: $ROUTER_PID, port: 8443)"
else
  echo "[azureclaw] Inference router provided by sidecar (workload-identity mode)"
fi

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
    echo "[azureclaw] Gateway running (PID: $GATEWAY_PID)"
    break
  fi
  sleep 1
done

# Start the node host — provides shell/exec/filesystem tools to the agent.
# Without this, the agent only has plugin tools (AzureClaw) and no local execution.
# Give the node host its own HOME so it generates a separate device fingerprint.
# Without this, it shares the TUI's device ID and blocks TUI pairing (role conflict).
NODE_HOSTNAME=$(cat /proc/sys/kernel/hostname 2>/dev/null || echo "sandbox")
mkdir -p /tmp/node-host-home/.openclaw
[ "$IS_ROOT" = "true" ] && chown -R sandbox:sandbox /tmp/node-host-home
# Create a minimal config for the node-host — it only needs gateway connectivity,
# NOT our plugins. Using the main config causes "plugin not found: azureclaw" crash loops.
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
echo "[azureclaw] Node host starting (PID: $NODE_PID)"

# Auto-approve all exec requests — no manual approval in headless sandbox.
# The agent is already constrained by seccomp, read-only rootfs, and non-root UID.
# Use timeout to prevent hanging (the command sometimes blocks on gateway connection).
# Run in background so the entrypoint continues to the relay listener section.
(
  sleep 2  # Give gateway a moment to stabilize
  echo '{ "mode": "auto-approve" }' | AGT_SKIP_INIT=1 timeout 10 $AS_SANDBOX openclaw approvals set --stdin > /dev/null 2>&1 || true
) &

# The AGT relay listener is NOT needed as a separate process.
# The plugin running inside the gateway already handles incoming mesh messages:
#   - plugin.ts onMessage → delegateToNativeAgent → openclaw agent --message
#   - The plugin's mesh connection stays alive as long as the gateway runs.
#   - delegateToNativeAgent spawns openclaw agent sessions on the SAME gateway (no conflicts).

# Keep the container alive — don't use exec (it would kill the gateway)
# Instead, wait forever while keeping the gateway backgrounded
tail -f /dev/null
