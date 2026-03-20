# AzureClaw

> Run AI agents safely. Ship them to production on Azure.

[![License](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/Azure/azureclaw/actions/workflows/ci.yml/badge.svg)](https://github.com/Azure/azureclaw/actions/workflows/ci.yml)

AzureClaw is the open-source runtime for running [OpenClaw](https://openclaw.ai/) AI assistants on Azure — safely, at scale, and connected to the Azure ecosystem from day one. Three commands to go from zero to a production-grade, sandboxed AI agent.

> **Alpha software** — AzureClaw is early-stage. Interfaces may change. We welcome issues and discussion.

---

## What You Get

| Outcome | How it works | You don't need to think about |
|---|---|---|
| **Your agent runs safely** | Every sandbox is isolated by default — network, filesystem, and syscalls are locked down. You define what the agent *can* do, not what it can't. | seccomp, NetworkPolicy, capabilities |
| **Any Azure AI model, one line** | Switch between GPT-4.1, o-series, Phi-4, Llama, Mistral, or 200+ models via Azure AI Foundry. The agent never sees credentials. | Foundry Models, IMDS auth, zero keys |
| **See what your agent is doing** | Every network call, file access, and process spawn is traced. Approve or deny egress requests in real time. | eBPF, Inspektor Gadget, Prometheus |
| **Ship to production** | One cluster, many agents. Namespace isolation. Per-sandbox token budgets. Content Safety + Prompt Shields on every request. | AKS, Azure Linux, Kata VM, NetworkPolicy |

---

## Quick Start

### Install

Works on macOS, Linux, Windows (WSL2). Pick your preferred method:

```bash
# Clone and install locally
git clone https://github.com/Azure/azureclaw.git
cd azureclaw/cli
npm install && npm run build && npm link
```

Prerequisites: Node.js 22+, Azure CLI 2.60+, Docker (for local dev).

### Run

```bash
# First time — configure Azure OpenAI credentials (interactive, secure)
azureclaw onboard

# Start the sandbox
azureclaw dev

# Connect and chat
azureclaw connect dev-agent
```

`azureclaw dev` output:

```
  ╔══════════════════════════════════════════════════╗
  ║           AzureClaw · Local Sandbox              ║
  ║        Secure AI Agent Runtime on Azure          ║
  ╚══════════════════════════════════════════════════╝

  ── Security ──────────────────────────────────────
  ✓ Read-only root filesystem
  ✓ Non-root user (sandbox:1000)
  ✓ All root privileges removed
  ✓ seccomp profile (azureclaw-strict)
  ✓ Writable paths: /sandbox, /tmp only
  ✓ tmpfs /tmp (noexec, 1GB limit)
  ✓ API key mounted as read-only secret (/run/secrets/)

  ── Inference ─────────────────────────────────────
  ✓ Rust inference router (port 8443)
  ✓ All model calls routed through router
  ✓ Token counting + latency metrics enabled

  ── Commands ──────────────────────────────────────
  Connect:  azureclaw connect dev-agent
  Shell:    azureclaw connect dev-agent --shell
  Status:   azureclaw status dev-agent
  Web UI:   http://localhost:18789/#token=...
```

Inside the sandbox, OpenClaw TUI is pre-configured — just start chatting:

```
azureclaw@dev-agent:~$ openclaw tui
> say hello in 3 words
Hello, world, friend!
```

<details>
<summary><strong>Advanced: AKS production setup</strong></summary>

For deploying to AKS (production):

```bash
az login

# Enhanced isolation (default) — runc + custom seccomp
azureclaw up --name my-assistant --model gpt-4.1

# Standard isolation — runc + kernel-default seccomp
azureclaw up --name my-assistant --isolation standard

# Confidential isolation — Kata VM per pod
azureclaw up --name my-assistant --isolation confidential

azureclaw connect my-assistant
```

Prerequisites: Node.js 22+, Azure CLI 2.60+, an Azure subscription.

</details>

### Isolation Levels

AzureClaw provides three isolation levels — choose the right trade-off between performance and security:

| Level | Runtime | Seccomp | Node Pool | Use case |
|---|---|---|---|---|
| **standard** | runc | RuntimeDefault | clawpool | Dev/test, trusted workloads |
| **enhanced** (default) | runc | Localhost (`azureclaw-strict`) | clawpool | Production, general-purpose |
| **confidential** | Kata VM | RuntimeDefault | katapool | Multi-tenant, untrusted code, regulated environments |

- **Standard**: Basic container isolation with the kernel's default syscall filter. Fastest, least overhead.
- **Enhanced**: Custom strict seccomp profile that only allows explicitly listed syscalls. Blocks anything unknown.
- **Confidential**: Each pod runs in its own lightweight VM (Cloud Hypervisor). Kernel-level exploits can't escape. Requires a dedicated Kata node pool.

> **Alpha image access:** AzureClaw sandbox images currently use Azure Linux 3.0 as the container base. AKS nodes run Azure Linux (default AKS node OS). Kata VM isolation requires AKS preview feature registration (`KataVMIsolationPreview`).

---

## The Agent Experience

AzureClaw is designed so that **the agent developer's workflow doesn't change**. You write OpenClaw skills, configure channels, and chat with your assistant exactly as you would on bare metal. The difference is invisible: every request is governed by policy, every model call is authenticated, and the entire environment is hardened beneath you.

### Use any Azure AI model

```bash
azureclaw my-assistant model set gpt-4.1           # Azure OpenAI
azureclaw my-assistant model set Phi-4              # Azure AI Foundry
azureclaw my-assistant model set llama-3.3-70b      # Foundry catalog
```

Model switching is instant — no restart, no credential changes. The inference router handles auth, rate limiting, and content safety transparently.

### Connect to Azure services from inside the sandbox

Agents often need to call Azure services — search an index, read a blob, query a database. AzureClaw makes this seamless:

```yaml
# In your sandbox policy — grant access to specific Azure services
azureServices:
  - service: storage
    account: my-data-lake
    permissions: [read]
  - service: ai-search
    index: product-catalog
    permissions: [search]
  - service: cosmos-db
    database: agent-memory
    permissions: [read, write]
```

Inside the sandbox, the Azure SDKs authenticate automatically via Managed Identity. No keys, no connection strings, no secrets.

```typescript
// Inside the sandbox — this just works
import { BlobServiceClient } from "@azure/storage-blob";
import { DefaultAzureCredential } from "@azure/identity";

const client = new BlobServiceClient(
  "https://mydatalake.blob.core.windows.net",
  new DefaultAzureCredential() // resolved via Workload Identity — zero config
);
```

### Azure AI Foundry integration

AzureClaw is the runtime layer that complements Azure AI Foundry:

| Foundry provides | AzureClaw provides |
|---|---|
| Model catalog (200+ models) | Sandboxed environment to run agents that call those models |
| Prompt Flow / agent orchestration | Isolated execution with policy-governed egress |
| Evaluations (quality, safety, groundedness) | Per-sandbox evaluation metrics streamed to Log Analytics |
| Content Safety + Prompt Shields | Transparent enforcement on every inference call |
| Tracing | Enriched with eBPF-level syscall/network/file traces |

Think of it as: **Foundry is where you build and evaluate your agent. AzureClaw is where you run it safely.**

### See what your agent is doing

```bash
azureclaw my-assistant status          # health, model, tokens used, pending approvals
azureclaw my-assistant logs -f         # stream agent logs
azureclaw my-assistant trace           # live eBPF trace (network, files, processes)
azureclaw my-assistant status          # health, model, tokens used, pending approvals
```

When your agent tries to reach an endpoint not in the policy, you get a real-time prompt:

```
⚠ my-assistant wants to connect to: api.stripe.com:443 (POST /v1/charges)
  Binary: /usr/bin/curl
  [a]pprove  [d]eny  [p]olicy (add permanently)
```

---

## How It Works (short)

```
azureclaw onboard       → configure Azure OpenAI credentials (once)
azureclaw dev            → start local sandbox (Docker + Azure Linux container)
     │
     ▼
┌──────────────────────────────────────┐
│ Docker (Azure Linux 3 container)     │
│                                      │
│  OpenClaw gateway + TUI              │
│  Rust inference router ──────────────┼──▶ Azure OpenAI / AI Foundry
│  seccomp + read-only rootfs          │
│  Prometheus metrics (port 8443)      │
│  Web UI (port 18789)                 │
└──────────────────────────────────────┘

azureclaw up             → same container, AKS nodes (production)
```

Each sandbox is an isolated container. Security is layered and on by default. The Rust inference router handles all model calls — auth, token counting, content safety, and audit logging.

## Key Commands

| Command | What it does |
|---|---|
| `azureclaw onboard` | Configure Azure OpenAI credentials (interactive, secure) |
| `azureclaw dev` | Start a sandboxed agent locally via Docker |
| `azureclaw connect <name>` | Connect and launch OpenClaw TUI |
| `azureclaw connect <name> --shell` | Drop to bash shell inside the sandbox |
| `azureclaw status <name>` | Health, model, security, inference router metrics |
| `azureclaw destroy <name>` | Tear down a sandbox |
| `azureclaw up` | Deploy to AKS (production) |
| `azureclaw model set <name> <model>` | Switch AI model (instant, no restart) |
| `azureclaw trace <name>` | Live eBPF trace (network, files, processes) |
| `azureclaw policy allow <name> <host>` | Add endpoint to network allowlist (hot-reload) |
| `azureclaw policy deny <name> <host>` | Remove endpoint from allowlist (hot-reload) |
| `azureclaw approve --list` | List/approve/deny pending egress requests |

## Security — On by Default

You don't need to be a security expert. AzureClaw ships secure defaults:

- **Network:** Default-deny egress. Only endpoints you list are reachable. The Rust inference router acts as an inference-as-network-policy enforcement point — all model calls are proxied, authenticated, and content-filtered before leaving the sandbox. Agent containers are further restricted by iptables UID-based rules to localhost + DNS only.
- **Filesystem:** Read-only root. Agents can write to `/sandbox` and `/tmp` only.
- **Identity:** No API keys or secrets in the sandbox. Ever. Managed Identity handles auth.
- **Inference:** Every model call goes through Content Safety + Prompt Shields. Configurable, on by default.
- **Node OS:** Azure Linux — auto-patched, SELinux-enforcing.
- **Governance:** Azure Policy for Kubernetes enforces subscription-level constraints (allowed regions, VM sizes, mandatory tags, deny public endpoints).
- **Visibility:** Every syscall, file open, network connection, and DNS lookup is traced via Inspektor Gadget (eBPF).

Want to go further? Add `--isolation confidential` and your workload runs in a Kata VM with its own dedicated kernel.

## Migration from NemoClaw

AzureClaw is compatible with NemoClaw's core UX patterns. See [Migration Guide](docs/migration-from-nemoclaw.md) for details.

## Learn More

- [Plan](PLAN.md) — full project plan, architecture, and phased delivery
- [Architecture](docs/architecture.md) — detailed component design
- [Security](docs/security.md) — defense-in-depth model
- [Demo: Operation Claw Shield](docs/DEMO.md) — multi-agent attack simulation showcasing security layers
- [Multi-Tenant Isolation](docs/multi-tenant.md) — namespace isolation model
- [Migration from NemoClaw](docs/migration-from-nemoclaw.md) — step-by-step guide

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines. AzureClaw welcomes contributions — whether you're fixing bugs, adding features, improving docs, or building new sandbox images.

## License

This project is licensed under the [MIT License](LICENSE).

## Code of Conduct

This project has adopted the [Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/).
