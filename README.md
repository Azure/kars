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
| **Your agent runs safely** | Every sandbox is isolated by default — network, filesystem, and syscalls are locked down. You define what the agent *can* do, not what it can't. | seccomp, SELinux, Cilium, Envoy, NetworkPolicy |
| **Any Azure AI model, one line** | Switch between GPT-4.1, o-series, Phi-4, Llama, Mistral, or 1800+ models. The agent never sees credentials. | Workload Identity, Key Vault, token routing |
| **See what your agent is doing** | Every network call, file access, and process spawn is traced. Approve or deny egress requests in real time. | eBPF, Inspektor Gadget, Log Analytics |
| **Azure services just work** | Your agent can use Azure Storage, Cosmos DB, AI Search, or any Azure service — authenticated via Managed Identity, no keys needed. | Service principals, RBAC bindings, CSI drivers |
| **Ship to production** | One cluster, many agents. Autoscale. Multi-region. 99.95% SLA. Compliance baselines baked into the node OS. | AKS, Azure Container Linux, Azure Linux 4, azure-osconfig |
| **Stays safe over time** | Immutable node OS, auto-patching, signed images, continuous drift remediation. | dm-verity, Notation, SBOM, CIS benchmarks |

---

## Quick Start

### Install

Works on macOS, Linux, Windows (WSL2). Pick your preferred method:

```bash
# npm (recommended — cross-platform, integrity-checked)
npm install -g @azure/azureclaw

# npx (run without installing)
npx @azure/azureclaw up
```

<details>
<summary><strong>Platform-specific package managers</strong></summary>

```bash
# macOS (Homebrew)
brew install azure/azureclaw/azureclaw

# Windows (winget)
winget install Azure.AzureClaw

# Linux (apt — Debian/Ubuntu)
curl -fsSL https://packages.microsoft.com/keys/microsoft.asc | sudo gpg --dearmor -o /usr/share/keyrings/microsoft.gpg
echo "deb [signed-by=/usr/share/keyrings/microsoft.gpg] https://packages.microsoft.com/repos/azureclaw stable main" | sudo tee /etc/apt/sources.list.d/azureclaw.list
sudo apt update && sudo apt install azureclaw

# Linux (dnf — Fedora/RHEL/Azure Linux)
sudo dnf install -y https://packages.microsoft.com/config/azureclaw/azureclaw.rpm
```

</details>

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
azureclaw init --resource-group my-rg --location eastus2
azureclaw onboard
azureclaw up
azureclaw connect my-assistant
```

Prerequisites: Node.js 22+, Azure CLI 2.60+, an Azure subscription.

</details>

> **Alpha image access:** AzureClaw sandbox images are based on Azure Linux 4 Alpha and AKS nodes use Azure Container Linux Alpha — both are limited availability. To gain access:
> - **Azure Linux 4 Alpha:** [AzureLinux4Alpha1 docs](https://eng.ms/docs/products/azure-linux/overview/AzureLinux4Alpha1)
> - **Azure Container Linux Alpha:** [Azure Container Linux Alpha docs](https://dev.azure.com/mariner-org/mariner/_wiki/wikis/Azure%20Container%20Linux%20Plan/6490/Azure-Container-Linux-Alpha)
>
> Contact the respective teams for access to the ACR staging images.

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
| Model catalog (1800+ models) | Sandboxed environment to run agents that call those models |
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
azureclaw my-assistant costs           # compute + inference cost breakdown
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
azureclaw dev            → start local sandbox (Docker + Azure Linux 4)
     │
     ▼
┌──────────────────────────────────────┐
│ Docker (Azure Linux 4 container)     │
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
| `azureclaw costs <name>` | Compute + inference cost breakdown |
| `azureclaw policy set <name> <file>` | Update network/service policy (hot-reload) |

## Security — On by Default

You don't need to be a security expert. AzureClaw ships secure defaults:

- **Network:** Default-deny egress. Only endpoints you list are reachable.
- **Filesystem:** Read-only root. Agents can write to `/sandbox` and `/tmp` only.
- **Identity:** No API keys or secrets in the sandbox. Ever. Managed Identity handles auth.
- **Inference:** Every model call goes through Content Safety + Prompt Shields. Configurable, on by default.
- **Node OS:** Azure Container Linux (AKS) + Azure Linux 4 (container base) — immutable, SELinux-enforcing, CIS-hardened, auto-patched.
- **Visibility:** Every syscall, file open, network connection, and DNS lookup is traced.

Want to go further? Add `--isolation confidential` and your workload runs in a hardware-encrypted TEE (AMD SEV-SNP).

## Migration from NemoClaw

AzureClaw maintains compatibility with NemoClaw's policy format and core UX:

```bash
azureclaw migrate --from-nemoclaw ~/.nemoclaw/blueprints/
```

## Learn More

- [Plan](PLAN.md) — full project plan, architecture, and phased delivery
- [Architecture](docs/architecture.md) — detailed component design
- [Security](docs/security.md) — defense-in-depth model
- [Network Policies](docs/network-policies.md) — egress control reference
- [Inference Providers](docs/inference-providers.md) — model configuration
- [Confidential Containers](docs/confidential-containers.md) — hardware isolation
- [Compliance](docs/compliance.md) — regulatory framework support
- [Migration from NemoClaw](docs/migration-from-nemoclaw.md) — step-by-step guide

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines. AzureClaw welcomes contributions — whether you're fixing bugs, adding features, improving docs, or building new sandbox images.

## License

This project is licensed under the [MIT License](LICENSE).

## Code of Conduct

This project has adopted the [Microsoft Open Source Code of Conduct](https://opensource.microsoft.com/codeofconduct/).
