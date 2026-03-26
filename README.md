<div align="center">

# 🔱 AzureClaw

**Secure AI Agent Runtime for Azure**

[![License: MIT](https://img.shields.io/badge/License-MIT-0078D4.svg)](LICENSE)
[![CI](https://github.com/Azure/azureclaw/actions/workflows/ci.yml/badge.svg)](https://github.com/Azure/azureclaw/actions/workflows/ci.yml)
[![Azure](https://img.shields.io/badge/Azure-AKS%20%7C%20Foundry%20%7C%20Kata-0078D4)](https://azure.microsoft.com)

Run AI agents in hardened sandboxes on AKS with defense-in-depth security.<br>
Zero-credential inference through Azure AI Foundry. Optional Kata VM isolation. Multi-agent governance via AGT.

</div>

---

## What is AzureClaw?

AzureClaw is a production runtime for AI agents on Azure. It solves the core problem: **how do you give an AI agent real tools without giving it the keys to the kingdom?** Each agent runs inside a hardened sandbox on AKS — with a Rust sidecar that mediates all external access. Agents never see Azure credentials (the sidecar authenticates via Workload Identity), every inference call passes through Content Safety + Prompt Shields, and all inter-agent messaging is E2E encrypted via Signal Protocol. For maximum isolation, upgrade to Kata Confidential VMs — per-pod dedicated kernels where container escapes hit a hardware boundary. One CLI command (`azureclaw up`) takes you from zero to a fully secured, governed agent runtime.

---

## Architecture

```
                 ┌──────────┐
                 │ User     │
                 │ TUI / TG │
                 └────┬─────┘
                      │
        ┌─────────────▼──────────────────────────────────────────────┐
        │  AKS Cluster (Azure Linux · Cilium)                        │
        │                                                            │
        │  ┌─ Sandbox Pod (per agent) ────────────────────────────┐  │
        │  │                                                      │  │
        │  │  init: egress-guard (iptables)                       │  │
        │  │   └─ agent process → localhost + DNS only             │  │
        │  │                                                      │  │
        │  │  ┌──────────────┐   localhost   ┌─────────────────┐  │  │
        │  │  │  OpenClaw    │──────:8443───►│ Inference Router │──┼──┼──► Azure AI Foundry
        │  │  │  (agent)     │               │ (Rust sidecar)   │  │  │     (200+ models)
        │  │  └──────────────┘               │                  │  │  │
        │  │   read-only rootfs              │ • WI/IMDS auth   │  │  │
        │  │   drop ALL caps                 │ • Content Safety  │  │  │
        │  │   no Azure credentials          │ • Token budgets   │  │  │
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

### 🔒 Security (defense-in-depth)

**Always on (all isolation levels):**

- **iptables egress guard** — agent process can only reach `localhost`; all external traffic forced through sidecar
- **NetworkPolicy** — default-deny egress at the cluster level
- **Read-only rootfs** — non-root, drop ALL capabilities, no privilege escalation
- **Domain blocklist** — 51k+ domains auto-refreshed from OISD + URLhaus every 6h
- **Content Safety + Prompt Shields** — Azure AI Content Safety on every inference call
- **Zero Azure credentials** — agents never see Azure auth tokens; the sidecar authenticates via IMDS/Workload Identity

**Per isolation level (`--isolation`):**

| Level | Runtime | What it adds |
|-------|---------|-------------|
| `standard` | runc | Kernel-default seccomp filter |
| `enhanced` (default) | runc | Custom strict seccomp profile (~219 allowed syscalls) |
| `confidential` | Kata VM | Per-pod dedicated kernel on AMD SEV-SNP hardware — container escapes hit a VM boundary |

> **Note on plugin credentials:** Channel tokens (Telegram, Slack) and third-party API keys (Brave, Tavily) are accessible to the agent process — plugins need them to function. However, the agent cannot exfiltrate them: iptables blocks all outbound traffic except through the governed sidecar. Azure auth tokens remain isolated in the sidecar at all times.

### 🤖 AI Agent

- **Messaging channels** — Telegram, Slack, Discord, WhatsApp (auto-configured via CLI flags)
- **Third-party plugins** — Brave, Tavily, Exa, Firecrawl, Perplexity (API key → auto-enabled)
- **Foundry web search** — Bing Grounding via Responses API (zero-config, no API key needed)
- **Sub-agent spawning** — agents create child agents via CRD (isolated, governed)
- **10 Foundry skills** — memory, code interpreter, web search, knowledge, evaluations, and more
- **200+ models** — hot-switch between GPT-4.1, GPT-5-mini, DeepSeek-V3.2, Phi-4, Llama, etc.
- **Multi-frontend** — TUI, Telegram, Web UI at `localhost:18789`

### 🏛️ Governance (AGT)

- **Trust scoring** — per-agent scores 0–1000 across 5 tiers (Ed25519 signed)
- **E2E encryption** — Signal Protocol (X3DH + Double Ratchet) for inter-agent messaging
- **Policy engine** — YAML-based tool-level allow/deny/approval/rate-limit rules
- **Audit trail** — hash-chain append-only log with integrity verification

### ⚙️ Operations

- **One-command deploy** — `azureclaw up` provisions AKS + ACR + Foundry + sandbox end-to-end
- **Operator dashboard** — `azureclaw operator` launches a live TUI for managing all agents
- **Credential management** — `azureclaw credentials update` rotates tokens for running sandboxes
- **Image pipeline** — `azureclaw push` builds and pushes images to ACR with optional rollout
- **Monitoring** — Prometheus metrics, Log Analytics, eBPF tracing via `azureclaw trace`

---

## Quick Start

### Prerequisites

Node.js 22+ · Docker (for local dev) · Azure CLI 2.60+ · kubectl · Helm (for AKS)

### Try It Locally (Docker)

No Azure subscription needed — start a sandbox in seconds:

```bash
# 1. Install
git clone https://github.com/Azure/azureclaw.git
cd azureclaw/cli && npm install && npm run build && npm link

# 2. Start a local agent
azureclaw dev

# 3. Chat through the TUI
🦞 You: What's the latest news about AI security?

# 4. Add a Telegram channel
azureclaw dev --channels telegram --telegram-token "BOT_TOKEN"

# 5. Add third-party search plugins
azureclaw dev --brave-api-key "KEY" --tavily-api-key "KEY"
```

### Deploy to AKS (Production)

```bash
# Full stack deploy — prompts for region & subscription
azureclaw up

# Connect to your agent
azureclaw connect my-assistant

# Review egress activity
azureclaw egress my-assistant --learned
```

`azureclaw up` runs preflight checks (Azure CLI, kubectl, Helm, subscription, SKU availability), prompts for region and agent name, then provisions everything end-to-end.

### Add Agents

```bash
# Add a governed agent with Telegram and egress learning
azureclaw add research-bot \
  --model gpt-4.1 \
  --channels telegram --telegram-token "BOT_TOKEN" \
  --governance --learn-egress

# Add a confidential agent (auto-provisions Kata nodepool if needed)
azureclaw add helper --model gpt-5-mini --isolation confidential
```

### Operator Dashboard

```bash
azureclaw operator
```

Live TUI for managing all agents across your cluster:

```
┌─────────────── 🔱 AzureClaw Operator │ azureclaw-aks │ ● API 5/5 ──┐
│ ● research-bot    Running   gpt-4.1     enhanced   tg    2h         │
│ └ kernel-checker  Running   gpt-5-mini  enhanced          45m       │
│ ● helper          Running   gpt-5-mini  confidential      30m       │
├── Security ────────┬── Egress ──────────┬── Activity ────────────────┤
│ Isolation enhanced │ ●P api.openai.com  │ ✓ Approved 3 domains      │
│ Seccomp   strict   │ ✓A graph.microsoft │ ↻ Refreshed 3 agents      │
├────────────────────┴────────────────────┴────────────────────────────┤
│ [Tab] Focus [↑↓] Nav [Enter] Connect [c] Cluster [n] Spawn [q] Quit│
└─────────────────────────────────────────────────────────────────────-┘
```

**Keyboard:** `Enter` connect to agent TUI · `Tab` switch panels · `a` approve egress · `Shift+A` approve all · `d` delete/deny · `e` enforce egress · `n` spawn agent · `m` switch model · `c` cluster health · `l` logs · `r` refresh · `q` quit

### Update Credentials

Rotate channel tokens or plugin API keys on running sandboxes without redeploying:

```bash
azureclaw credentials update my-agent \
  --telegram-token "NEW_TOKEN" \
  --brave-api-key "NEW_KEY"
```

---

## CLI Reference

| Command | Description |
|---|---|
| **Lifecycle** | |
| `azureclaw up` | Deploy full stack — preflight, AKS + ACR + Foundry + sandbox |
| `azureclaw dev` | Local Docker sandbox with same security controls |
| `azureclaw add <name>` | Add sandbox to existing cluster |
| `azureclaw destroy [name]` | Tear down sandbox or entire resource group (`--all`) |
| `azureclaw push` | Build and push images to ACR (`--only`, `--apply`) |
| **Operations** | |
| `azureclaw operator` | Live TUI dashboard — agents, egress, security, cluster health |
| `azureclaw connect <name>` | TUI, shell (`--shell`), or Web UI (`--web`) |
| `azureclaw status <name>` | Health, model, tokens used |
| `azureclaw list` | All sandboxes across Docker and AKS |
| `azureclaw logs <name>` | Stream logs (`-f`, `--service router\|gateway\|openclaw`) |
| **Configuration** | |
| `azureclaw credentials` | Set Azure OpenAI credentials (interactive) |
| `azureclaw credentials update <name>` | Rotate channel/plugin keys on running sandbox |
| `azureclaw model set <name> <model>` | Switch model (hot-swap, no restart) |
| `azureclaw model get <name>` | Show current model |
| `azureclaw model list [name]` | List available Foundry models |
| `azureclaw policy allow <name> <host>` | Add allowed egress endpoint |
| `azureclaw policy get <name>` | Show active policy |
| `azureclaw policy deny <name> <host>` | Remove allowed endpoint |
| `azureclaw egress <name>` | Egress management (`--learned`, `--pending`, `--approve`, `--enforce`) |
| **Observability** | |
| `azureclaw trace <name>` | eBPF tracing (`--network`, `--dns`, `--files`, `--exec`) |
| `azureclaw eval <name>` | Run Foundry evaluations against agent |

### Common Flags

These flags are shared across `dev`, `add`, and `credentials update`:

| Flag | Description |
|---|---|
| `--channels telegram,slack,discord,whatsapp` | Enable messaging channels |
| `--telegram-token`, `--slack-token`, `--discord-token` | Channel credentials |
| `--brave-api-key`, `--tavily-api-key`, `--exa-api-key` | Search plugins |
| `--firecrawl-api-key`, `--perplexity-api-key`, `--openai-api-key` | Additional plugins |
| `--governance` / `--no-governance` | AGT governance (trust, policy, audit) |
| `--learn-egress` | Enable egress learn mode |
| `--isolation standard\|enhanced\|confidential` | Pod isolation level |
| `--model <model>` | AI model (default: `gpt-4.1`) |

---

## Security Model

Every sandbox runs in its own namespace with defense layers stacked in depth. Some layers are always active; others depend on the isolation level you choose.

### Always Active

| Layer | Control |
|---|---|
| **Container hardening** | Read-only rootfs, non-root, drop ALL capabilities, no privilege escalation |
| **iptables egress guard** | Agent process restricted to localhost + DNS; all internet traffic goes through sidecar |
| **NetworkPolicy** | Default-deny egress at the Kubernetes level (Cilium-enforced) |
| **Domain blocklist** | 51k+ known-bad domains blocked; auto-refreshes from OISD + URLhaus every 6h |
| **Inference safety** | Content Safety + Prompt Shields on every request + per-agent token budgets |
| **Zero Azure credentials** | Agent never sees Azure auth tokens — sidecar authenticates via IMDS/Workload Identity |

### Per Isolation Level

| Level | Runtime | Security posture |
|---|---|---|
| `standard` | runc | Kernel-default seccomp, shared node pool |
| `enhanced` (**default**) | runc | Custom strict seccomp (~219 syscalls), shared node pool |
| `confidential` | Kata VM | Per-pod dedicated kernel on AMD SEV-SNP hardware; container escapes hit a VM boundary; dedicated node pool; isolation inherited by sub-agents |

### Credential Compartmentalization

| Credential type | Where it lives | Agent can see it? |
|---|---|---|
| Azure auth tokens (IMDS, WI) | Sidecar only (projected file) | ❌ No — iptables blocks IMDS, file permissions enforce separation |
| Azure OpenAI API key | Sidecar only (`/run/secrets/`) | ❌ No — mounted only in sidecar container |
| Plugin API keys (Brave, Tavily, etc.) | Agent env vars | ✅ Yes — plugins need them. Agent cannot exfiltrate: egress blocked by iptables |
| Channel tokens (Telegram, Slack) | Agent env vars | ✅ Yes — channels need them. Same egress protection applies |

See [docs/security.md](docs/security.md) for full details and OWASP LLM Top 10 coverage.

---

## Channels & Plugins

### Messaging Channels

| Channel | Flag | Credential |
|---------|------|-----------|
| Telegram | `--channels telegram` | `--telegram-token` (BotFather) |
| Slack | `--channels slack` | `--slack-token` (Bot OAuth) |
| Discord | `--channels discord` | `--discord-token` |
| WhatsApp | `--channels whatsapp` | QR code pairing at runtime |

On AKS, channel tokens are stored as K8s secrets and injected into the sandbox pod automatically.

### Third-Party Plugins

| Plugin | Flag |
|--------|------|
| Brave Search | `--brave-api-key` |
| Tavily | `--tavily-api-key` |
| Exa | `--exa-api-key` |
| Firecrawl | `--firecrawl-api-key` |
| Perplexity | `--perplexity-api-key` |
| OpenAI | `--openai-api-key` |

Plugins auto-activate when their API key is present. No additional configuration needed.

### Foundry Web Search (Bing Grounding)

Built-in web search via Azure AI Foundry's Responses API. **No API key needed** — auto-discovers the Foundry project's Bing connection at runtime.

See [docs/channels-plugins.md](docs/channels-plugins.md) for setup and details.

---

## Documentation

| Document | Description |
|---|---|
| [Architecture](docs/architecture.md) | Component design, CRD schema, API routes, auth flow |
| [Security](docs/security.md) | Defense-in-depth model, OWASP coverage, threat mitigations |
| [Channels & Plugins](docs/channels-plugins.md) | Telegram, Slack, Discord, search plugins, Foundry Bing |
| [Egress Proxy](docs/egress-proxy.md) | Blocklist, allowlist, learn mode, approval flow |
| [E2E Encryption](docs/e2e-encryption-proof.md) | Signal Protocol inter-agent encryption proof |
| [Multi-Tenant](docs/multi-tenant.md) | Namespace isolation, credential and channel separation |
| [Security Validation](docs/security-validation.md) | Live cluster evidence for every security layer |
| [Demo](docs/DEMO.md) | "Operation Claw Shield" — multi-tenant attack simulation |

---

## Egress Proxy

All agent network traffic is mediated by the inference router sidecar:

| Mode | Behavior |
|---|---|
| **Blocklist** (always on) | 51k+ known-bad domains blocked; auto-refreshes from OISD + URLhaus |
| **Allowlist** | Only pre-approved domains permitted |
| **Learn mode** | Unknown domains allowed + recorded; promote to allowlist when ready |

```bash
azureclaw add my-agent --model gpt-4.1 --learn-egress  # deploy with learn mode
azureclaw egress my-agent --learned                      # review discovered domains
azureclaw egress my-agent --enforce                      # lock down to learned set
```

---

## Project Structure

```
azureclaw/
├── cli/                  # TypeScript CLI (azureclaw command)
│   └── skills/           # Foundry skill definitions (10 skills)
├── controller/           # Rust K8s operator (ClawSandbox CRDs)
├── inference-router/     # Rust sidecar proxy (axum)
├── policy-engine/        # Seccomp profiles & security policies
├── sandbox-images/       # OpenClaw container images
├── deploy/               # Bicep IaC, Helm charts, monitoring
├── docs/                 # Architecture, security, demo guides
├── examples/             # Sample agents (basic, confidential, telegram)
├── tests/                # E2E tests (Docker + Kind)
└── vendor/               # AgentMesh SDK (8 vendor bug fixes)
```

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

[MIT](LICENSE) · [Code of Conduct](CODE_OF_CONDUCT.md)
