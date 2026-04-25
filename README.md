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

> **Not a fork.** AzureClaw extends [OpenClaw](https://openclaw.ai) using its native
> plugin API and `tools.deny` config — no OpenClaw source is modified, patched, or
> vendored. Any upstream OpenClaw release is drop-in compatible. See
> [Upstream Alignment](docs/upstream-alignment.md) for the full rationale.

---

## What is AzureClaw?

AzureClaw is a production runtime for AI agents on Azure. It solves the core problem: **how do you give an AI agent real tools without giving it the keys to the kingdom?** Each agent runs inside a hardened sandbox on AKS — with a Rust inference router that mediates all external access. Agents never see Azure credentials (the router authenticates via Workload Identity), every inference call passes through Content Safety + Prompt Shields, and all inter-agent messaging is E2E encrypted via Signal Protocol. AGT governance (policy, trust, audit) runs natively inside the router — no sidecar needed. For maximum isolation, upgrade to Kata Confidential VMs — per-pod dedicated kernels where container escapes hit a hardware boundary. One CLI command (`azureclaw up`) takes you from zero to a fully secured, governed agent runtime.

---

## Architecture

```
                    ┌──────────┐
                    │ User     │
                    │ TUI / TG │
                    └────┬─────┘
                         │
   ┌─────────────────────▼───────────────────────────────────────────────────┐
   │  AKS Cluster (Azure Linux · Cilium)                                     │
   │                                                                         │
   │  ┌─ Sandbox Pod (per agent) ─────────────────────────────────────────┐  │
   │  │                                                                   │  │
   │  │  init: egress-guard (iptables)                                    │  │
   │  │   └─ agent process → localhost + DNS only                         │  │
   │  │                                                                   │  │
   │  │  ┌──────────────┐   localhost    ┌──────────────────┐             │  │
   │  │  │  OpenClaw    │──────:8443────►│ Inference Router  │────────────┼──┼──► Azure AI Foundry
   │  │  │  (agent)     │               │ (Rust)            │            │  │     (200+ models)
   │  │  └──────────────┘               │                   │            │  │
   │  │   read-only rootfs              │ • WI/IMDS auth    │            │  │
   │  │   drop ALL caps                 │ • Content Safety   │            │  │
   │  │   no Azure credentials          │ • Token budgets    │            │  │
   │  │                                 │ • Domain blocklist │            │  │
   │  │                                 │ • Egress proxy     │            │  │
   │  │                                 │ • AGT governance   │            │  │
   │  │                                 │   (native Rust)    │            │  │
   │  │                                 │   • PolicyEngine   │            │  │
   │  │                                 │   • TrustManager   │            │  │
   │  │                                 │   • AuditLogger    │            │  │
   │  │                                 │   • RateLimiter    │            │  │
   │  │                                 └────────────────────┘            │  │
   │  │  NetworkPolicy: default-deny egress                               │  │
   │  └───────────────────────────────────────────────────────────────────┘  │
   │                                                                         │
   │  ┌─ AgentMesh ───────────────────────────────────────────────────────┐  │
   │  │  agent-alpha ◄──E2E encrypted──► agent-beta                       │  │
   │  │  (Signal Protocol · X3DH + Double Ratchet · trust-gated)          │  │
   │  │                                                                    │  │
   │  │  agentmesh-relay (WebSocket :8765) — routes encrypted messages     │  │
   │  │  agentmesh-registry (REST :8080 + PostgreSQL) — discovery/prekeys  │  │
   │  └───────────────────────────────────────────────────────────────────┘  │
   │                                                                         │
   │  Controller (Rust/kube-rs) — reconciles ClawSandbox CRDs               │
   └─────────────────────────────────────────────────────────────────────────┘
```

> 📐 **[Architecture & Flow Diagrams](docs/architecture-diagrams.md)** — Mermaid diagrams for all core flows: pod architecture, agent creation, sub-agent spawning, E2E encrypted communication, inference routing, egress control, bidirectional handoff with sub-agents, and defense-in-depth layers.

### Docker Images

| Image | Language | Purpose |
|-------|----------|---------|
| `azureclaw-controller` | Rust | K8s operator — reconciles ClawSandbox CRDs into pods |
| `azureclaw-inference-router` | Rust | Inference proxy — Content Safety, native AGT governance, egress filtering |
| `azureclaw-sandbox` / `openclaw-sandbox` | Node.js | Main agent container (OpenClaw + AGT SDK + Python tools) |
| `agentmesh-relay` | Rust | WebSocket relay for E2E encrypted inter-agent messaging |
| `agentmesh-registry` | Rust + PostgreSQL | Agent discovery, prekey storage, React admin UI |

All images build on Azure Linux 3 (`mcr.microsoft.com/azurelinux/base/core:3.0`).

---

## Key Features

### 🔒 Security (defense-in-depth)

**Always on (all isolation levels):**

- **iptables egress guard** — agent process can only reach `localhost`; all external traffic forced through router
- **NetworkPolicy** — default-deny egress at the cluster level
- **Read-only rootfs** — non-root, drop ALL capabilities, no privilege escalation
- **Domain blocklist** — 51k+ domains auto-refreshed from OISD + URLhaus every 6h
- **Content Safety + Prompt Shields** — Azure AI Content Safety on every inference call
- **Zero Azure credentials** — agents never see Azure auth tokens; the router authenticates via IMDS/Workload Identity

**Per isolation level (`--isolation`):**

| Level | Runtime | What it adds |
|-------|---------|-------------|
| `standard` | runc | Kernel-default seccomp filter |
| `enhanced` (default) | runc | Custom strict seccomp profile (~219 allowed syscalls) |
| `confidential` | Kata VM | Per-pod dedicated kernel on AMD SEV-SNP hardware — container escapes hit a VM boundary |

> **Note on plugin credentials:** Channel tokens (Telegram, Slack) and third-party API keys (Brave, Tavily) are accessible to the agent process — plugins need them to function. However, the agent cannot exfiltrate them: iptables blocks all outbound traffic except through the governed router. Azure auth tokens remain isolated in the router at all times.

### 🤖 AI Agent

- **Messaging channels** — Telegram, Slack, Discord, WhatsApp (auto-configured via CLI flags)
- **Third-party plugins** — Brave, Tavily, Exa, Firecrawl, Perplexity (API key → auto-enabled)
- **Foundry web search** — Bing Grounding via Responses API (zero-config, no API key needed)
- **Sub-agent spawning** — agents create child agents via CRD (isolated, governed, full tool access via native delegation)
- **10 Foundry skills** — web search (Bing Grounding), code execute, image generation, file search, memory, conversations, evaluations, knowledge, and more — all via Workload Identity (no API keys)
- **Python 3** — 43 packages pre-installed: pandas, numpy, scipy, matplotlib, pdfplumber, pypdf, python-docx, openpyxl, python-pptx, Pillow, sqlalchemy, tiktoken, cryptography, networkx, and more
- **200+ models** — hot-switch between GPT-4.1, GPT-5-mini, DeepSeek-V3.2, Phi-4, Llama, etc.
- **Multi-frontend** — TUI, Telegram, Web UI at `localhost:18789`

### 🏛️ Governance (AGT — native Rust)

- **Native governance** — policy evaluation, trust management, and audit run in-process inside the Rust inference router (no sidecar, <1µs eval latency)
- **Trust scoring** — per-agent scores 0–1000, threshold 500, clamped ±200/update, Ed25519 signed
- **Policy engine** — YAML-driven rules (hot-reloaded) covering shell safety, inference rate-limiting, content safety, mesh trust gates
- **Audit trail** — SHA-256 Merkle tree append-only chain with tamper detection and integrity verification
- **Components** — PolicyEngine, TrustManager, AuditLogger, RateLimiter, BehaviorMonitor (native Rust, compiled into the inference router)
- **Prometheus metrics** — `azureclaw_agt_policy_evaluations_total`, `azureclaw_agt_eval_latency_seconds`, `azureclaw_agt_behavior_alerts_total`, and more

### 🔐 E2E Encryption (Signal Protocol)

- **X3DH key exchange** — identity, signed-prekey, and one-time prekey bundles for session setup
- **Double Ratchet** — per-message forward secrecy via ratchet rotation
- **KNOCK protocol** — policy-gated session establishment (trust score ≥ 500 required)
- **AgentMesh relay** — untrusted WebSocket relay (`:8765`) routes encrypted payloads without decryption
- **AgentMesh registry** — agent discovery and prekey storage (REST `:8080` + PostgreSQL)

### ⚙️ Operations

- **One-command deploy** — `azureclaw up` provisions AKS + ACR + Foundry + sandbox end-to-end
- **Live handoff** — `azureclaw handoff <name> --to cloud|local` migrates agents between local Docker and AKS with sub-agent state, E2E encrypted workspace transfer, and task resumption
- **Operator dashboard** — `azureclaw operator` launches a live TUI for managing all agents
- **Credential management** — `azureclaw credentials update` rotates tokens for running sandboxes
- **Image pipeline** — `azureclaw push` builds and pushes images to ACR with optional rollout
- **Monitoring** — Prometheus metrics, Log Analytics, eBPF tracing via `azureclaw trace`

---

## Quick Start

### Prerequisites

| Tool | Version | Required For |
|------|---------|--------------|
| Node.js | 22+ | CLI (both paths) |
| Docker | Latest | Local dev + image builds |
| Azure CLI | 2.60+ | AKS path only |
| kubectl | 1.28+ | AKS path only |
| Helm | 3.14+ | AKS path only |
| Rust | 1.88+ (edition 2024) | Building from source (both paths) |

> **Azure RBAC:** `azureclaw up` needs `Contributor` **and** `User Access Administrator` at subscription scope (or `Owner`). See [`docs/permissions.md`](docs/permissions.md) for the full breakdown, a least-privilege custom role, and common failure modes. The CLI runs a preflight check automatically and fails fast in ≤30s if anything is missing.

### Step 1: Install the CLI

```bash
git clone https://github.com/Azure/azureclaw.git
cd azureclaw

# Build the CLI
cd cli && npm ci && npm run build && npm link
cd ..

# Verify
azureclaw --help
```

---

### Path A: Local Dev (Docker) — no Azure needed

Start a sandboxed agent locally in under a minute:

```bash
# Build the sandbox image and start it
azureclaw dev --build

# First run will prompt for Azure OpenAI credentials:
#   Endpoint: https://your-resource.openai.azure.com
#   API Key:  sk-...
# Or set them beforehand:
azureclaw credentials

# You're now in a chat session with a governed AI agent
🦞 You: What's the latest news about AI security?
```

**Optional enhancements:**

```bash
# Add Telegram channel
azureclaw dev --build --channels telegram --telegram-token "BOT_TOKEN"

# Add third-party search plugins
azureclaw dev --build --brave-api-key "KEY" --tavily-api-key "KEY"

# Use a specific model
azureclaw dev --build --model gpt-5-mini
```

> **What happens:** Docker builds the sandbox image (Azure Linux 3 + inference router + OpenClaw), starts a container with iptables egress filtering, and connects you via the TUI. No Kubernetes needed.

---

### Path B: Deploy to AKS (Production)

Full production deployment with all 9 security layers:

```bash
# 1. Login to Azure
az login

# 2. Build all 5 container images (controller, router, sandbox, relay, registry)
#    First run takes ~10 min; subsequent builds are cached
azureclaw push

# 3. Deploy everything — AKS cluster, ACR, Key Vault, Azure OpenAI, Helm chart
#    Prompts for region, subscription, and agent name
azureclaw up

# 4. Verify the cluster is healthy
azureclaw operator    # live TUI — press 'c' for cluster health

# 5. Connect to your first agent
azureclaw connect my-assistant
```

**What `azureclaw up` does (in order):**
1. Preflight checks (az, kubectl, helm, subscription, SKU availability)
2. Deploys Azure infrastructure via Bicep (AKS, ACR, Key Vault, AOAI)
3. Installs AzureClaw Helm chart (CRD, controller, RBAC, seccomp profiles)
4. Deploys AgentMesh (relay + registry) for E2E encrypted inter-agent comms
5. Creates your first agent sandbox with native AGT governance

**After deployment:**

```bash
# Add more agents
azureclaw add research-bot --model gpt-4.1 --governance --learn-egress

# Add channels
azureclaw credentials update research-bot --telegram-token "BOT_TOKEN"

# Add a confidential agent (auto-provisions Kata nodepool)
azureclaw add helper --model gpt-5-mini --isolation confidential

# Review egress activity
azureclaw egress research-bot --learned

# Multi-agent communication (E2E encrypted)
azureclaw connect research-bot
🦞 You: @helper can you review this code for security issues?
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
| `azureclaw up --upgrade` | Fast upgrade — reuse cached context, Helm + RBAC + fedcred sync |
| `azureclaw dev` | Local Docker sandbox with same security controls |
| `azureclaw add <name>` | Add sandbox to existing cluster |
| `azureclaw destroy [name]` | Tear down sandbox or entire resource group (`--all`) |
| `azureclaw push` | Build and push all 5 images to ACR (`--apply` restarts deployments, `--only <image>` for single image) |
| **Operations** | |
| `azureclaw operator` | Live TUI dashboard — agents, egress, security, cluster health |
| `azureclaw connect <name>` | TUI, shell (`--shell`), or Web UI (`--web`) |
| `azureclaw handoff <name> --to cloud` | Live-migrate agent + sub-agents from local Docker to AKS |
| `azureclaw handoff <name> --to local` | Live-migrate agent + sub-agents from AKS back to local Docker |
| `azureclaw handoff <name> --status` | Show current handoff progress |
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
| **Multi-Agent** | |
| `azureclaw mesh auth` | Authenticate with global AgentMesh registry (OAuth) |
| `azureclaw mesh status` | Show mesh connectivity and registered agents |
| `azureclaw mesh send <amid>` | Send E2E encrypted message to another agent |

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
| **iptables egress guard** | Agent process restricted to localhost + DNS; all internet traffic goes through router |
| **NetworkPolicy** | Default-deny egress at the Kubernetes level (Cilium-enforced) |
| **Domain blocklist** | 51k+ known-bad domains blocked; auto-refreshes from OISD + URLhaus every 6h |
| **Inference safety** | Content Safety + Prompt Shields on every request + per-agent token budgets |
| **Content Safety** | Foundry-side guardrails (`DefaultV2`) — content filter annotations parsed from model responses; no separate API call needed |
| **Zero Azure credentials** | Agent never sees Azure auth tokens — router authenticates via IMDS/Workload Identity |
| **Admin token** | From K8s Secret mounted at `/etc/azureclaw/secrets/` — never hardcoded; required for trust mutation. Canonical header is `Authorization: Bearer <token>`; the legacy `x-azureclaw-admin` header is still accepted but emits a one-shot `warn!` on first use and will be removed in a future release. Compared in constant time (`handoff::constant_time_eq`). Optional `ROUTER_ADMIN_ALLOW_IPS` IP allowlist + `ADMIN_ALLOWED_ORIGINS` browser-origin gate add defence-in-depth on top of the token. |
| **AGT policy evaluation** | Per-request governance on inference, spawn, mesh receive, and response actions |
| **Audit chain** | SHA-256 Merkle tree with integrity verification (validated on AKS: `integrity=valid`) |

### Per Isolation Level

| Level | Runtime | Security posture |
|---|---|---|
| `standard` | runc | Kernel-default seccomp, shared node pool |
| `enhanced` (**default**) | runc | Custom strict seccomp (~219 syscalls), shared node pool |
| `confidential` | Kata VM | Per-pod dedicated kernel on AMD SEV-SNP hardware; container escapes hit a VM boundary; dedicated node pool; isolation inherited by sub-agents |

### Credential Compartmentalization

| Credential type | Where it lives | Agent can see it? |
|---|---|---|
| Azure auth tokens (IMDS, WI) | Router only (projected file) | ❌ No — iptables blocks IMDS, file permissions enforce separation |
| Azure OpenAI API key | Router only (`/run/secrets/`) | ❌ No — mounted only in router container |
| Admin token | K8s Secret (`/etc/azureclaw/secrets/`) | ❌ No — mounted only in router; required for trust mutation |
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
| [Architecture](docs/architecture.md) | Component design, CRD schema, API routes, operator dashboard, auth flow |
| [Architecture Diagrams](docs/architecture-diagrams.md) | Mermaid flow diagrams: pod layout, agent creation, spawn, mesh, egress, inference |
| [Security](docs/security.md) | Defense-in-depth model, OWASP coverage, threat mitigations |
| [Channels & Plugins](docs/channels-plugins.md) | Telegram, Slack, Discord, search plugins, Foundry Bing |
| [Egress Proxy](docs/egress-proxy.md) | Blocklist, allowlist, learn mode, approval flow |
| [E2E Encryption](docs/e2e-encryption-proof.md) | Signal Protocol inter-agent encryption proof |
| [Multi-Tenant](docs/multi-tenant.md) | Namespace isolation, credential and channel separation |
| [Security Validation](docs/security-validation.md) | Live cluster evidence for every security layer |
| [Demo](docs/DEMO.md) | "Operation Claw Shield" — multi-tenant attack simulation |

---

## Egress Proxy

All agent network traffic is mediated by the inference router:

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
│   ├── skills/           # Foundry skill definitions (10 skills)
│   └── policies/         # AGT governance policy YAML (default rules)
├── controller/           # Rust K8s operator (ClawSandbox CRDs)
├── inference-router/     # Rust inference proxy (axum) — includes native AGT governance
├── sandbox-images/       # OpenClaw container images
├── policy-engine/        # Seccomp profiles & security policies
├── deploy/               # Bicep IaC, Helm charts, AgentMesh K8s manifests
├── docs/                 # Architecture, security, E2E encryption, demo guides
├── examples/             # Sample agents (basic, confidential, telegram, demo)
├── tests/                # E2E tests (Docker + Kind)
└── vendor/               # AgentMesh SDK, relay, registry (patched forks)
```

> **📦 Why `vendor/`?** AgentMesh is pre-release — we found and fixed 8 bugs in the relay, registry, and SDK (see `vendor/*/README.md` for each patch). These are carried as patched forks until fixes land upstream. Once AgentMesh ships a stable release, `vendor/` goes away entirely.

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
