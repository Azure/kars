# AzureClaw

**Secure runtime for [OpenClaw](https://openclaw.ai) AI agents on Azure.**

[![License](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/Azure/azureclaw/actions/workflows/ci.yml/badge.svg)](https://github.com/Azure/azureclaw/actions/workflows/ci.yml)

AzureClaw runs AI agents safely on AKS with defense-in-depth security, zero-credential inference through [Azure AI Foundry](https://learn.microsoft.com/azure/ai-studio/), and multi-agent governance via [AGT](https://github.com/microsoft/agent-governance-toolkit).

> **Alpha** — Interfaces may change. We welcome issues and discussion.

---

## What Is AzureClaw?

AzureClaw is the **runtime layer** between [OpenClaw](https://openclaw.ai) (the agent framework) and [Azure AI Foundry](https://learn.microsoft.com/azure/ai-studio/) (managed AI services). It solves one problem: **how do you run AI agents on Azure without giving them the keys to the kingdom?**

Every agent runs in an isolated Kubernetes sandbox with 8 layers of security. The agent can only reach the outside world through a **Rust sidecar proxy** (inference router) that authenticates, filters, rate-limits, and audits every request. When multiple agents need to collaborate, they communicate through a **trust-gated mesh** — no direct network access between sandboxes.

**Three pillars:**
- **[OpenClaw](https://openclaw.ai)** owns the agent — authoring, orchestration, TUI, skills
- **[Azure AI Foundry](https://learn.microsoft.com/azure/ai-studio/)** provides managed AI services — 200+ models, memory, code interpreter, web search, evaluations
- **[AGT](https://github.com/microsoft/agent-governance-toolkit)** governs multi-agent behavior — tool-level policy, inter-agent trust, tamper-evident audit

AzureClaw connects them with enterprise-grade security out of the box.

---

## Architecture

```
┌─ AKS Cluster (Azure Linux, Cilium CNI) ──────────────────────────────────────────┐
│                                                                                    │
│  azureclaw-system namespace                                                        │
│  ┌──────────────────────────────────────────────────────┐                          │
│  │  🦀 Controller (Rust/kube-rs) × 2 replicas           │                          │
│  │  Watches ClawSandbox CRDs → reconciles sandboxes     │                          │
│  │  Creates: namespace, SA, NetworkPolicy, Deployment,   │                          │
│  │           Service, ConfigMap (per agent)              │                          │
│  └──────────────────────────────────────────────────────┘                          │
│  🔒 seccomp DaemonSet (azureclaw-strict on every node)                             │
│                                                                                    │
│  azureclaw-<agent> namespace (one per sandbox)                                     │
│  ┌──────────────────────────────────────────────────────────────────────┐           │
│  │  Pod (2 containers + 1 init)                                         │           │
│  │                                                                      │           │
│  │  init: egress-guard                                                  │           │
│  │   └─ iptables: UID 1000 → localhost + DNS only                       │           │
│  │                                                                      │           │
│  │  🦞 openclaw (UID 1000)              ⚡ inference-router (UID 1001) │           │
│  │  ├─ OpenClaw agent                   ├─ IMDS/WI auth (zero keys)    │           │
│  │  ├─ Read-only rootfs       ────────► ├─ Content Safety + Shields    ──────► ☁️ Foundry
│  │  ├─ 9 Foundry skills      localhost  ├─ Token budgets (429)         │           │
│  │  └─ AGT governance         :8443     ├─ 18 Foundry API groups       │           │
│  │                                      ├─ AGT mesh + trust + audit    │           │
│  │                                      └─ Prometheus metrics          │           │
│  ├─ Service: {name}:8443 (K8s DNS for AGT mesh)                        │           │
│  ├─ NetworkPolicy (default-deny egress + AGT mesh ingress)              │           │
│  └─ ServiceAccount (Workload Identity)                                  │           │
│                                                                                    │
│  🔗 AGT mesh: agent-alpha ◄──K8s DNS──► agent-beta                                │
│     (trust-gated, audited, routed through inference routers)                       │
└────────────────────────────────────────────────────────────────────────────────────┘
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

### Prerequisites

| Requirement | Details |
|---|---|
| **Node.js 22+** | CLI runtime |
| **Azure CLI 2.60+** | `az login` for authentication |
| **Docker** | Local dev mode (`azureclaw dev`) |
| **Azure Subscription** | With permissions to create resource groups |

### Azure Setup (required for AKS deployment)

**Azure AI Foundry project** is required for inference and Foundry services:

1. Create an [Azure AI Foundry](https://learn.microsoft.com/azure/ai-studio/) project (or use an existing one)
2. Deploy at least one model (e.g., `gpt-4.1`) in the project
3. Note the **Foundry project endpoint** (e.g., `https://my-resource.services.ai.azure.com/api/projects/my-project`)

**Required Azure RBAC roles** on the identity running `azureclaw up`:

| Role | Scope | Why |
|---|---|---|
| **Owner** (recommended) | Resource group | Creates resources + assigns RBAC |
| *Or* Contributor + User Access Administrator | Resource group | Split create/assign |

The controller automatically assigns these roles to the AKS kubelet identity:

| Role | Purpose |
|---|---|
| Cognitive Services OpenAI User | Model inference API access |
| Cognitive Services User | Content Safety API access |
| Azure AI User | Memory Store internal model calls |
| AcrPull | Pull sandbox images from ACR |

For Memory Store to work, `azureclaw up` also grants **Azure AI User** to the Foundry project's managed identity on the resource group (automated via Bicep).

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

## Examples — Interacting via OpenClaw

All interaction happens through the **OpenClaw agent TUI** — the agent uses Foundry services automatically via skills. No curl needed.

### Memory Store — remember and recall across sessions

```bash
azureclaw connect my-agent
```
```
🦞 You: Remember that I'm a backend engineer who loves Rust and espresso.
Agent: I've stored that in your profile.

# Later, in a new session:
🦞 You: What programming language do I love and what do I drink?
Agent: You love Rust, and you drink espresso.
```

### Code Interpreter — run Python for data analysis

```
🦞 You: Calculate the first 15 prime numbers using Python and show the list.
Agent: [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47]
```

### Multi-agent governance — two agents communicate via AGT mesh

```bash
# Deploy two governed agents
azureclaw add agent-alpha --model gpt-4.1 --governance --trust-threshold 500
azureclaw add agent-beta --model gpt-4.1 --governance --trust-threshold 500

# Connect to Alpha and ask it to delegate work to Beta
azureclaw connect agent-alpha
```
```
🦞 You: Send a task to agent-beta asking it to analyze our cluster security posture.
Agent: Message sent to agent-beta via AGT mesh (trust score: 500, tier: Standard).
       Message ID: agent-alpha-69bed776-104adcbe, status: delivered.
```
```bash
# Connect to Beta to see the received task
azureclaw connect agent-beta
```
```
🦞 You: Check your inbox for messages from other agents.
Agent: 1 message from agent-alpha:
       "Please analyze the cluster security posture" (type: task_request)
```

### Query Foundry deployments

```
🦞 You: List all deployed AI models in our Foundry project.
Agent: 4 models deployed:
       1. gpt-5-mini (OpenAI, v2025-08-07)
       2. gpt-4.1 (OpenAI, v2025-04-14)
       3. DeepSeek-V3.2 (DeepSeek, v1)
       4. text-embedding-3-small (OpenAI, v1)
```

### Run evaluations

```bash
azureclaw eval my-agent --list-evaluators
azureclaw eval my-agent --dataset test.jsonl --evaluator relevance
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
