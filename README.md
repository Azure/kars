# AzureClaw

> Secure runtime for AI agents on Azure.

[![License](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/Azure/azureclaw/actions/workflows/ci.yml/badge.svg)](https://github.com/Azure/azureclaw/actions/workflows/ci.yml)

AzureClaw is an open-source Kubernetes-native runtime for running [OpenClaw](https://openclaw.ai/) AI agents on Azure — safely and at scale. It provides sandboxed execution with defense-in-depth security, zero-credential inference through Azure AI Foundry, and per-sandbox governance out of the box.

> **Alpha** — Interfaces may change. We welcome issues and discussion.

---

## What It Does

| Capability | Implementation |
|---|---|
| **Sandboxed execution** | Read-only rootfs, non-root (UID 1000), custom seccomp, iptables UID-based egress, default-deny NetworkPolicy |
| **200+ AI models** | Azure AI Foundry inference via Rust sidecar proxy. IMDS auth — zero keys in sandbox. |
| **Foundry agentic skills** | Persistent memory (threads), knowledge search (file_search), web grounding (web_search), code interpreter — shipped as OpenClaw skills via plugin |
| **Content safety** | Azure AI Content Safety + Prompt Shields on every inference call (on by default, fail-open) |
| **Token governance** | Per-sandbox daily and per-request budgets with HTTP 429 enforcement |
| **Three isolation levels** | `standard` (runc), `enhanced` (custom seccomp), `confidential` (Kata VM per pod) |
| **Multi-tenant** | One AKS cluster, many agents. Each sandbox gets its own namespace, NetworkPolicy, and ServiceAccount. |
| **AGT governance (opt-in)** | Tool-level policy enforcement, inter-agent trust scoring, tamper-evident audit — via [Agent Governance Toolkit](https://github.com/microsoft/agent-governance-toolkit) |
| **Observability** | Prometheus metrics (tokens, latency, requests). Optional eBPF tracing via Inspektor Gadget. |

---

## Quick Start

```bash
git clone https://github.com/Azure/azureclaw.git
cd azureclaw/cli && npm install && npm run build && npm link
```

**Prerequisites:** Node.js 22+, Azure CLI 2.60+, Docker (local dev).

### Local Development

```bash
azureclaw onboard          # configure Azure OpenAI credentials (once)
azureclaw dev              # start sandboxed agent locally (Docker)
azureclaw connect dev-agent  # chat via OpenClaw TUI
```

### AKS Production

```bash
az login
azureclaw up --name my-agent --model gpt-4.1                     # enhanced (default)
azureclaw up --name my-agent --isolation standard                  # basic isolation
azureclaw up --name my-agent --isolation confidential              # Kata VM per pod
azureclaw connect my-agent
```

`azureclaw up` provisions the full stack: resource group, AKS cluster, ACR, Key Vault, Cognitive Services, Helm chart, CRD, and sandbox pod. Subsequent agents use `azureclaw add` (no infra redeploy).

**Azure permissions:**

| Role | Scope | Why |
|------|-------|-----|
| **Owner** (recommended) | Resource group | Creates resources + assigns RBAC (ACR pull, Cognitive Services) |
| *Or* Contributor + User Access Administrator | Resource group | Split create/assign |

---

## Architecture

```
┌─ AKS Cluster ─────────────────────────────────────────────────┐
│                                                                │
│  azureclaw-system namespace                                    │
│  ├─ Controller (Rust, kube-rs) × 2 replicas                   │
│  ├─ ClawSandbox CRD (v1alpha1)                                │
│  └─ seccomp DaemonSet (installs azureclaw-strict on nodes)    │
│                                                                │
│  azureclaw-<agent> namespace (per sandbox)                     │
│  ├─ Pod                                                        │
│  │  ├─ init: egress-guard (iptables UID-based rules)           │
│  │  ├─ openclaw (UID 1000) ── localhost:8443 ──┐               │
│  │  └─ inference-router (UID 1001, Rust/axum) ─┼──► Foundry   │
│  ├─ NetworkPolicy (default-deny egress)                        │
│  └─ ServiceAccount (Workload Identity)                         │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

**Components:**

| Component | Language | Role |
|-----------|----------|------|
| **CLI** | TypeScript | 12 commands: `up`, `add`, `dev`, `connect`, `status`, `logs`, `model`, `trace`, `policy`, `approve`, `onboard`, `destroy` |
| **Controller** | Rust (kube-rs) | K8s operator: reconciles ClawSandbox CRDs into namespaces, pods, NetworkPolicies, iptables init containers |
| **Inference Router** | Rust (axum) | Per-sandbox sidecar proxy: auth (IMDS/WI), Content Safety, Prompt Shields, token budgets, SSE streaming, Prometheus metrics |
| **OpenClaw Plugin** | TypeScript | Slash commands for agent status, model switching within the sandbox |

See [docs/architecture.md](docs/architecture.md) for the full component design.

---

## Isolation Levels

| Level | Runtime | Seccomp | Node Pool | Use Case |
|---|---|---|---|---|
| **standard** | runc | RuntimeDefault | clawpool | Dev/test, trusted agents |
| **enhanced** (default) | runc | Localhost (`azureclaw-strict`) | clawpool | Production |
| **confidential** | Kata VM | RuntimeDefault | katapool | Untrusted code, regulated environments |

All levels include: read-only rootfs, non-root user, drop ALL capabilities, iptables UID-based egress guard, default-deny NetworkPolicy.

---

## Security

Defense-in-depth — every layer is independently enforceable:

1. **Azure infrastructure** — NSG, AKS API server IP allowlist, DDoS protection
2. **Node OS** — Azure Linux, SELinux enforcing, auto-patched
3. **Kata VM** (confidential only) — per-pod dedicated kernel via Cloud Hypervisor
4. **Container hardening** — read-only rootfs, non-root, no privilege escalation, drop ALL
5. **Kernel confinement** — custom seccomp profile (~150 allowed syscalls, blocks ptrace/mount/bpf/unshare)
6. **Per-container network** — iptables rules: agent (UID 1000) restricted to localhost + DNS; router (UID 1001) controlled by NetworkPolicy
7. **Inference safety** — Content Safety + Prompt Shields + token budgets on every request

**Zero credentials**: agents never see API keys. The inference router authenticates via IMDS (kubelet Managed Identity) or Workload Identity federation.

See [docs/security.md](docs/security.md) for details.

---

## Azure Services

### Implemented

| Service | How AzureClaw Uses It |
|---------|----------------------|
| **AKS** | Runtime cluster — Azure Linux nodes, Cilium CNI, Workload Identity |
| **Azure AI Foundry** | Inference backend (200+ models via `/openai/v1/`), Agent API proxy (`/agents/*` for threads, memory, files, runs) |
| **Azure AI Content Safety** | Input/output filtering on every inference call (Hate, SelfHarm, Sexual, Violence) |
| **Prompt Shields** | Jailbreak and prompt injection detection |
| **ACR** | Container image registry (Premium SKU, content trust) |
| **Key Vault** | Secret storage (soft-delete, purge protection) |
| **Log Analytics** | Cluster monitoring (90-day retention) |
| **Application Insights** | Application-level telemetry |
| **Managed Identity** | IMDS-based auth for inference + ACR pull + Cognitive Services access |

### Roadmap (not yet implemented)

| Service | Planned Use |
|---------|-------------|
| **Azure Storage / AI Search / Cosmos DB** | `azureServices` CRD field exists in schema but controller does not yet create RBAC bindings |
| **Azure Monitor Alerts** | Token spike and egress anomaly alerting |

---

## Key Commands

| Command | What It Does |
|---|---|
| `azureclaw up --name <n>` | Deploy full AKS stack + first sandbox |
| `azureclaw add <name>` | Add sandbox to existing cluster (no infra redeploy) |
| `azureclaw dev` | Local Docker sandbox |
| `azureclaw connect <name>` | Attach to OpenClaw TUI |
| `azureclaw status <name>` | Health, model, tokens used |
| `azureclaw model set <name> <model>` | Hot-switch AI model |
| `azureclaw policy allow/deny <name> <host>` | Manage network allowlist (hot-reload via CRD patch) |
| `azureclaw trace <name>` | eBPF tracing (requires Inspektor Gadget) |
| `azureclaw approve --list` | Review pending egress requests |
| `azureclaw destroy <name>` | Tear down sandbox or entire resource group |

---

## Infrastructure

Provisioned by `azureclaw up` via Bicep (5 modules):

| Module | Resources |
|--------|-----------|
| **aks.bicep** | AKS cluster (Azure Linux, Cilium, WI), system + clawpool + optional katapool, RBAC role assignments |
| **acr.bicep** | Container Registry (Premium, content trust, firewall) |
| **openai.bicep** | Cognitive Services (OpenAI kind), model deployment, OIDC auth only |
| **keyvault.bicep** | Key Vault (RBAC, soft-delete, purge protection) |
| **monitor.bicep** | Log Analytics (90-day) + Application Insights |

Helm chart deploys: CRD, controller (2 replicas), RBAC, seccomp DaemonSet, NetworkPolicy template.

---

## Development

```bash
make build    # Rust (controller + router) + TypeScript CLI
make test     # 13 unit tests (Rust)
make lint     # clippy + oxlint
make images   # Docker images (controller + inference-router)
make help     # all targets
```

**Rust:** edition 2024, rust-version 1.88, kube-rs 3.1, axum 0.8
**Node:** 22 LTS, TypeScript

---

## Learn More

- [Architecture](docs/architecture.md) — component design and data flow
- [Security](docs/security.md) — defense-in-depth layers
- [Multi-Tenant Isolation](docs/multi-tenant.md) — namespace isolation model
- [Demo: Operation Claw Shield](docs/DEMO.md) — multi-agent attack simulation
- [Migration from NemoClaw](docs/migration-from-nemoclaw.md) — step-by-step guide
- [Backlog](BACKLOG.md) — current status and roadmap
- [Contributing](CONTRIBUTING.md) — build, test, and PR process

## License

[MIT License](LICENSE) · [Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/)
