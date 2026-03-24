#!/bin/bash
# AzureClaw sandbox entrypoint
# Configures OpenClaw automatically from mounted secrets and env vars.
# The user never needs to manually configure anything.
#
# UID model (mirrors AKS sidecar architecture):
#   UID 1001 (router)  — inference router, can reach internet
#   UID 1000 (sandbox) — agent processes, restricted to localhost + DNS

set -e

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

# ── Egress guard: iptables restricts UID 1000 to localhost + DNS ────────────
# Only applies when running as root (dev mode). On AKS, the egress-guard init
# container handles this before the sandbox container starts.
if [ "$IS_ROOT" = "true" ] && command -v iptables >/dev/null 2>&1; then
  iptables -N AZURECLAW_EGRESS 2>/dev/null || true
  iptables -A AZURECLAW_EGRESS -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  iptables -A AZURECLAW_EGRESS -o lo -j ACCEPT
  iptables -A AZURECLAW_EGRESS -p udp --dport 53 -j ACCEPT
  iptables -A AZURECLAW_EGRESS -p tcp --dport 53 -j ACCEPT
  iptables -A AZURECLAW_EGRESS -j REJECT --reject-with icmp-port-unreachable
  iptables -A OUTPUT -m owner --uid-owner 1000 -j AZURECLAW_EGRESS
  echo "[azureclaw] iptables egress guard active (UID 1000 → localhost + DNS only)"
fi

OPENCLAW_DIR="/sandbox/.openclaw"
OPENCLAW_CONFIG="$OPENCLAW_DIR/openclaw.json"
WORKSPACE_DIR="/sandbox/.openclaw/workspace"

# Read API key from mounted secret
API_KEY=""
if [ -f /run/secrets/azure-openai-key ]; then
  API_KEY=$(cat /run/secrets/azure-openai-key)
fi

