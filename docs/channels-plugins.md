# Channels & Plugins

Messaging channels and third-party plugins extend your AzureClaw agent with external communication and search capabilities. Configuration is handled via CLI flags — the sandbox entrypoint auto-configures everything from environment variables at startup.

---

## Messaging Channels

### Overview

Channels connect your agent to messaging platforms. Pass channel flags to `azureclaw dev` (local) or `azureclaw add` (AKS) and the entrypoint enables them automatically.

| Channel | Flag | Credential Flag | Notes |
|---------|------|-----------------|-------|
| Telegram | `--channels telegram` | `--telegram-token` | Token from [BotFather](https://t.me/BotFather) |
| Slack | `--channels slack` | `--slack-token` | Bot User OAuth Token (`xoxb-...`) |
| Discord | `--channels discord` | `--discord-token` | Bot token from Discord Developer Portal |
| WhatsApp | `--channels whatsapp` | — | QR code pairing at runtime (no token needed) |

Multiple channels can be enabled at once:

```bash
azureclaw dev --channels telegram,slack --telegram-token "TOKEN" --slack-token "xoxb-TOKEN"
```

### Telegram Setup

1. Message [@BotFather](https://t.me/BotFather) → `/newbot` → copy the token
2. Pass the token to the CLI:

```bash
# Local
azureclaw dev --channels telegram --telegram-token "123456:ABC-DEF..."

# AKS
azureclaw add my-agent --channels telegram --telegram-token "123456:ABC-DEF..." --learn-egress
```

### Slack Setup

1. Create a Slack App at [api.slack.com/apps](https://api.slack.com/apps)
2. Add Bot Token Scopes: `chat:write`, `app_mentions:read`, `im:history`
3. Install to workspace → copy Bot User OAuth Token

```bash
azureclaw dev --channels slack --slack-token "xoxb-..."
```

### Discord Setup

1. Create an application at [discord.com/developers](https://discord.com/developers/applications)
2. Add a Bot → copy the token
3. Invite the bot to your server with `Send Messages` and `Read Message History` permissions

```bash
azureclaw dev --channels discord --discord-token "TOKEN"
```

### WhatsApp Setup

WhatsApp uses QR code pairing — no token flag needed. The agent prints a QR code to the console on first launch.

```bash
azureclaw dev --channels whatsapp
```

Scan the QR code with WhatsApp on your phone to link the session.

---

## Third-Party Plugins

### How It Works

Plugins are activated by providing their API keys via CLI flags. The entrypoint sets the corresponding environment variable inside the sandbox, and the agent auto-discovers available plugins at startup.

| Plugin | CLI Flag | Env Var |
|--------|----------|---------|
| Brave Search | `--brave-api-key` | `BRAVE_API_KEY` |
| Tavily | `--tavily-api-key` | `TAVILY_API_KEY` |
| Exa | `--exa-api-key` | `EXA_API_KEY` |
| Firecrawl | `--firecrawl-api-key` | `FIRECRAWL_API_KEY` |
| Perplexity | `--perplexity-api-key` | `PERPLEXITY_API_KEY` |
| OpenAI | `--openai-api-key` | `OPENAI_API_KEY` |

```bash
# Enable Brave and Tavily search
azureclaw dev --brave-api-key "KEY" --tavily-api-key "KEY"

# AKS deployment with plugins
azureclaw add my-agent --brave-api-key "KEY" --tavily-api-key "KEY" --learn-egress
```

### Environment Variable Fallback

If you prefer not to pass keys via CLI flags, set the environment variables directly before running the CLI. The entrypoint checks for both the flag and the corresponding env var:

```bash
export BRAVE_API_KEY="your-key"
export TAVILY_API_KEY="your-key"
azureclaw dev
```

---

## Foundry Web Search (Bing Grounding)

### Overview

Built-in web search powered by Azure AI Foundry's Responses API with Bing Grounding. Unlike third-party plugins, this requires **no API key** — it uses the Foundry project's Bing connection, auto-discovered at runtime.

### Setup

1. **Create a Bing Grounding resource:**
   Go to [Grounding with Bing Search](https://portal.azure.com/#create/Microsoft.BingGroundingSearch) in the Azure Portal and create the resource. In the portal, you'll see a resource creation blade with fields for Subscription, Resource Group, Name, and Region. The resource type is **Grounding with Bing Search** under the **AI + Machine Learning** category.

2. **Connect to your Foundry project:**
   In the [Azure AI Foundry portal](https://ai.azure.com), navigate to your project → **Management** → **Connected resources** → **+ New connection** → select the Bing Grounding resource you created. The connected resources list will show the Bing connection with its resource ID and status.

3. **Deploy your agent:**
   The `foundry_web_search` tool auto-discovers the Bing connection at startup — zero config needed.

```bash
# Local development with Foundry web search
azureclaw dev --build

# AKS (connection discovered automatically via Workload Identity)
azureclaw add my-agent --learn-egress
```

### Manual Override

If auto-discovery fails (e.g., multiple Bing connections), set the connection ID explicitly:

```bash
export BING_CONNECTION_ID="/subscriptions/.../connections/bing-grounding"
azureclaw dev
```

### Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `foundry_web_search` tool not available | No Bing connection in Foundry project | Add Bing Grounding as a connected resource |
| `ConnectionNotFound` error | Connection ID mismatch | Set `BING_CONNECTION_ID` explicitly |
| `AuthorizationFailed` | Workload Identity missing permissions | Ensure the managed identity has `Cognitive Services User` role on the Bing resource |
| Search returns empty results | Bing resource in wrong region | Bing Grounding is global; verify the connection is active |

---

## Credentials on AKS (K8s Secrets)

When deploying to AKS with `azureclaw add`, channel tokens and plugin API keys are stored as Kubernetes secrets in the agent's namespace:

```
CLI (azureclaw add --telegram-token "...")
    │
    ▼
K8s Secret (azureclaw-<name>/channel-telegram-token)
    │
    ▼
Controller mounts via envFrom in pod spec
    │
    ▼
entrypoint.sh reads env vars → configures channels/plugins
    │
    ▼
Agent process (pre-configured, never sees raw tokens)
```

Secret naming convention:

| Credential Type | Secret Name |
|----------------|-------------|
| Telegram token | `channel-telegram-token` |
| Slack token | `channel-slack-token` |
| Discord token | `channel-discord-token` |
| Brave API key | `plugin-brave-api-key` |
| Tavily API key | `plugin-tavily-api-key` |
| Exa API key | `plugin-exa-api-key` |
| Firecrawl API key | `plugin-firecrawl-api-key` |
| Perplexity API key | `plugin-perplexity-api-key` |
| OpenAI API key | `plugin-openai-api-key` |

These secrets are:
- **Created automatically** by the CLI during `azureclaw add`
- **Mounted as environment variables** into the sandbox pod by the controller
- **Never exposed** to the agent process — the entrypoint reads them and configures channels/plugins before handing off to the agent
- **Scoped to the agent namespace** — other agents cannot access them

### Rotating Credentials

Use `azureclaw credentials update` to rotate tokens on a running sandbox:

```bash
# Update a single credential
azureclaw credentials update my-agent --telegram-token "NEW_TOKEN"

# Update multiple credentials at once
azureclaw credentials update my-agent --telegram-token "NEW" --brave-api-key "NEW"

# Update without restarting the pod (apply on next restart)
azureclaw credentials update my-agent --telegram-token "NEW" --no-restart
```

The command updates the K8s secret and triggers a rolling restart of the sandbox pod (unless `--no-restart` is passed).

---

## Entrypoint Auto-Configuration

The sandbox entrypoint (`sandbox-images/entrypoint.sh`) handles channel and plugin setup:

1. **Reads environment variables** injected from K8s secrets (AKS) or CLI flags (local dev)
2. **Enables channels** — starts the appropriate adapter (Telegram polling, Slack WebSocket, etc.)
3. **Activates plugins** — registers tools with the agent runtime when their API keys are present
4. **Discovers Foundry connections** — queries the Foundry API for Bing Grounding connections
5. **Starts the agent** — hands off to OpenClaw with all channels and plugins configured

No manual configuration files needed — everything is driven by environment variables.

---

## Troubleshooting

### Channel Issues

| Symptom | Cause | Fix |
|---------|-------|-----|
| Telegram bot not responding | Token invalid or bot not started | Verify token with `curl https://api.telegram.org/bot<TOKEN>/getMe` |
| Telegram `409 Conflict` | Another instance polling the same bot | Stop other instances; only one poller per bot token |
| Slack messages not received | Missing scopes | Add `chat:write`, `app_mentions:read`, `im:history` in Slack App config |
| Slack `invalid_auth` | Token revoked or wrong workspace | Reinstall the Slack app and use the new `xoxb-` token |
| Discord bot offline | Missing `MESSAGE_CONTENT` intent | Enable it in Discord Developer Portal → Bot → Privileged Gateway Intents |
| WhatsApp QR not appearing | Console output buffered | Check gateway logs: `kubectl logs <pod> -c openclaw` |
| Channel traffic blocked | Domain not on egress allowlist | Run `azureclaw egress <name> --learned` and approve channel API domains |

### Plugin Issues

| Symptom | Cause | Fix |
|---------|-------|-----|
| Plugin tool not available | API key env var not set | Verify with `kubectl exec <pod> -c openclaw -- env \| grep API_KEY` |
| Plugin returns errors | Invalid API key | Use `azureclaw credentials update <name> --brave-api-key "NEW_KEY"` |
| Plugin works locally but not on AKS | Secret not mounted | Check secret exists: `kubectl get secret -n azureclaw-<name>` |
| Multiple plugins conflicting | N/A — plugins are independent | Each plugin registers its own tool; no conflicts expected |

### Foundry Bing Search Issues

| Symptom | Cause | Fix |
|---------|-------|-----|
| `foundry_web_search` tool not available | No Bing connection in Foundry project | Add Bing Grounding as a connected resource in the Foundry portal |
| `ConnectionNotFound` error | Connection ID mismatch or multiple connections | Set `BING_CONNECTION_ID` explicitly |
| `AuthorizationFailed` | Workload Identity missing permissions | Ensure the managed identity has `Cognitive Services User` role on the Bing resource |
| Search returns empty results | Bing resource inactive | Verify the connection is active in the Foundry portal |

### General Debugging

```bash
# Check which channels and plugins are active
kubectl logs <pod> -c openclaw -n azureclaw-<name> | head -50

# Check env vars injected into the sandbox
kubectl exec <pod> -c openclaw -n azureclaw-<name> -- env | sort

# View entrypoint auto-configuration output
kubectl logs <pod> -c openclaw -n azureclaw-<name> | grep -E "(channel|plugin|bing)"

# Rotate a credential and restart
azureclaw credentials update <name> --telegram-token "NEW_TOKEN"
```
