<div align="center">

# 🔱 AzureClaw

**Secure AI Agent Runtime for Azure**

[![License: MIT](https://img.shields.io/badge/License-MIT-0078D4.svg)](LICENSE)
[![CI](https://github.com/Azure/azureclaw/actions/workflows/ci.yml/badge.svg)](https://github.com/Azure/azureclaw/actions/workflows/ci.yml)
[![Azure](https://img.shields.io/badge/Azure-AKS%20%7C%20Foundry%20%7C%20Kata-0078D4)](https://azure.microsoft.com)

Run AI agents in Kata Confidential VM sandboxes on AKS with 8 layers of defense-in-depth security.
Zero-credential inference through Azure AI Foundry. Multi-agent governance via AGT.

</div>

---

## Architecture

```
                 ┌──────────┐
                 │ User     │
                 │ TUI / TG │
                 └────┬─────┘
                      │
        ┌─────────────▼──────────────────────────────────────────────┐
        │  AKS Cluster (Azure Linux · Cilium · Kata VMs)            │
        │                                                            │
        │  ┌─ Sandbox Pod (per agent) ────────────────────────────┐  │
        │  │                                                      │  │
        │  │  init: egress-guard (iptables)                       │  │
        │  │   └─ UID 1000 → localhost + DNS only                 │  │
        │  │                                                      │  │
        │  │  ┌──────────────┐   localhost   ┌─────────────────┐  │  │
        │  │  │  OpenClaw    │──────:8443───►│ Inference Router │──┼──┼──► Azure AI Foundry
        │  │  │  (agent)     │               │ (Rust sidecar)   │  │  │     (200+ models)
        │  │  │  UID 1000    │               │ UID 1001         │  │  │
        │  │  └──────────────┘               │                  │  │  │
        │  │   read-only rootfs              │ • IMDS/WI auth   │  │  │
        │  │   drop ALL caps                 │ • Content Safety  │  │  │
        │  │   seccomp strict                │ • Token budgets   │  │  │
        │  │                                 │ • Domain blocklist│  │  │
        │  │                                 │ • Egress proxy    │  │  │
        │  │                                 │ • AGT governance  │  │  │
        │  │                                 └─────────┬────────┘  │  │
        │  │  NetworkPolicy: default-deny egress       │           │  │
        │  └───────────────────────────────────────────┘           │  │
        │                                              │           │  │
        │  ┌─ AGT Relay ──────────────────────────┐    │           │  │
        │  │  agent-alpha ◄──E2E encrypted──► agent-beta           │  │
        │  │  (Signal Protocol · trust-gated · audited)            │  │
        │  └───────────────────────────────────────────────────────┘  │
        │                                                            │
        │  Controller (Rust/kube-rs) — reconciles ClawSandbox CRDs   │
        └────────────────────────────────────────────────────────────┘
```

---

## Key Features

### 🔒 Security (8 layers, always on)

- **Kata Confidential VMs** — per-pod dedicated kernel; container escapes hit a VM boundary
- **iptables egress guard** — agent UID can only reach `localhost`; all external traffic forced through sidecar
- **NetworkPolicy** — default-deny egress at the cluster level
- **Domain blocklist** — 51k+ domains auto-refreshed from OISD + URLhaus every 6h
- **Egress proxy** — allowlist with approval flow + learn mode for zero-config onboarding
- **Content Safety + Prompt Shields** — Azure AI Content Safety on every inference call
- **Seccomp strict** — custom profile, ~150 allowed syscalls
- **Zero credentials** — agents never see API keys; router authenticates via IMDS/Workload Identity

### 🤖 AI Agent

- **OpenClaw gateway** — TUI + Telegram + Web UI frontends
- **Messaging channels** — Telegram, Slack, Discord, WhatsApp (auto-configured)
- **Third-party plugins** — Brave, Tavily, Exa, Firecrawl (API key → auto-enabled)
- **Foundry web search** — Bing Grounding via Responses API (zero-config)
- **Sub-agent spawning** — agents create child agents via CRD (isolated, governed)
- **9 Foundry skills** — memory, code interpreter, web search, knowledge, evaluations, and more
- **200+ models** — hot-switch between GPT-4.1, GPT-5-mini, DeepSeek-V3.2, Phi-4, Llama, etc.

### 🏛️ Governance (AGT)

- **Trust scoring** — per-agent scores 0–1000 across 5 tiers (Ed25519 signed)
- **E2E encryption** — Signal Protocol (X3DH + Double Ratchet) for inter-agent messaging
- **Policy engine** — YAML-based tool-level allow/deny/approval/rate-limit rules
- **Audit trail** — hash-chain append-only log with integrity verification

### ⚙️ Operations

- **CLI** — single `azureclaw up` command provisions AKS + ACR + Foundry + sandbox
- **Web UI** — token-based control interface at `localhost:18789`
- **Telegram channel** — always-on agent communication
- **Monitoring** — Prometheus metrics, Log Analytics, eBPF tracing

---

## Quick Start

```bash
# 1. Install
git clone https://github.com/Azure/azureclaw.git
cd azureclaw/cli && npm install && npm run build && npm link

# 2. Deploy — preflight checks tools, prompts for region & subscription
azureclaw up

# 3. Connect
azureclaw connect my-assistant

# 4. Chat with your agent through the TUI
🦞 You: Summarize the top HackerNews stories about AI security.

# 5. Review egress activity
azureclaw egress my-assistant --learned
```

`azureclaw up` runs preflight checks (Azure CLI, kubectl, Helm, subscription, SKU availability), prompts for region and agent name if not provided, then provisions everything end-to-end.

**Prerequisites:** Node.js 22+ · Azure CLI 2.60+ · kubectl · Helm · Azure subscription

> For local development without Azure: `azureclaw dev` starts a Docker sandbox with the same security controls.

### Local Development

```bash
# Local development (Docker, no Azure needed)
azureclaw dev

# With Telegram channel
azureclaw dev --channels telegram --telegram-token "BOT_TOKEN"

# With third-party search plugins
azureclaw dev --brave-api-key "KEY" --tavily-api-key "KEY"

# With Foundry web search (auto-discovers Bing connection)
# Requires: Bing Grounding resource connected to your Foundry project
azureclaw dev --build
```

---

## CLI Reference

| Command | Description |
|---|---|
| **Lifecycle** | |
| `azureclaw up` | Deploy full stack — preflight checks, interactive prompts, AKS + ACR + AOAI + sandbox |
| `azureclaw dev` | Local Docker sandbox (same security controls). Flags: `--channels telegram,slack,discord`, `--telegram-token`, `--slack-token`, `--discord-token`, `--brave-api-key`, `--tavily-api-key`, `--exa-api-key`, `--firecrawl-api-key`, `--perplexity-api-key`, `--openai-api-key` |
| `azureclaw add <name>` | Add sandbox (`--governance`, `--learn-egress`, `--isolation`). Same channel/plugin flags as `dev`; credentials stored as K8s secrets |
| `azureclaw destroy <name>` | Tear down sandbox or resource group |
| **Operations** | |
| `azureclaw connect <name>` | Connect TUI to sandbox |
| `azureclaw status <name>` | Health, model, tokens used |
| `azureclaw logs <name>` | View container logs |
| **Configuration** | |
| `azureclaw credentials` | Set or update Azure OpenAI credentials |
| `azureclaw model set <name> <model>` | Switch model (hot-swap, no restart) |
| `azureclaw policy <subcommand>` | Network policy management (`allow`, `deny`, `get`, `learn`, `set`) |
| `azureclaw egress <name>` | Manage egress security (`--pending`, `--approve`, `--deny`, `--allowlist`, `--learned`) |
| **Observability** | |
| `azureclaw trace <name>` | eBPF tracing |
| `azureclaw eval <name>` | Run Foundry evaluations |

> **No prerequisite commands.** Both `up` and `dev` prompt for any missing configuration inline. `up` runs preflight checks (tools, auth, SKU availability) before provisioning.

---

## Security Model

Every agent runs in an isolated namespace with 8 defense layers stacked in depth:

| # | Layer | Control |
|---|---|---|
| 0 | **Azure Infra** | NSG, AKS API IP allowlist, DDoS protection |
| 1 | **Node OS** | Azure Linux, SELinux enforcing, auto-patched |
| 2 | **Kata VM** | Per-pod dedicated kernel (confidential isolation) |
| 3 | **Container** | Read-only rootfs, non-root, drop ALL capabilities |
| 4 | **Kernel** | Custom seccomp profile (~150 allowed syscalls) |
| 5 | **Network** | iptables UID guard + NetworkPolicy + 51k+ domain blocklist |
| 6 | **Inference** | Content Safety + Prompt Shields + token budgets |
| 7 | **Governance** | AGT policy engine + trust scoring + audit log |

> Agents never see API keys. The inference router authenticates to Azure AI Foundry via IMDS/Workload Identity.

See [docs/security.md](docs/security.md) for full details and OWASP LLM Top 10 coverage.

---

## Egress Proxy

All agent network traffic is mediated by the inference router sidecar. Three enforcement modes:

| Mode | Behavior |
|---|---|
| **Blocklist** (always on) | 51k+ known-bad domains blocked; auto-refreshes from OISD + URLhaus |
| **Allowlist** | Only pre-approved domains permitted |
| **Learn mode** | Unknown domains allowed + recorded; apply learned set as allowlist when ready |

```bash
# Deploy with learn mode
azureclaw add my-agent --model gpt-4.1 --learn-egress

# Review learned domains
azureclaw egress my-agent --learned

# Lock down to learned set
azureclaw egress my-agent --apply
```

See [docs/security.md](docs/security.md) for the egress architecture.

---

## Channels & Plugins

### Messaging Channels

Connect your agent to messaging platforms. Channels are configured via CLI flags and auto-enabled at startup.

| Channel | Flag | Credential |
|---------|------|-----------|
| Telegram | `--channels telegram` | `--telegram-token` (from BotFather) |
| Slack | `--channels slack` | `--slack-token` (Bot OAuth token) |
| Discord | `--channels discord` | `--discord-token` |
| WhatsApp | `--channels whatsapp` | QR code pairing at runtime |

```bash
# AKS deployment with Telegram
azureclaw add my-agent --channels telegram --telegram-token "BOT_TOKEN" --learn-egress

# Local development
azureclaw dev --channels telegram --telegram-token "BOT_TOKEN"
```

On AKS, channel tokens are stored as K8s secrets and injected into the sandbox pod automatically.

### Third-Party Plugins

Enable search and scraping plugins by providing their API keys. The sandbox auto-activates plugins when their keys are present.

| Plugin | Flag | Env Var |
|--------|------|---------|
| Brave Search | `--brave-api-key` | `BRAVE_API_KEY` |
| Tavily | `--tavily-api-key` | `TAVILY_API_KEY` |
| Exa | `--exa-api-key` | `EXA_API_KEY` |
| Firecrawl | `--firecrawl-api-key` | `FIRECRAWL_API_KEY` |
| Perplexity | `--perplexity-api-key` | `PERPLEXITY_API_KEY` |
| OpenAI | `--openai-api-key` | `OPENAI_API_KEY` |

### Foundry Web Search (Bing Grounding)

Built-in web search via Azure AI Foundry's Responses API with Bing Grounding. **No API key needed** — uses the Foundry project's Bing connection, auto-discovered at runtime.

**Setup:**
1. Create a [Grounding with Bing Search](https://portal.azure.com/#create/Microsoft.BingGroundingSearch) resource
2. Add it as a connection in your Foundry project (Portal → Project → Connected resources)
3. The `foundry_web_search` tool auto-discovers the connection — zero config

> Override: set `BING_CONNECTION_ID` env var with the full resource ID.

See [docs/channels-plugins.md](docs/channels-plugins.md) for full details.

---

## Project Structure

```
azureclaw/
├── cli/                  # TypeScript CLI (azureclaw command)
├── controller/           # Rust K8s operator (ClawSandbox CRDs)
├── inference-router/     # Rust sidecar proxy (axum)
├── policy-engine/        # Seccomp profiles & security policies
├── sandbox-images/       # OpenClaw container images
├── deploy/               # Bicep IaC, Helm charts, monitoring
├── docs/                 # Architecture, security, demo guides
├── examples/             # Sample agents (basic, confidential, multi-tenant)
├── tests/                # E2E tests (Docker + Kind)
└── vendor/               # AgentMesh SDK
```

---

## Documentation

| Document | Description |
|---|---|
| [Architecture](docs/architecture.md) | Component design, CRD schema, API routes, auth flow |
| [Security](docs/security.md) | 8-layer defense model, OWASP coverage, AGT governance |
| [Multi-Tenant](docs/multi-tenant.md) | Namespace isolation model |
| [E2E Encryption](docs/e2e-encryption-proof.md) | Signal Protocol inter-agent encryption proof |
| [Channels & Plugins](docs/channels-plugins.md) | Telegram, Slack, Discord, search plugins, Foundry Bing |
| [Egress Proxy](docs/egress-proxy.md) | Blocklist, allowlist, learn mode |
| [Demo](docs/DEMO.md) | "Operation Claw Shield" — multi-tenant attack simulation |

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build instructions, test process, and PR guidelines.

```bash
make build    # Rust + TypeScript
make test     # Unit tests
make lint     # clippy + oxlint
make images   # Docker images
```

---

## License

[MIT](LICENSE) · [Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/)