# Get config from env vars (set by azureclaw dev/up)
ENDPOINT="${AZURE_OPENAI_ENDPOINT:-}"
MODEL="${OPENCLAW_MODEL:-gpt-4.1}"

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
        "models": [
          { "id": "${MODEL}", "name": "${MODEL} (Azure via AzureClaw)" }
        ]
      }
    }
  },
  "tools": {
    "deny": ["sessions_spawn", "sessions_send"]
  },
  "plugins": {
    "allow": ["azureclaw"]
  },
  "channels": {
    "telegram": {
      "enabled": TELEGRAM_ENABLED_PLACEHOLDER,
      "botToken": "${TELEGRAM_BOT_TOKEN:-}",
      "dmPolicy": "open",
      "allowFrom": ["*"]
    }
  },
  "agents": {
    "defaults": {
      "model": { "primary": "azure-openai/${MODEL}" }
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

  # Set Telegram enabled based on whether token is provided
  if [ -n "${TELEGRAM_BOT_TOKEN:-}" ]; then
    sed -i 's/TELEGRAM_ENABLED_PLACEHOLDER/true/' "$OPENCLAW_CONFIG"
  else
    sed -i 's/TELEGRAM_ENABLED_PLACEHOLDER/false/' "$OPENCLAW_CONFIG"
  fi

  # Set provider credentials via environment (OpenClaw reads these automatically)
  # These are exported so openclaw tui/agent picks them up
  export AZURE_OPENAI_API_KEY="${API_KEY}"
  export AZURE_OPENAI_ENDPOINT="${ENDPOINT}"

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
  cat > "$WORKSPACE_DIR/AGENTS.md" << 'EOF'
# AzureClaw Agent

You are a helpful AI assistant running inside an AzureClaw sandbox on Azure.
You are secure, sandboxed, and connected to Azure AI Foundry.

## Capabilities
- You can help with coding, analysis, writing, and general questions
- You have access to shell/exec tools for running commands inside the sandbox
- You can read files, run system commands (uname, hostname, cat /etc/os-release, etc.)
- Your workspace is /sandbox — all your files live here
- Your network access is governed by policy — unauthorized endpoints will be blocked

## Inter-Agent Communication (IMPORTANT)
You can spawn sub-agents and communicate with them via E2E encrypted messaging.
**You MUST use these tools for inter-agent communication — never fabricate sub-agent responses.**

### Workflow for sub-agent tasks:
1. **Spawn**: Call `azureclaw_spawn` with a name — it returns when the sub-agent is Running
2. **Send**: Call `azureclaw_mesh_send` with `to_agent` and `content` — this sends via AGT relay with Signal Protocol E2E encryption
3. **Wait & Read**: Call `azureclaw_mesh_inbox` to check for replies — retry a few times with short pauses if empty (the sub-agent needs time to process)
4. **Destroy**: Call `azureclaw_spawn_destroy` when done — this tears down the sub-agent completely

### Rules:
- NEVER generate or invent a sub-agent's response — always read it from `azureclaw_mesh_inbox`
- If `azureclaw_mesh_inbox` returns no messages, wait and retry (up to 60 seconds)
- All messages between agents are E2E encrypted (Signal Protocol) — the relay cannot read them
- Sub-agents auto-process task_request messages and send replies back via the mesh

## Azure AI Foundry capabilities (via AzureClaw inference router)
- **200+ AI models** — inference through Foundry model catalog (GPT-4.1, DeepSeek, Phi-4, Llama, etc.)
- **Persistent memory** — Foundry threads survive pod restarts (use the foundry-memory skill)
- **Knowledge search** — upload documents and search them with vector similarity (foundry-knowledge skill)
- **Web search** — real-time web grounding with citations, no egress needed (foundry-web-search skill)
- **Code interpreter** — Python execution for data analysis and charts (foundry-code skill)

All Foundry services are accessed through http://localhost:8443. Authentication is automatic (IMDS).
You never need API keys.

## Security context
- You are running as a non-root user (sandbox:1000)
- The root filesystem is read-only
- You can write to /sandbox and /tmp only
- All system calls are filtered by seccomp
- Your inference calls go through Content Safety + Prompt Shields
- Token budgets are enforced per sandbox
EOF

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

  cat > "$WORKSPACE_DIR/SOUL.md" << 'EOF'
# Soul

You are **AzureClaw Agent** — a secure, sandboxed AI assistant powered by Azure.

You are helpful, concise, and technically competent. You run inside an isolated
container on Azure Linux, with your inference routed through Azure OpenAI.

When asked about yourself, you can mention that you're running inside AzureClaw —
an open-source secure runtime for AI agents on Azure.

You are friendly but professional. You get things done.
EOF

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
  # Copy node_modules for AGT SDK (@agentmesh/sdk) and other runtime deps
  if [ -d /opt/azureclaw-plugin/node_modules ]; then
    cp -r /opt/azureclaw-plugin/node_modules "$OPENCLAW_DIR/extensions/azureclaw/"
    echo "[azureclaw] AGT SDK (@agentmesh/sdk) available"
  fi
  # Copy AGT policies if governance enabled
  if [ "${AGT_GOVERNANCE_ENABLED:-}" = "true" ] && [ -d /opt/azureclaw-plugin/policies ]; then
    mkdir -p "$OPENCLAW_DIR/policies"
    cp /opt/azureclaw-plugin/policies/*.yaml "$OPENCLAW_DIR/policies/" 2>/dev/null || true
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
  # Ensure router can write its log file
  touch /tmp/inference-router.log
  [ "$IS_ROOT" = "true" ] && chown 1001:1001 /tmp/inference-router.log
  ROUTER_PORT=8443 \
  AZURE_OPENAI_ENDPOINT="$ENDPOINT" \
  AZURE_OPENAI_API_KEY="$API_KEY" \
  DEFAULT_MODEL="$MODEL" \
  CONTENT_SAFETY_ENABLED=true \
  $AS_ROUTER azureclaw-inference-router > /tmp/inference-router.log 2>&1 &
  ROUTER_PID=$!
  sleep 1
  echo "[azureclaw] Inference router running (PID: $ROUTER_PID, port: 8443)"
else
  echo "[azureclaw] Inference router provided by sidecar (workload-identity mode)"
fi

# Start OpenClaw gateway in the background (needed for TUI)
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
mkdir -p /tmp/node-host-home
[ "$IS_ROOT" = "true" ] && chown sandbox:sandbox /tmp/node-host-home
HOME=/tmp/node-host-home OPENCLAW_GATEWAY_TOKEN="$GATEWAY_TOKEN" $AS_SANDBOX openclaw node run \
  --host 127.0.0.1 --port 18789 \
  --node-id "node-${NODE_HOSTNAME}" > /tmp/node-host.log 2>&1 &
NODE_PID=$!
echo "[azureclaw] Node host starting (PID: $NODE_PID)"

# Auto-approve all exec requests — no manual approval in headless sandbox.
# The agent is already constrained by seccomp, read-only rootfs, and non-root UID.
$AS_SANDBOX openclaw approvals set --stdin <<'APPROVALS' > /dev/null 2>&1 || true
{ "mode": "auto-approve" }
APPROVALS

# Start a persistent background agent session that loads the AzureClaw plugin.
# This keeps the AGT relay connection alive so the agent can receive E2E encrypted
# messages from other agents in the mesh. Without this, the plugin only loads during
# on-demand sessions and misses relay messages.
(
  sleep 5  # Wait for gateway to stabilize
  $AS_SANDBOX openclaw agent --local \
    --session-id "agt-relay-listener-${NODE_HOSTNAME}" \
    --message "You are an AGT relay listener. Stay connected and respond to any relay messages with AGT RELAY CONFIRMED." \
    > /tmp/agt-relay-listener.log 2>&1 || true
) &
AGT_LISTENER_PID=$!
echo "[azureclaw] AGT relay listener starting (PID: $AGT_LISTENER_PID)"

# Keep the container alive — don't use exec (it would kill the gateway)
# Instead, wait forever while keeping the gateway + AGT listener backgrounded
tail -f /dev/null
