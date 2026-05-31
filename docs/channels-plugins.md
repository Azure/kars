# Channels & external plugins

Messaging channels (Telegram, Slack, Discord, WhatsApp) and **third-party** search/scrape API integrations (Brave, Tavily, Exa, Firecrawl, Perplexity, OpenAI) extend your kars agent with external communication and search capabilities. Configuration is via CLI flags — the sandbox entrypoint auto-configures everything from environment variables at startup.

> **Looking for the kars-owned plugins?** This page is about **external** integrations. For the kars-owned components:
> - **[kars OpenClaw plugin](openclaw-plugin.md)** — the in-sandbox plugin (24 governance-aware tools, 10 skills) shipped with every kars-managed agent.
> - **[`@kars/mesh` plugin](mesh-plugin.md)** — the companion npm package for pairing a **local** OpenClaw with a remote kars cluster (8 federation tools, 1 skill).

---

## Messaging Channels

### Overview

Channels connect your agent to messaging platforms. Pass channel flags to `kars dev` (local) or `kars add` (AKS) and the entrypoint enables them automatically.

| Channel | Flag | Credential Flag | Notes |
|---------|------|-----------------|-------|
| Telegram | `--channels telegram` | `--telegram-token` | Token from [BotFather](https://t.me/BotFather) |
| Slack | `--channels slack` | `--slack-token` | Bot User OAuth Token (`xoxb-...`) |
| Discord | `--channels discord` | `--discord-token` | Bot token from Discord Developer Portal |
| WhatsApp | `--channels whatsapp` | — | QR code pairing at runtime (no token needed) |

Multiple channels can be enabled at once:

```bash
kars dev --channels telegram,slack --telegram-token "TOKEN" --slack-token "xoxb-TOKEN"
```

### Telegram Setup

1. Message [@BotFather](https://t.me/BotFather) → `/newbot` → copy the token
2. Pass the token to the CLI:

```bash
# Local
kars dev --channels telegram --telegram-token "123456:ABC-DEF..."

# AKS
kars add my-agent --channels telegram --telegram-token "123456:ABC-DEF..." --learn-egress
```

### Slack Setup

1. Create a Slack App at [api.slack.com/apps](https://api.slack.com/apps)
2. Add Bot Token Scopes: `chat:write`, `app_mentions:read`, `im:history`
3. Install to workspace → copy Bot User OAuth Token

```bash
kars dev --channels slack --slack-token "xoxb-..."
```

### Discord Setup

1. Create an application at [discord.com/developers](https://discord.com/developers/applications)
2. Add a Bot → copy the token
3. Invite the bot to your server with `Send Messages` and `Read Message History` permissions

```bash
kars dev --channels discord --discord-token "TOKEN"
```

### WhatsApp Setup

WhatsApp uses QR code pairing — no token flag needed. The agent prints a QR code to the console on first launch.

```bash
kars dev --channels whatsapp
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
kars dev --brave-api-key "KEY" --tavily-api-key "KEY"

# AKS deployment with plugins
kars add my-agent --brave-api-key "KEY" --tavily-api-key "KEY" --learn-egress
```

### Environment Variable Fallback

If you prefer not to pass keys via CLI flags, set the environment variables directly before running the CLI. The entrypoint checks for both the flag and the corresponding env var:

```bash
export BRAVE_API_KEY="your-key"
export TAVILY_API_KEY="your-key"
kars dev
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
kars dev --build

# AKS (connection discovered automatically via Workload Identity)
kars add my-agent --learn-egress
```

### Manual Override

If auto-discovery fails (e.g., multiple Bing connections), set the connection ID explicitly:

```bash
export BING_CONNECTION_ID="/subscriptions/.../connections/bing-grounding"
kars dev
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

When deploying to AKS with `kars add`, channel tokens and plugin API keys are stored as Kubernetes secrets in the agent's namespace:

```
CLI (kars add --telegram-token "...")
    │
    ▼
K8s Secret (kars-<name>/<name>-credentials)
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

Secret naming convention: All credentials are stored in a **single secret** named `<name>-credentials` in the `kars-<name>` namespace. The secret contains keys mapped to environment variables:

| Credential Type | Secret Key | Environment Variable |
|----------------|------------|---------------------|
| Telegram token | `TELEGRAM_BOT_TOKEN` | `TELEGRAM_BOT_TOKEN` |
| Telegram allowlist | `TELEGRAM_ALLOW_FROM` | `TELEGRAM_ALLOW_FROM` |
| Slack token | `SLACK_BOT_TOKEN` | `SLACK_BOT_TOKEN` |
| Discord token | `DISCORD_BOT_TOKEN` | `DISCORD_BOT_TOKEN` |
| WhatsApp | `WHATSAPP_ENABLED` | `WHATSAPP_ENABLED` |
| Brave API key | `BRAVE_API_KEY` | `BRAVE_API_KEY` |
| Tavily API key | `TAVILY_API_KEY` | `TAVILY_API_KEY` |
| Exa API key | `EXA_API_KEY` | `EXA_API_KEY` |
| Firecrawl API key | `FIRECRAWL_API_KEY` | `FIRECRAWL_API_KEY` |
| Perplexity API key | `PERPLEXITY_API_KEY` | `PERPLEXITY_API_KEY` |
| OpenAI API key | `OPENAI_API_KEY` | `OPENAI_API_KEY` |

These secrets are:
- **Created automatically** by the CLI during `kars add` or `kars credentials update`
- **Mounted as environment variables** into the sandbox pod by the controller (via `envFrom` with `optional: true` — pods start even without credentials)
- **Read by the entrypoint** — which auto-configures channels/plugins before handing off to the agent
- **Scoped to the agent namespace** — other agents cannot access them

### Rotating Credentials

Use `kars credentials update` to rotate tokens on a running sandbox:

```bash
# Update a single credential
kars credentials update my-agent --telegram-token "NEW_TOKEN"

# Update multiple credentials at once
kars credentials update my-agent --telegram-token "NEW" --brave-api-key "NEW"

# Update without restarting the pod (apply on next restart)
kars credentials update my-agent --telegram-token "NEW" --no-restart
```

The command updates the K8s secret and triggers a rolling restart of the sandbox pod (unless `--no-restart` is passed).

---

## Entrypoint Auto-Configuration

The sandbox entrypoint (`sandbox-images/openclaw/entrypoint.sh`) handles channel and plugin setup:

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
| Channel traffic blocked | Domain not on egress allowlist | Run `kars egress <name> --learned` and approve channel API domains |

### Plugin Issues

| Symptom | Cause | Fix |
|---------|-------|-----|
| Plugin tool not available | API key env var not set | Verify with `kubectl exec <pod> -c openclaw -- env \| grep API_KEY` |
| Plugin returns errors | Invalid API key | Use `kars credentials update <name> --brave-api-key "NEW_KEY"` |
| Plugin works locally but not on AKS | Secret not mounted | Check secret exists: `kubectl get secret -n kars-<name>` |
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
kubectl logs <pod> -c openclaw -n kars-<name> | head -50

# Check env vars injected into the sandbox
kubectl exec <pod> -c openclaw -n kars-<name> -- env | sort

# View entrypoint auto-configuration output
kubectl logs <pod> -c openclaw -n kars-<name> | grep -E "(channel|plugin|bing)"

# Rotate a credential and restart
kars credentials update <name> --telegram-token "NEW_TOKEN"
```
