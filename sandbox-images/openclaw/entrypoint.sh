#!/bin/bash
# AzureClaw sandbox entrypoint
# Configures OpenClaw automatically from mounted secrets and env vars.
# The user never needs to manually configure anything.

set -e

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

# Generate gateway token early (needed for config file + .bashrc)
GATEWAY_TOKEN=$(head -c 32 /dev/urandom | base64 | tr -dc 'a-zA-Z0-9' | head -c 32)
export OPENCLAW_GATEWAY_TOKEN="$GATEWAY_TOKEN"

# Only configure if not already done (idempotent)
if [ ! -f "$OPENCLAW_CONFIG" ]; then
  # Create OpenClaw directories
  mkdir -p "$OPENCLAW_DIR" "$WORKSPACE_DIR"

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
      "dangerouslyDisableDeviceAuth": true
    }
  }
}
EOF
  chmod 600 "$OPENCLAW_CONFIG"

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
- You have access to bash, file operations, and git inside the sandbox
- Your workspace is /sandbox — all your files live here
- Your network access is governed by policy — unauthorized endpoints will be blocked

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

  # Install AzureClaw plugin into OpenClaw's extensions directory
  if [ -d /opt/azureclaw-plugin ]; then
    mkdir -p "$OPENCLAW_DIR/extensions/azureclaw/dist"
    cp /opt/azureclaw-plugin/package.json "$OPENCLAW_DIR/extensions/azureclaw/"
    cp /opt/azureclaw-plugin/openclaw.plugin.json "$OPENCLAW_DIR/extensions/azureclaw/"
    # Copy all built JS/TS files preserving directory structure
    cd /opt/azureclaw-plugin && find . -name '*.js' -o -name '*.d.ts' -o -name '*.map' | while read f; do
      mkdir -p "$OPENCLAW_DIR/extensions/azureclaw/dist/$(dirname "$f")"
      cp "$f" "$OPENCLAW_DIR/extensions/azureclaw/dist/$f"
    done
    # Copy Foundry skills (SKILL.md files)
    if [ -d /opt/azureclaw-plugin/skills ]; then
      cp -r /opt/azureclaw-plugin/skills "$OPENCLAW_DIR/extensions/azureclaw/"
      echo "[azureclaw] Foundry + governance skills installed"
    fi
    # Copy node_modules for AGT SDK (@agentmesh/sdk)
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
else
  # Load credentials for existing config
  export AZURE_OPENAI_API_KEY="${API_KEY}"
  export AZURE_OPENAI_ENDPOINT="${ENDPOINT}"
  echo "[azureclaw] OpenClaw already configured"
fi

# Add gateway token to .bashrc so interactive shells have it
cat >> /sandbox/.bashrc << RCEOF2

# AzureClaw: Gateway auth
export OPENCLAW_GATEWAY_TOKEN="${GATEWAY_TOKEN}"
RCEOF2

# Start AzureClaw inference router — only in dev mode (no sidecar).
# In AKS, the controller deploys the router as a separate sidecar container.
if [ "${AZURECLAW_AUTH_MODE:-}" != "workload-identity" ]; then
  ROUTER_PORT=8443 \
  AZURE_OPENAI_ENDPOINT="$ENDPOINT" \
  DEFAULT_MODEL="$MODEL" \
  CONTENT_SAFETY_ENABLED=true \
  azureclaw-inference-router > /tmp/inference-router.log 2>&1 &
  ROUTER_PID=$!
  sleep 1
  echo "[azureclaw] Inference router running (PID: $ROUTER_PID, port: 8443)"
else
  echo "[azureclaw] Inference router provided by sidecar (workload-identity mode)"
fi

# Start OpenClaw gateway in the background (needed for TUI)
OPENCLAW_GATEWAY_TOKEN="$GATEWAY_TOKEN" openclaw gateway --port 18789 > /tmp/gateway.log 2>&1 &
GATEWAY_PID=$!

# Wait for gateway to be ready
for i in $(seq 1 10); do
  if curl -sf http://127.0.0.1:18789/healthz > /dev/null 2>&1; then
    echo "[azureclaw] Gateway running (PID: $GATEWAY_PID)"
    break
  fi
  sleep 1
done

# Keep the container alive — don't use exec (it would kill the gateway)
# Instead, wait forever while keeping the gateway backgrounded
tail -f /dev/null
