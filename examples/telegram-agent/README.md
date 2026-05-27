# Telegram Agent Example

Deploy an AI agent connected to Telegram with optional Bing web search.

## Local Development

```bash
# Start a local sandbox with Telegram channel
kars dev --channels telegram --telegram-token "YOUR_TOKEN"

# With a third-party search plugin
kars dev --channels telegram --telegram-token "YOUR_TOKEN" --brave-api-key "YOUR_KEY"
```

## AKS Production

```bash
# Deploy to AKS with governance and auto-learned egress rules
kars add telegram-agent \
  --channels telegram \
  --telegram-token "YOUR_TOKEN" \
  --governance \
  --learn-egress

# Check status
kars status telegram-agent

# View logs
kars logs telegram-agent
```

## Update Token

```bash
# Rotate the Telegram bot token without redeploying
kars credentials update telegram-agent --telegram-token "NEW_TOKEN"
```

This updates the `telegram-agent-credentials` K8s secret and restarts the pod.

## Deploy Plugin Changes

After modifying the sandbox image or entrypoint:

```bash
kars push --only sandbox --apply
```

## How It Works

The channel/plugin pattern:

```
CLI flag (--telegram-token) → env var (TELEGRAM_BOT_TOKEN)
  → entrypoint.sh auto-config → plugins.allow + plugins.entries + channels.telegram
```

1. `kars add` stores the token in a K8s secret (`telegram-agent-credentials`)
2. The controller mounts the secret via `envFrom` (optional: true)
3. `entrypoint.sh` detects `TELEGRAM_BOT_TOKEN` and configures the Telegram channel
4. OpenClaw loads the Telegram extension and connects to the Telegram Bot API

## CRD Manifest

See [`karssandbox.yaml`](karssandbox.yaml) for the Kubernetes CRD manifest you can apply directly:

```bash
kubectl apply -f examples/telegram-agent/karssandbox.yaml
```

## Prerequisites

- Telegram bot token from [@BotFather](https://t.me/BotFather)
- AKS cluster provisioned via `kars up` (for production)
- (Optional) Bing Grounding resource connected to Foundry project for web search
- (Optional) Third-party API key for Brave, Tavily, Exa, etc.
