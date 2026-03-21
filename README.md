# AzureClaw

**Secure runtime for [OpenClaw](https://openclaw.ai) AI agents on Azure.**

[![License](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/Azure/azureclaw/actions/workflows/ci.yml/badge.svg)](https://github.com/Azure/azureclaw/actions/workflows/ci.yml)

AzureClaw runs AI agents safely on AKS with defense-in-depth security, zero-credential inference through [Azure AI Foundry](https://learn.microsoft.com/azure/ai-studio/), and multi-agent governance via [AGT](https://github.com/microsoft/agent-governance-toolkit).

> **Alpha** — Interfaces may change. We welcome issues and discussion.

---

## Architecture

```
┌─ AKS Cluster (Azure Linux, Cilium CNI) ──────────────────────────────────────┐
│                                                                               │
│  azureclaw-system namespace                                                   │
│  ┌─────────────────────────────────────────────┐                              │
│  │ Controller (Rust, kube-rs) × 2 replicas     │  Watches ClawSandbox CRDs    │
│  │ Reconciles → namespace, pod, NetworkPolicy, │  and creates sandboxes       │
│  │ ServiceAccount, Service, ConfigMap          │                              │
│  └─────────────────────────────────────────────┘                              │
│                                                                               │
│  azureclaw-<agent> namespace (per sandbox)                                    │
│  ┌────────────────────────────────────────────────────────────────────┐        │
│  │ Pod (2 containers + 1 init)                                        │        │
│  │                                                                    │        │
│  │  init: egress-guard (iptables: UID 1000 → localhost + DNS only)    │        │
│  │                                                                    │        │
│  │  ┌──────────────────────┐    ┌────────────────────────────────┐    │        │
│  │  │ openclaw (UID 1000)  │    │ inference-router (UID 1001)    │    │        │
│  │  │ • OpenClaw agent     │    │ • IMDS/WI auth (zero keys)    │    │        │
│  │  │ • Read-only rootfs   │───►│ • Content Safety + Shields    │───►│ Foundry │
│  │  │ • 9 Foundry skills   │    │ • Token budgets (429)         │    │        │
│  │  │ • AGT governance     │    │ • 18 Foundry API groups       │    │        │
│  │  │                      │    │ • AGT mesh + trust + audit    │    │        │
│  │  └──────────────────────┘    └────────────────────────────────┘    │        │
│  │       localhost:8443 ──────────────────┘                           │        │
│  ├─ Service: {name}:8443 (K8s DNS for AGT mesh)                      │        │
│  ├─ NetworkPolicy (default-deny egress + mesh ingress)                │        │
│  └─ ServiceAccount (Workload Identity)                                │        │
└───────────────────────────────────────────────────────────────────────────────┘
```

### Why These Components?

| Component | Language | Why It Exists |
|-----------|----------|---------------|
| **CLI** (`azureclaw`) | TypeScript | Single command to go from zero to production. Provisions AKS, ACR, Key Vault, Foundry, Helm, CRD — or runs locally with Docker. |
| **Controller** | Rust (kube-rs) | K8s operator that reconciles `ClawSandbox` CRDs into isolated sandboxes with all security controls. Creates namespace, ServiceAccount, NetworkPolicy, Deployment, Service, ConfigMap per agent. |
| **Inference Router** | Rust (axum) | Per-sandbox sidecar proxy — the **only** network path for the agent. Handles IMDS auth, Content Safety, Prompt Shields, token budgets, SSE streaming, all 18 Foundry API groups, AGT governance (policy, trust, audit, mesh). |
| **OpenClaw Plugin** | TypeScript | Slash commands inside the agent: `/azureclaw-models`, `/azureclaw-security`, `/azureclaw-switch`. Ships 9 Foundry skills. |

### External Dependencies

| Dependency | What We Use It For | Link |
|---|---|---|
| **[OpenClaw](https://openclaw.ai)** | Open-source AI agent framework — authoring, orchestration, TUI | [GitHub](https://github.com/anthropics/openclaw) |
| **[Azure AI Foundry](https://learn.microsoft.com/azure/ai-studio/)** | 200+ models, Memory Store, Code Interpreter, Web Search, Knowledge, Evaluations — all via Responses API | [Docs](https://learn.microsoft.com/azure/ai-studio/reference/reference-model-inference-api) |
| **[AGT](https://github.com/microsoft/agent-governance-toolkit)** | Multi-agent governance: tool-level policy, inter-agent trust, tamper-evident audit, mesh communication | [GitHub](https://github.com/microsoft/agent-governance-toolkit) |

---

## Quick Start

```bash
# Install
git clone https://github.com/Azure/azureclaw.git
cd azureclaw/cli && npm install && npm run build && npm link

# Local (Docker)
azureclaw onboard            # configure credentials (once)
azureclaw dev                # start sandbox locally
azureclaw connect dev-agent  # chat with the agent

# AKS Production
az login
azureclaw up --name my-agent --model gpt-4.1
azureclaw connect my-agent
```

**Prerequisites:** Node.js 22+, Azure CLI 2.60+, Docker.

---

## Foundry Integration

All [Azure AI Foundry](https://learn.microsoft.com/azure/ai-studio/) services are accessed through the inference router via the **Responses API** — no hosted Foundry agents needed. OpenClaw is the orchestrator; Foundry provides managed AI services.

| Foundry Service | Router Path | What It Does |
|---|---|---|
| **Model Catalog (200+)** | `/v1/chat/completions` | GPT-4.1, GPT-5-mini, DeepSeek-V3.2, Phi-4, Llama, etc. |
| **Memory Store** | `/memory_stores/*` | Persistent cross-session memory with semantic search |
| **Code Interpreter** | `/openai/responses` + `code_interpreter` | Python execution for data analysis |
| **Web Search** | `/openai/responses` + `bing_grounding` | Real-time web grounding with citations |
| **Memory Search** | `/openai/responses` + `memory_search` | Cross-session memory recall |
| **Knowledge (Foundry IQ)** | `/openai/responses` + `file_search` | RAG over uploaded documents |
| **Evaluations** | `/openai/evals`, `/evaluators` | Run evaluations, list evaluators |
| **Conversations** | `/openai/conversations` | Persistent multi-turn threads |
| **Deployments** | `/deployments` | Query deployed models + versions |

**9 skills** ship with AzureClaw teaching the agent how to use each service:
`foundry-memory`, `foundry-code`, `foundry-knowledge`, `foundry-web-search`, `foundry-agents`, `foundry-conversations`, `foundry-evaluations`, `foundry-deployments`, `agt-governance`.

---

## AGT Governance

[Agent Governance Toolkit](https://github.com/microsoft/agent-governance-toolkit) provides application-layer governance that infrastructure controls can't see (tool calls, inter-agent trust, behavioral audit).

```bash
azureclaw add my-agent --governance --trust-threshold 700 --policy-profile default
```

| Capability | How It Works | API |
|---|---|---|
| **Policy enforcement** | Evaluate tool calls against YAML policies (allow/deny/approval/rate-limit) | `POST /agt/evaluate` |
| **Trust scoring** | Per-agent scores 0-1000, 5 tiers, evolve with interaction outcomes | `GET /agt/trust/{agent}` |
| **Inter-agent mesh** | Trust-gated messaging via K8s DNS (`{agent}.{ns}.svc.cluster.local:8443`) | `POST /agt/mesh/send` |
| **Tamper-evident audit** | Hash-chain append-only log, integrity verification | `GET /agt/audit/verify` |

**Multi-agent flow:** Agent A → Router A (trust gate + audit) → K8s DNS → Router B (trust gate + audit) → Agent B inbox.

AGT does NOT duplicate infrastructure controls — no network rules, no content safety, no token budgets. See [docs/security.md](docs/security.md) for the overlap resolution.

---

## Security

8 infrastructure layers (always on) + AGT governance (opt-in):

| # | Layer | What It Does |
|---|---|---|
| 0 | **Azure infra** | NSG, AKS API IP allowlist, DDoS protection |
| 1 | **Node OS** | Azure Linux, SELinux enforcing, auto-patched |
| 2 | **Kata VM** | Per-pod dedicated kernel (confidential level only) |
| 3 | **Container** | Read-only rootfs, non-root, drop ALL capabilities |
| 4 | **Kernel** | Custom seccomp (~150 allowed syscalls) |
| 5 | **Network** | iptables UID guard + default-deny NetworkPolicy |
| 6 | **Inference** | Content Safety + Prompt Shields + token budgets |
| 7 | **AGT** | Policy engine + trust scoring + audit log + mesh |

**Zero credentials**: agents never see API keys. Router authenticates via IMDS.

See [docs/security.md](docs/security.md) for details.

---

## Isolation Levels

| Level | Runtime | Seccomp | Use Case |
|---|---|---|---|
| **standard** | runc | RuntimeDefault | Dev/test |
| **enhanced** (default) | runc | `azureclaw-strict` | Production |
| **confidential** | Kata VM | RuntimeDefault | Untrusted code, regulated environments |

---

## Examples

### Deploy two governed agents that communicate

```bash
azureclaw add agent-alpha --model gpt-4.1 --governance --trust-threshold 500
azureclaw add agent-beta --model gpt-4.1 --governance --trust-threshold 500

# Alpha sends a task to Beta (from inside sandbox)
curl -X POST http://localhost:8443/agt/mesh/send \
  -H 'Content-Type: application/json' \
  -d '{"to_agent":"agent-beta","content":"Analyze the cluster security","type":"task_request"}'

# Beta checks inbox
curl http://localhost:8443/agt/mesh/inbox
```

### Store and recall memories

```bash
# Store memories
curl -X POST 'http://localhost:8443/memory_stores/my-store:update_memories?api-version=2025-11-15-preview' \
  -d '{"items":[{"role":"user","content":"I prefer Rust and espresso","type":"message"}],"scope":"default"}'

# Semantic search
curl -X POST 'http://localhost:8443/memory_stores/my-store:search_memories?api-version=2025-11-15-preview' \
  -d '{"scope":"default","items":[{"role":"user","content":"What language?","type":"message"}],"options":{"max_memories":5}}'
```

### Run Code Interpreter

```bash
curl -X POST 'http://localhost:8443/openai/responses?api-version=2025-11-15-preview' \
  -d '{"model":"gpt-4.1","input":"Calculate fibonacci(20)","tools":[{"type":"code_interpreter","container":{"type":"auto"}}],"store":false}'
```

---

## CLI Commands

| Command | Description |
|---|---|
| `azureclaw up` | Deploy full AKS stack + first sandbox |
| `azureclaw add <name>` | Add sandbox (supports `--governance`, `--trust-threshold`, `--policy-profile`) |
| `azureclaw dev` | Local Docker sandbox (with iptables + seccomp) |
| `azureclaw connect <name>` | Attach to OpenClaw TUI |
| `azureclaw status <name>` | Health, model, tokens used |
| `azureclaw model set <name> <model>` | Hot-switch AI model |
| `azureclaw eval <name>` | Run Foundry evaluations (`--list-evaluators`, `--dataset`) |
| `azureclaw policy allow <name> <host>` | Manage network allowlist |
| `azureclaw trace <name>` | eBPF tracing |
| `azureclaw approve --list` | Review pending egress requests |
| `azureclaw logs <name>` | Stream container logs |
| `azureclaw destroy <name>` | Tear down sandbox or resource group |

---

## Infrastructure

Provisioned by `azureclaw up` via Bicep:

| Module | Resources |
|---|---|
| `aks.bicep` | AKS (Azure Linux, Cilium, Workload Identity) |
| `acr.bicep` | Container Registry (Premium, content trust) |
| `openai.bicep` | Cognitive Services (Entra ID auth only) |
| `keyvault.bicep` | Key Vault (RBAC, soft-delete) |
| `monitor.bicep` | Log Analytics + Application Insights |

---

## Development

```bash
make build    # Rust + TypeScript
make test     # Unit tests
make lint     # clippy + oxlint
make images   # Docker images
```

---

## Documentation

| Doc | Description |
|---|---|
| [Architecture](docs/architecture.md) | Component design, CRD schema, Foundry API routes, authentication flow |
| [Security](docs/security.md) | 8-layer defense-in-depth, OWASP coverage, AGT governance |
| [Multi-Tenant](docs/multi-tenant.md) | Namespace isolation model |
| [Demo](docs/DEMO.md) | Multi-agent attack simulation |
| [Contributing](CONTRIBUTING.md) | Build, test, PR process |

---

## License

[MIT](LICENSE) · [Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/)
