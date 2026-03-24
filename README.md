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

---

## CLI Reference

| Command | Description |
|---|---|
| **Lifecycle** | |
| `azureclaw up` | Deploy full stack — preflight checks, interactive prompts, AKS + ACR + AOAI + sandbox |
| `azureclaw dev` | Local Docker sandbox (same security controls) |
| `azureclaw add <name>` | Add sandbox (`--governance`, `--learn-egress`, `--isolation`) |
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
