# AzureClaw — Project Plan

> Azure's enterprise-grade, open-source runtime for running OpenClaw agents safely on Azure Kubernetes Service with Azure Container Linux.

**Status:** Planning
**License:** MIT (open source)
**Repository:** Azure/azureclaw

---

## 1. Executive Summary

AzureClaw is Azure's answer to NVIDIA NemoClaw — an open-source stack that makes it safe, simple, and enterprise-ready to run [OpenClaw](https://openclaw.ai/) AI assistants on Azure infrastructure. Where NemoClaw pairs OpenClaw with NVIDIA's OpenShell runtime and routes inference through NVIDIA cloud, AzureClaw pairs OpenClaw with **Azure Kubernetes Service (AKS)**, **Azure Container Linux (ACL)** as the node OS, and routes inference through **Azure OpenAI / Azure AI Foundry** — while adding multiple layers of security and compliance that go beyond what NemoClaw offers today.

### Why AzureClaw?

| Dimension | NemoClaw (NVIDIA) | AzureClaw (Azure) |
|---|---|---|
| **Runtime** | OpenShell (K3s in Docker) | AKS + Azure Container Linux (production K8s) |
| **Node OS** | Generic container base | Azure Container Linux (node) + Azure Linux 4 (container base) |
| **Inference** | NVIDIA Cloud (Nemotron 3) | Azure OpenAI, Azure AI Foundry (GPT-4o/4.1, o-series, Phi, + 1800 models) |
| **Sandbox isolation** | Container + Landlock + seccomp | seccomp + SELinux (ACL-native) + Confidential Containers add-on |
| **Identity** | API keys only | Managed Identity + Entra ID + Workload Identity Federation |
| **Secrets** | Env vars injected at runtime | Azure Key Vault with CSI driver, auto-rotation |
| **Network policy** | Custom YAML proxy | Azure NPM / Cilium + Azure Firewall + Private Link |
| **Compliance** | Manual | azure-osconfig + Compliance Augmentation Engine (CIS/STIG baselines) — TODO |
| **Observability** | TUI + logs | Azure Monitor + Inspektor Gadget (eBPF) + Log Analytics + Prometheus/Grafana |
| **Deployment** | Single-node Docker | Multi-node AKS, multi-region, autoscale |
| **Scale** | Single-player | Multi-tenant with namespace isolation |
| **Supply chain** | Blueprint digest verification | Notation + ORAS supply chain signing, ACR vulnerability scanning |
| **Cost** | NVIDIA API pricing | Azure Reserved Instances, Spot VMs, per-token billing transparency |

---

## 2. Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           AzureClaw Control Plane                          │
│                                                                             │
│  ┌─────────────┐  ┌──────────────┐  ┌────────────────┐  ┌───────────────┐ │
│  │ azureclaw    │  │  Blueprint    │  │  Policy Engine │  │  Inference    │ │
│  │ CLI          │  │  Controller   │  │  (Admission +  │  │  Router      │ │
│  │ (TypeScript) │  │  (Rust)       │  │   Network)     │  │  (Rust)      │ │
│  └──────┬───────┘  └──────┬───────┘  └───────┬────────┘  └──────┬───────┘ │
│         │                 │                   │                   │         │
└─────────┼─────────────────┼───────────────────┼───────────────────┼─────────┘
          │                 │                   │                   │
          ▼                 ▼                   ▼                   ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                     Azure Kubernetes Service (AKS)                         │
│                   Node OS: Azure Container Linux (ACL)                      │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                    Sandbox Namespace (per-agent)                     │   │
│  │                                                                     │   │
│  │  ┌──────────────┐  ┌───────────────┐  ┌─────────────────────────┐ │   │
│  │  │  OpenClaw     │  │  Sidecar Proxy │  │  Policy Enforcer       │ │   │
│  │  │  Agent Pod    │  │  (Envoy/eBPF)  │  │  (admission webhook)   │ │   │
│  │  │  ┌──────────┐ │  │               │  │                         │ │   │
│  │  │  │ OpenClaw  │ │  │  L7 egress    │  │  - Network Policy      │ │   │
│  │  │  │ Gateway   │ │  │  filtering    │  │  - Filesystem Policy   │ │   │
│  │  │  │ + Agent   │ │  │  TLS inspect  │  │  - Process Policy      │ │   │
│  │  │  └──────────┘ │  │               │  │  - Inference Policy    │ │   │
│  │  │               │  │               │  │                         │ │   │
│  │  │  seccomp +    │  │               │  │                         │ │   │
│  │  │  SELinux +     │  │               │  │                         │ │   │
│  │  │  read-only    │  │               │  │                         │ │   │
│  │  │  rootfs       │  │               │  │                         │ │   │
│  │  └──────────────┘  └───────────────┘  └─────────────────────────┘ │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                    Platform Services Namespace                      │   │
│  │                                                                     │   │
│  │  ┌───────────────┐  ┌──────────────┐  ┌───────────────────────┐   │   │
│  │  │  Blueprint     │  │  Approval     │  │  Metrics / Logs      │   │   │
│  │  │  Controller    │  │  Controller   │  │  Collector           │   │   │
│  │  └───────────────┘  └──────────────┘  └───────────────────────┘   │   │
│  │                                                                     │   │
│  │  ┌───────────────────────────────────────────────────────────────┐ │   │
│  │  │  Inspektor Gadget DaemonSet (eBPF tracing on ACL nodes)      │ │   │
│  │  └───────────────────────────────────────────────────────────────┘ │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘
          │                    │                    │
          ▼                    ▼                    ▼
┌──────────────┐  ┌────────────────────┐  ┌─────────────────────┐
│ Azure OpenAI │  │ Azure Key Vault    │  │ Azure Monitor       │
│ / AI Foundry │  │ (secrets, certs)   │  │ + Log Analytics     │
└──────────────┘  └────────────────────┘  └─────────────────────┘
```

---

## 3. Component Deep Dive

### 3.1 AzureClaw CLI (`azureclaw`)

**Language:** TypeScript (matching NemoClaw's approach, maximizing OpenClaw ecosystem compatibility)

The CLI is the primary user entrypoint. It mirrors NemoClaw's UX while adding Azure-specific capabilities.

```
azureclaw/
├── src/
│   ├── index.ts                     CLI entry point
│   ├── cli.ts                       Commander.js subcommand wiring
│   ├── commands/
│   │   ├── init.ts                  Initialize AKS cluster + ACL node pool
│   │   ├── onboard.ts               Interactive setup wizard (NemoClaw-compatible)
│   │   ├── launch.ts                Create sandboxed OpenClaw agent
│   │   ├── connect.ts               Interactive shell into sandbox pod
│   │   ├── status.ts                Health check across all components
│   │   ├── logs.ts                  Stream agent and platform logs
│   │   ├── policy.ts                Manage network/filesystem/inference policies
│   │   ├── approve.ts               Approve/deny pending network requests
│   │   ├── deploy.ts                Deploy to AKS (wraps azd/kubectl)
│   │   └── destroy.ts               Teardown with confirmation
│   ├── blueprint/
│   │   ├── resolve.ts               Blueprint version resolution from ACR
│   │   ├── verify.ts                Notation signature + digest verification
│   │   ├── plan.ts                  Resource planning (AKS, ACR, KV, AOAI)
│   │   └── apply.ts                 Apply blueprint via Helm/kubectl
│   ├── azure/
│   │   ├── identity.ts              Managed Identity + Workload Identity setup
│   │   ├── keyvault.ts              Key Vault integration
│   │   ├── openai.ts                Azure OpenAI / AI Foundry connection
│   │   └── monitor.ts               Azure Monitor integration
│   └── tui/
│       └── terminal.ts              Rich TUI (ink/blessed) for approval flow
├── openclaw.plugin.json             OpenClaw plugin manifest
└── package.json
```

**Key commands:**

| Command | Description |
|---------|-------------|
| `azureclaw init` | Provision AKS cluster with ACL node pools, set up Azure resources |
| `azureclaw onboard` | Guided wizard: Azure login, model selection, policy, sandbox creation |
| `azureclaw <name> launch` | Create a new sandboxed OpenClaw agent |
| `azureclaw <name> connect` | Shell into a running sandbox |
| `azureclaw <name> status` | Show sandbox health, policy state, inference config |
| `azureclaw <name> logs` | Stream agent and platform logs |
| `azureclaw <name> policy set` | Apply/update network policy (hot-reload) |
| `azureclaw <name> approve` | Approve a pending egress request |
| `azureclaw <name> destroy` | Teardown sandbox with confirmation |
| `azureclaw deploy` | Deploy full stack to AKS (CI/CD friendly) |

### 3.2 Blueprint Controller

**Language:** Rust (via [kube-rs](https://kube.rs/) — CNCF Sandbox, v3.1, 5.8k dependents)

The blueprint controller is a Kubernetes operator that manages the lifecycle of sandboxed OpenClaw instances. It replaces NemoClaw's Python blueprint with a cloud-native CRD-based approach, built in Rust using kube-rs.

**Custom Resource Definitions:**

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: my-assistant
  namespace: azureclaw-sandboxes
spec:
  # OpenClaw configuration
  openclaw:
    version: "2026.3.13"
    image: azureclaw.azurecr.io/openclaw-sandbox:latest
    config:
      model: "azure/gpt-4.1"
      channels:
        telegram:
          enabled: true
        slack:
          enabled: true

  # Sandbox security
  sandbox:
    isolation: "confidential"          # standard | enhanced | confidential
    seccompProfile: "azureclaw-strict"
    selinuxContext: "azureclaw_sandbox_t"
    readOnlyRootFilesystem: true
    runAsNonRoot: true
    allowPrivilegeEscalation: false
    writablePaths:
      - /sandbox
      - /tmp

  # Inference routing
  inference:
    provider: "azure-openai"
    endpoint: "https://my-aoai.openai.azure.com/"
    model: "gpt-4.1"
    fallback:
      provider: "azure-ai-foundry"
      model: "Phi-4"

  # Network policy
  networkPolicy:
    defaultDeny: true
    allowedEndpoints:
      - host: "api.github.com"
        port: 443
        methods: ["GET"]
        paths: ["/repos/**"]
      - host: "clawhub.com"
        port: 443
        methods: ["GET", "POST"]
    approvalRequired: true             # Block unknown egress, require operator approval

  # Resource limits
  resources:
    requests:
      cpu: "500m"
      memory: "1Gi"
    limits:
      cpu: "2"
      memory: "4Gi"
```

### 3.3 Policy Engine

The policy engine is a key differentiator. It combines multiple enforcement layers:

#### Layer 1: Kubernetes Network Policy (L3/L4)
- Azure NPM or Cilium-based network policies
- Default-deny egress per sandbox namespace
- Allowlist-only outbound

#### Layer 2: Envoy Sidecar Proxy (L7)
- Full HTTP method + path filtering (like NemoClaw but richer)
- TLS origination and inspection
- Request/response logging
- Rate limiting per endpoint
- Request body size limits

#### Layer 3: Admission Webhook (preventive)
- Validates all pod specs against security baseline
- Blocks privilege escalation, host mounts, capabilities
- Enforces image allowlists (only signed images from ACR)
- Integrates with Azure Policy for Kubernetes

#### Layer 4: Kernel-level (defense in depth)
- **seccomp:** Strict syscall filtering (deny-by-default profile)
- **SELinux:** Mandatory access control via ACL's enforcing SELinux policy (type `azureclaw_sandbox_t`)
- **Read-only rootfs:** Writable only to /sandbox and /tmp via emptyDir
- **Non-root:** All containers run as non-root user
- **No new capabilities:** `allowPrivilegeEscalation: false`

#### Layer 5: Confidential Containers (optional add-on)
- **AMD SEV-SNP** or **Intel TDX** memory encryption
- Workload attestation
- Hardware-rooted trust
- *Optional add-on for customers who require TEE-level isolation — not the default, as it may be overkill for many use cases*
- *However, given NemoClaw's positioning around TEE, we maintain parity by supporting it*

### 3.4 Inference Router

Routes all agent LLM calls through a controlled gateway — never direct from the sandbox.

```
Agent (sandbox pod) ──▶ Envoy sidecar ──▶ Inference Router ──▶ Azure OpenAI
                                                             ──▶ Azure AI Foundry
                                                             ──▶ Self-hosted (Ollama on AKS)
```

**Features:**
- **Managed Identity auth** — no API keys in the sandbox; the inference router authenticates with Azure OpenAI using Workload Identity
- **Model routing** — declarative model selection per sandbox, with fallback chains
- **Token budgets** — per-sandbox token limits with alerts
- **Content safety** — Azure AI Content Safety integration for input/output filtering
- **Prompt shields** — Azure Prompt Shields for jailbreak/injection detection
- **Audit logging** — every inference call logged to Azure Monitor

### 3.5 Azure Linux OS Strategy

AzureClaw uses two related Microsoft Linux distributions — one for the **container base image** and one for the **AKS node OS**:

| Layer | Distro | Where | Purpose |
|-------|--------|-------|---------|
| **Container image** | **Azure Linux 4** | Sandbox Dockerfile base | The OS inside the sandbox container. Agent code runs here. |
| **Node OS** | **Azure Container Linux (ACL)** | AKS node pool osSKU | The host OS on AKS nodes. Immutable, minimal, purpose-built for containers. |

Both are Microsoft-maintained, SELinux-enforcing, and CIS-hardened. They share the same package ecosystem (tdnf/rpm) and security posture.

#### Azure Linux 4 (container base image)

Azure Linux 4 Alpha is available via ACR (limited availability — alpha access required):
```
azlpubstagingacroxz2o4gw.azurecr.io/azurelinux/base/core:4.0
```

> **Alpha access:** See [AzureLinux4Alpha1 docs](https://eng.ms/docs/products/azure-linux/overview/AzureLinux4Alpha1) to request access to the staging ACR.

This is the base for all AzureClaw sandbox images. Using Azure Linux 4 instead of Debian/Ubuntu means:
- **Same OS family top to bottom** — the container and the host OS are both Azure Linux. No Debian-on-Mariner mismatches.
- **Local dev parity** — `azureclaw dev` runs the same Azure Linux 4 container locally via Docker. When you flip to AKS, the sandbox image is identical.
- **tdnf package manager** — lightweight, RPM-based, no apt. Minimal attack surface.
- **SELinux-aware** — policy modules work the same in Docker and on AKS.

The sandbox Dockerfile uses a build-arg so the base image can be overridden:
```dockerfile
ARG AZURELINUX_BASE=azlpubstagingacroxz2o4gw.azurecr.io/azurelinux/base/core:4.0
FROM ${AZURELINUX_BASE} AS builder
```

#### Azure Container Linux (AKS node OS)

ACL is the node OS for AKS — a minimal, immutable, container-optimized variant:
- **Minimal attack surface:** No package manager on the host, no shell, no unnecessary services
- **Immutable OS:** Read-only root filesystem, updates via image swap (A/B partition)
- **Fast boot:** ~2s boot time, critical for scaling sandbox pods quickly
- **Automatic security updates:** Unattended, image-based OS updates
- **CIS hardened:** Default CIS L1 benchmark compliance
- **SELinux enforcing:** Mandatory access control on the host
- **dm-verity:** Verified boot chain

#### Local → AKS Workflow

The local-first development story is a key UX win:

```
azureclaw dev                          # Docker: Azure Linux 4 container locally
    │   (iterate, test, debug)
    ▼
azureclaw deploy                       # AKS: same Azure Linux 4 container, ACL nodes
```

Both environments use:
- Same Azure Linux 4 base image
- Same seccomp profile
- Same SELinux policy
- Same network policy rules (simulated in Docker, enforced by Cilium on AKS)
- Same inference routing (local: via `az login` creds; AKS: via Managed Identity)

The only difference is the node OS: Docker's host kernel locally vs ACL on AKS. Since ACL provides additional hardening (immutable rootfs, verified boot, CIS baseline via azure-osconfig), AKS is strictly more secure — but the sandbox behavior is identical.

**AzureClaw node pool configuration:**
```yaml
agentPoolProfiles:
  - name: clawpool
    osType: Linux
    osSKU: AzureContainerLinux      # ACL Alpha (fallback: AzureLinux for AzL4)
    mode: User
    vmSize: Standard_D4s_v5         # or Standard_DC4as_v5 for confidential
    enableEncryptionAtHost: true
    enableFIPS: true                 # FIPS 140-2 validated crypto
    kubeletConfig:
      seccompDefault: true           # default seccomp for all pods
    linuxProfile:
      sysctls:
        netCoreDefaultQdisc: "fq"
```

> **Note:** If ACL is not yet GA, the AKS node pool can use `osSKU: AzureLinux` (Azure Linux 3/4) as a fallback. The container images are the same either way — the node OS just provides additional host-level hardening.
>
> **Alpha access for AKS node OS:** See [Azure Container Linux Alpha docs](https://dev.azure.com/mariner-org/mariner/_wiki/wikis/Azure%20Container%20Linux%20Plan/6490/Azure-Container-Linux-Alpha) to request access to ACL node pools.

---

## 4. Security Architecture — The Key Differentiator

AzureClaw's security story is the primary competitive advantage over NemoClaw.

### 4.1 Defense in Depth Model

```
┌─────────────────────────────────────────────────────────────┐
│ Layer 8: Azure AI Content Safety + Prompt Shields           │  ← Inference safety
├─────────────────────────────────────────────────────────────┤
│ Layer 7: Inspektor Gadget (eBPF tracing + anomaly detect)   │  ← Runtime observability
├─────────────────────────────────────────────────────────────┤
│ Layer 6: Envoy L7 proxy (method/path/header filtering)      │  ← Application firewall
├─────────────────────────────────────────────────────────────┤
│ Layer 5: Kubernetes NetworkPolicy (Cilium/Azure NPM)        │  ← Network segmentation
├─────────────────────────────────────────────────────────────┤
│ Layer 4: SELinux + seccomp (kernel-level confinement)        │  ← OS-level sandboxing
├─────────────────────────────────────────────────────────────┤
│ Layer 3: Read-only rootfs + non-root + no-new-privileges    │  ← Container hardening
├─────────────────────────────────────────────────────────────┤
│ Layer 2: Confidential Containers (SEV-SNP / TDX) [add-on]   │  ← Hardware isolation
├─────────────────────────────────────────────────────────────┤
│ Layer 1: Azure Container Linux (immutable, CIS, SELinux)     │  ← Host OS hardening
│          + azure-osconfig (CIS/STIG baseline enforcement)    │     [TODO]
├─────────────────────────────────────────────────────────────┤
│ Layer 0: Azure infrastructure (DDoS, Firewall, NSG)          │  ← Cloud perimeter
└─────────────────────────────────────────────────────────────┘
```

### 4.2 Supply Chain Security

| Feature | NemoClaw | AzureClaw |
|---------|----------|-----------|
| Image signing | Digest verification | Notation + Ratify (CNCF standard) |
| Image registry | GHCR | Azure Container Registry with geo-replication |
| Vulnerability scanning | None built-in | ACR vulnerability scanning + SBOM |
| SBOM | None | Automatic SBOM generation (SPDX) attached to images |
| Admission control | None | Azure Policy + Ratify (only verified images admitted) |
| Base image | Community Docker | Azure Linux 4 (Microsoft-maintained) |

### 4.3 Identity & Access

- **Zero standing credentials:** No API keys baked into images or injected as env vars
- **Workload Identity:** Pods authenticate to Azure services via federated OIDC tokens
- **Key Vault CSI:** Secrets mounted as volumes, auto-rotated, never in etcd
- **Entra ID RBAC:** Operator access controlled through Azure Entra ID groups
- **Audit trail:** Every `kubectl exec`, policy change, and approval logged to Azure Monitor

### 4.4 Compliance (TODO — azure-osconfig integration)

Compliance will be powered by **azure-osconfig** + the **Compliance Augmentation Engine** rather than Defender for Cloud. This gives us:

- **azure-osconfig daemon** on ACL nodes — lightweight, open-source security configuration agent with declarative desired-state model
- **ComplianceEngine module** — 40+ typed audit/remediation procedures (EnsureSshdOption, EnsureSysctl, EnsureFilePermissions, etc.)
- **CIS AKS Optimized Azure Linux benchmark** — purpose-built CIS benchmark for AKS nodes running Azure Linux, processed by the Augmentation Engine
- **DISA STIG support** — 400+ rules per distro, validated against DISA's official SCC scanner
- **Multi-authority management** — Azure Policy + GitOps + local files (works in air-gapped / sovereign scenarios)
- **Automated remediation** — declarative desired-state model continuously reconciles drift

> **Status:** TODO — requires integration work with the azure-osconfig team and Augmentation Engine pipeline.
> See: `/Security Baseline/azure-osconfig/` and `/Security Baseline/azcorelinux-Compliance-AugmentationEngine/`

| Capability | Defender for Cloud (rejected) | azure-osconfig (planned) |
|---|---|---|
| Benchmark support | Azure Security Baseline only | CIS (20+ distros) + DISA STIGs + CIS AKS Optimized |
| Runtime agent | MDE / Guest Configuration (proprietary) | `osconfig` daemon (MIT-licensed, open source) |
| AKS node support | Limited VM-level only | Purpose-built CIS AKS Optimized Azure Linux benchmark |
| Air-gap / sovereign | Requires cloud connectivity | GitOps + local files work fully offline |
| Remediation | Limited auto-remediation | Full declarative desired-state remediation |
| Validation | Trust vendor assessment | SCC comparison tooling for audit/accreditation |

---

## 5. Azure Integration Philosophy — The Key Differentiator

The single most important thing AzureClaw must get right is **making Azure services feel invisible inside the sandbox**. The agent developer should never think about authentication, networking, or credential management when calling Azure services. This is what separates AzureClaw from NemoClaw: not a list of technologies, but a seamless experience where the entire Azure ecosystem is available to sandboxed agents without compromising security.

### 5.1 Design Principle: Zero-Config Azure

Every Azure service the agent touches should work through this pattern:

1. **Operator declares** which Azure services a sandbox can access (in the ClawSandbox CRD or via `azureclaw policy`)
2. **AzureClaw provisions** Workload Identity federation + RBAC bindings automatically
3. **Inside the sandbox**, `DefaultAzureCredential()` just works — no keys, no connection strings, no env vars
4. **The policy engine** enforces access boundaries — if the agent tries to reach a service not in the policy, the request is blocked and surfaced for approval

```yaml
# Operator-facing: simple, declarative
spec:
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
    - service: ai-foundry
      permissions: [inference, evaluate]
```

```typescript
// Agent-facing: standard Azure SDK, zero config
const client = new BlobServiceClient(url, new DefaultAzureCredential());
// Auth resolved via Workload Identity → Managed Identity → RBAC. No secrets.
```

### 5.2 Azure AI Foundry Integration

AzureClaw is the **runtime complement** to Azure AI Foundry. Foundry is where you build and evaluate agents; AzureClaw is where you run them safely.

| What the agent developer wants | Foundry provides | AzureClaw provides |
|---|---|---|
| Use any model | 1800+ model catalog | Inference router with instant model switching, zero-credential auth |
| Keep it safe | Content Safety, Prompt Shields | Transparent enforcement on every call (on by default) |
| Know if it's working | Evaluations (quality, safety, groundedness) | Per-sandbox eval metrics → Log Analytics, trend dashboards |
| Debug it | Tracing | Enriched traces: model call + eBPF syscall/network/file context |
| Orchestrate complex flows | Prompt Flow, Semantic Kernel | Sandboxed execution of flows with policy-governed egress |
| Scale it | - | Multi-agent AKS with autoscale, multi-region |

**Instant model switching** (no restart):
```bash
azureclaw my-assistant model set gpt-4.1        # Azure OpenAI
azureclaw my-assistant model set Phi-4           # Foundry catalog
azureclaw my-assistant model set llama-3.3-70b   # Foundry catalog
```

### 5.3 Azure Service Connectors (auto-provisioned)

When a sandbox policy references an Azure service, AzureClaw automatically:

1. Creates a scoped Managed Identity for the sandbox pod
2. Assigns minimum-privilege RBAC roles (e.g., `Storage Blob Data Reader`)
3. Configures Workload Identity Federation between the pod's service account and the Managed Identity
4. Adds the service endpoint to the sandbox's network policy allowlist
5. Exposes connection details as environment variables (endpoint URL only — no secrets)

Supported Azure services:

| Service | Policy key | Auto-provisioned RBAC |
|---|---|---|
| Azure OpenAI | `ai-openai` | Cognitive Services OpenAI User |
| Azure AI Foundry | `ai-foundry` | Azure AI Developer |
| Azure AI Search | `ai-search` | Search Index Data Reader |
| Azure Storage (Blob) | `storage` | Storage Blob Data Reader/Contributor |
| Azure Cosmos DB | `cosmos-db` | Cosmos DB Data Reader/Contributor |
| Azure Key Vault | `keyvault` | Key Vault Secrets User |
| Azure Service Bus | `service-bus` | Service Bus Data Receiver/Sender |
| Azure Event Hubs | `event-hubs` | Event Hubs Data Receiver/Sender |
| Azure SQL | `sql` | db_datareader / db_datawriter |

### 5.4 Agent Evaluation Pipeline

- Built-in evaluation using Azure AI Foundry evaluations
- Measure quality, safety, groundedness per sandbox
- Trend analysis over time via Log Analytics

### 5.5 Content Safety (on by default)

- Azure AI Content Safety for every inference request
- Prompt Shields for jailbreak/injection detection
- Groundedness detection to prevent hallucination
- Custom content filters per sandbox
- **On by default** — no configuration needed, opt-out per sandbox if necessary

---

## 6. Observability

| Feature | Implementation |
|---------|---------------|
| Agent logs | Streamed to Azure Log Analytics via Container Insights |
| Inference metrics | Token usage, latency, model, per-sandbox — custom metrics |
| Network audit | Every allowed/denied egress request logged |
| **Runtime security observability** | **Inspektor Gadget (eBPF)** — syscall tracing, network flow, DNS, file access, process events |
| Policy changes | Azure Activity Log + admission webhook audit |
| Cost tracking | Per-sandbox cost attribution (compute + inference) |
| Dashboards | Pre-built Azure Workbook + Grafana dashboards |
| Alerts | Azure Monitor alerts for anomalies (token spikes, egress bursts) |
| TUI | Real-time terminal UI (matching NemoClaw's `openshell term`) |

### 6.1 Inspektor Gadget Integration

[Inspektor Gadget](https://www.inspektor-gadget.io/) is a CNCF project (now a Microsoft/Kinvolk project) that provides eBPF-powered observability for Kubernetes. It's a natural fit for AzureClaw:

- **Syscall tracing** — see exactly what syscalls each sandbox pod makes, validate seccomp profiles
- **Network flow monitoring** — real-time TCP/UDP/DNS visibility per pod, complement to NetworkPolicy audit
- **File access tracing** — verify SELinux/filesystem policies are enforced, detect unexpected writes
- **Process lifecycle** — track process creation inside sandboxes, detect unexpected binaries
- **Container escape detection** — monitor for mount namespace breakouts, capability escalation
- **DNS snooping** — see what domains agents resolve, catch policy-bypassing attempts

Inspektor Gadget runs as a DaemonSet on ACL nodes and exposes data via:
- `kubectl gadget` CLI (for operators)
- Prometheus metrics (for dashboards)
- JSON event stream (for Azure Monitor / Log Analytics integration)
- AzureClaw TUI (real-time view during approval flows)

Key gadgets for AzureClaw:

| Gadget | Use Case |
|--------|----------|
| `trace exec` | Monitor process execution inside sandboxes |
| `trace open` | Track file opens — validate filesystem policy |
| `trace tcp` / `trace dns` | Network observability beyond L3/L4 policy |
| `trace signal` | Detect kill/signal abuse between processes |
| `trace mount` | Detect mount attempts (should be blocked) |
| `snapshot process` | Point-in-time view of running processes per sandbox |
| `top file` / `top tcp` | Real-time resource consumption per pod |

---

## 7. Project Structure

```
azureclaw/
├── PLAN.md                          This document
├── README.md                        Project overview + quickstart
├── LICENSE                          MIT
├── SECURITY.md                      Security policy
├── CONTRIBUTING.md                  Contribution guide
├── CODE_OF_CONDUCT.md               Code of conduct
│
├── cli/                             AzureClaw CLI (TypeScript)
│   ├── src/
│   │   ├── index.ts
│   │   ├── cli.ts
│   │   ├── commands/
│   │   ├── blueprint/
│   │   ├── azure/
│   │   └── tui/
│   ├── package.json
│   └── tsconfig.json
│
├── controller/                      Blueprint Controller (Rust, kube-rs)
│   ├── src/
│   │   ├── main.rs
│   │   ├── crd.rs
│   │   └── reconciler.rs
│   └── Cargo.toml
│
├── inference-router/                Inference Router (Rust, axum)
│   ├── src/
│   │   ├── main.rs
│   │   ├── auth.rs
│   │   ├── config.rs
│   │   ├── proxy.rs
│   │   ├── routes.rs
│   │   ├── safety.rs
│   │   └── metrics.rs
│   └── Cargo.toml
│
├── policy-engine/                   Policy definitions + enforcement
│   ├── profiles/
│   │   ├── seccomp/
│   │   │   └── azureclaw-strict.json
│   │   └── selinux/
│   │       └── azureclaw_sandbox.te
│   ├── network/
│   │   └── baseline.yaml
│   └── admission/
│       └── webhook.rs
│
├── sandbox-images/                  Container images for sandboxes
│   ├── base/
│   │   └── Dockerfile               Based on Azure Linux 4
│   ├── openclaw/
│   │   └── Dockerfile               OpenClaw pre-installed
│   └── openclaw-browser/
│       └── Dockerfile                OpenClaw + headless browser
│
├── blueprints/                      Versioned deployment blueprints
│   ├── blueprint.yaml                Manifest
│   ├── default/
│   │   └── values.yaml              Default sandbox config
│   └── enterprise/
│       └── values.yaml              Enterprise config (confidential containers)
│
├── deploy/                          Infrastructure as Code
│   ├── bicep/
│   │   ├── main.bicep                AKS + ACR + KV + AOAI + Monitor
│   │   ├── modules/
│   │   │   ├── aks.bicep
│   │   │   ├── acr.bicep
│   │   │   ├── keyvault.bicep
│   │   │   ├── openai.bicep
│   │   │   ├── monitor.bicep
│   │   │   └── network.bicep
│   │   └── parameters/
│   │       ├── dev.bicepparam
│   │       ├── staging.bicepparam
│   │       └── production.bicepparam
│   ├── helm/
│   │   └── azureclaw/
│   │       ├── Chart.yaml
│   │       ├── values.yaml
│   │       └── templates/
│   │           ├── namespace.yaml
│   │           ├── controller.yaml
│   │           ├── inference-router.yaml
│   │           ├── network-policies.yaml
│   │           ├── rbac.yaml
│   │           └── monitoring.yaml
│   └── azure.yaml                   azd configuration
│
├── docs/                            Documentation
│   ├── architecture.md
│   ├── quickstart.md
│   ├── security.md
│   ├── network-policies.md
│   ├── inference-providers.md
│   ├── confidential-containers.md
│   ├── compliance.md
│   ├── troubleshooting.md
│   └── migration-from-nemoclaw.md
│
├── examples/                        Example configurations
│   ├── basic-agent/
│   ├── multi-channel-agent/
│   ├── confidential-agent/
│   └── enterprise-multi-tenant/
│
├── test/                            Tests
│   ├── unit/
│   ├── integration/
│   └── e2e/
│
└── .github/
    └── workflows/
        ├── ci.yml
        ├── release.yml
        └── security-scan.yml
```

---

## 8. Phased Delivery

### Phase 1: Foundation (Weeks 1–4)
- [ ] CLI scaffold with `init`, `onboard`, `launch`, `connect`, `status`, `logs`
- [ ] Sandbox container images (base + OpenClaw)
- [ ] Bicep IaC for AKS + ACR + Key Vault + Azure OpenAI
- [ ] Basic Helm chart for sandbox deployment
- [ ] seccomp + read-only rootfs + non-root enforcement
- [ ] Inference router with Azure OpenAI (Managed Identity)
- [ ] Basic network policy (default-deny + allowlist)
- [ ] README + quickstart documentation
- [ ] CI/CD pipeline (build, test, scan)

### Phase 2: Security Hardening (Weeks 5–8)
- [ ] SELinux policy modules for sandbox pods (leveraging ACL's enforcing SELinux)
- [ ] Envoy sidecar with L7 egress filtering
- [ ] Notation image signing + Ratify admission
- [ ] Azure Policy for Kubernetes integration
- [ ] Key Vault CSI driver for secrets
- [ ] Operator approval flow (TUI + API)
- [ ] Inspektor Gadget DaemonSet deployment + integration with TUI
- [ ] SBOM generation in CI

### Phase 3: Enterprise Features (Weeks 9–12)
- [ ] Blueprint Controller (CRD-based operator)
- [ ] Confidential Containers support as optional add-on (SEV-SNP)
- [ ] Multi-tenant namespace isolation
- [ ] Azure AI Content Safety integration
- [ ] Prompt Shields integration
- [ ] Token budgets + cost tracking
- [ ] Azure Monitor dashboards + workbooks + Inspektor Gadget metrics
- [ ] Alerting (token spikes, egress anomalies, syscall anomalies)
- [ ] Hot-reload policy updates
- [ ] TODO: azure-osconfig integration planning (CIS AKS Optimized baseline for ACL nodes)

### Phase 4: Ecosystem & Polish (Weeks 13–16)
- [ ] Azure AI Foundry model catalog integration
- [ ] Migration guide from NemoClaw
- [ ] Multi-region deployment
- [ ] Azure Linux 4 container image validation + Azure Container Linux AKS node pool validation
- [ ] Comprehensive e2e test suite
- [ ] Enterprise example (multi-tenant, compliance)
- [ ] Community sandbox catalog (BYOC)
- [ ] `azd` integration for one-click deployment
- [ ] Public documentation site

---

## 9. Key Design Decisions

### 9.1 Why AKS instead of K3s-in-Docker?
NemoClaw/OpenShell runs K3s inside a single Docker container. This is simple but:
- No real multi-node scaling
- No hardware isolation (confidential containers need real VM isolation)
- No Azure integrations (identity, Key Vault, policy)
- Not enterprise-grade (no SLA, no managed control plane)

AKS gives us production Kubernetes with a 99.95% SLA, built-in Azure integrations, and the ability to use Confidential Containers for hardware-level isolation.

### 9.2 Why Azure Container Linux?
ACL is Microsoft's hardened, minimal, container-optimized OS. It's the ideal node OS because:
- Minimal attack surface reduces the blast radius if a sandbox escape occurs
- Immutable root FS means compromised nodes can't persist malware
- Verified boot ensures the host hasn't been tampered with
- SELinux enforcing provides mandatory access control
- Fast boot (~2s) means nodes can scale quickly
- Automatic updates keep the OS patched without operator intervention

### 9.3 Why Rust-first?

AzureClaw follows a **Rust-first policy**: all new systems components are written in Rust unless there's a compelling justification to use another language.

**Why:**
- **Memory safety without GC** — critical for security-focused infrastructure. No use-after-free, no buffer overflows, no data races.
- **Performance** — the inference router is the hot path (every model call). Rust gives us zero-copy proxying, minimal latency, and small binary size.
- **Microsoft alignment** — Microsoft is one of the largest Rust adopters. Azure, Windows, and the Rust Foundation all have Microsoft investment.
- **OpenShell parity** — NVIDIA's OpenShell is 87.6% Rust. Matching their language choice signals equivalent engineering rigor.
- **kube-rs maturity** — the Rust Kubernetes ecosystem (kube-rs) is a CNCF Sandbox project, at v3.1, with 5.8k dependents and 163 contributors. It's production-ready.
- **Single language for systems** — both the controller and inference router are Rust. One toolchain, one CI pipeline, shared crate dependencies.

**Language map:**

| Component | Language | Justification |
|---|---|---|
| Inference Router | Rust (axum) | Performance-critical proxy, memory safety |
| Blueprint Controller | Rust (kube-rs) | K8s operator, CRD derive macros, same toolchain |
| CLI | TypeScript | OpenClaw ecosystem compatibility, agent developer UX |
| Policy profiles | Declarative (JSON/YAML/TE) | Not code — configuration |
| IaC | Bicep + Helm | Azure-native, Kubernetes-native |

### 9.4 Why Confidential Containers as an optional add-on?
NemoClaw's isolation is container + Landlock + seccomp. This is good but all in the same trust boundary as the host. Confidential Containers (via Kata + SEV-SNP/TDX) run workloads in a hardware-encrypted Trusted Execution Environment — even the cloud operator can't read the agent's memory.

However, Confidential Containers add complexity and cost (DC-series VMs, larger startup time). For many use cases, the combination of Azure Container Linux (immutable, SELinux-enforcing, CIS-hardened) + seccomp + network policy is sufficient. We offer Confidential Containers as an **optional add-on** (`--isolation confidential`) for customers who need TEE-level guarantees — regulated industries, government workloads, or scenarios where the agent processes highly sensitive data.

Given NemoClaw's TEE narrative, we maintain support to ensure competitive parity.

### 9.5 Why not fork OpenShell?
OpenShell is a Rust application that embeds K3s. It's well-designed for single-developer use but fundamentally different from what we need (managed AKS, Azure integrations). It's cleaner to build AzureClaw as a Kubernetes-native stack from the start rather than retrofitting OpenShell.

---

## 10. Migration Path from NemoClaw

AzureClaw will support migration from NemoClaw:

1. **Policy compatibility:** AzureClaw network policies use the same YAML schema as NemoClaw's `openclaw-sandbox.yaml`, with Azure-specific extensions
2. **CLI parity:** Core commands (`onboard`, `connect`, `status`, `logs`) have the same semantics
3. **OpenClaw config:** `openclaw.json` / workspace configuration is preserved as-is
4. **Blueprint bridge:** A CLI migration command converts NemoClaw blueprints to AzureClaw CRDs

```bash
# Migrate from NemoClaw to AzureClaw
azureclaw migrate --from-nemoclaw ~/.nemoclaw/blueprints/
```

---

## 11. Open Questions

1. **ACL Alpha / Azure Linux 4 Alpha:** Both are in preview (limited availability). Azure Linux 4 container base is available via ACR (`azlpubstagingacroxz2o4gw.azurecr.io/azurelinux/base/core:4.0`). ACL for AKS nodes may need `AzureLinux` osSKU as fallback until GA. Alpha testers must request access — see [AzureLinux4Alpha1](https://eng.ms/docs/products/azure-linux/overview/AzureLinux4Alpha1) and [Azure Container Linux Alpha](https://dev.azure.com/mariner-org/mariner/_wiki/wikis/Azure%20Container%20Linux%20Plan/6490/Azure-Container-Linux-Alpha).
2. **OpenClaw plugin registration:** Should AzureClaw register as an OpenClaw plugin (like NemoClaw does) or operate as a standalone orchestrator?
3. **GPU support:** Do we want GPU node pools for local inference (Ollama on AKS)? What's the cost model?
4. **Browser sandboxing:** OpenClaw has browser control — how do we sandbox headless Chrome in Confidential Containers?
5. **Channel bridging:** For always-on agents, do we need a persistent ingress (Telegram webhook, etc.) at the AKS level?
6. **Naming:** Is "AzureClaw" the final name or do we want something more... Azure-y? (AzureShell? Azure Agent Shell? Azure Claw Guard?)

---

## 12. Usability Audit — Agent Developer Perspective

This section captures a critical-eye review of the AzureClaw design from the perspective of someone who builds agentic AI systems for a living. The goal is to ensure that **security doesn't come at the cost of usability**, and that AzureClaw feels smooth rather than burdensome.

### 12.1 Problems Identified & Mitigations

| Problem | Severity | Mitigation |
|---------|----------|------------|
| **Too many commands to get started.** `az login` → `azureclaw init` → `azureclaw onboard` → `azureclaw connect` is 4 steps. NemoClaw is 1 (`curl ... \| bash`). | High | Add `azureclaw up` — one command that does everything. Init + onboard + launch + connect. Sensible defaults for everything. Advanced users can still use individual commands. |
| **Agent developer doesn't care about AKS/ACR/KV.** The init step exposes too many Azure infrastructure concepts. An agent developer wants to run their agent, not provision cloud resources. | High | `azureclaw up` hides infrastructure. It picks defaults (region, VM size, resource names) and provisions silently. Advanced config available via `azureclaw init --config my-infra.yaml`. |
| **Network policy is manual YAML.** Writing allowlists by hand is error-prone. Most agents need the same endpoints (GitHub, npm, ClawHub, model provider). | Medium | Ship **built-in policy presets**: `default` (minimal), `developer` (GitHub, npm, pip), `web` (allows common web endpoints). Users can extend, not write from scratch: `azureclaw policy extend my-assistant --allow api.stripe.com:443`. |
| **The CRD spec is intimidating.** The ClawSandbox CRD has 30+ fields. An agent developer who just wants to run OpenClaw shouldn't see this. | Medium | The CRD is for platform operators. Agent developers use the CLI or a minimal `azureclaw.yaml`. The CLI generates the CRD behind the scenes. |
| **No local development story.** How do you develop/test an agent before deploying to AKS? You shouldn't need a cluster to iterate. | High | Add `azureclaw dev` — runs a sandbox locally via Docker (similar to NemoClaw's single-node mode). Same policy engine, same network rules, but on your laptop. Graduate to AKS with `azureclaw deploy`. |
| **How do agent skills/tools work across the sandbox boundary?** OpenClaw has browser control, cron, webhooks, nodes. Which of these work in a sandbox? | Medium | Document clearly: tools that run inside the sandbox (bash, file ops, agent-to-agent) work as-is. Tools that need external access (browser, webhooks) require explicit policy. Browser gets a sandboxed Chromium sidecar. Webhooks get an ingress with auth. |
| **Approval flow is CLI-only.** `azureclaw approve` is useful for developers, but operators in production need a dashboard, webhook to Slack/Teams, or API integration. | Medium | The approval controller should support multiple channels: CLI/TUI (dev), webhook (Slack/Teams), REST API (automation), Azure Monitor alert action (enterprise). |
| **Cost visibility is afterthought.** Inference costs can spiral. Developers need to see cost in real time, not after the bill arrives. | Medium | `azureclaw <name> costs` shows real-time compute + inference cost. Token budgets per sandbox with alerts. Cost column in `azureclaw list`. |
| **Model switching ergonomics.** `azureclaw my-agent inference set --model azure/gpt-4.1` is too verbose. | Low | Shortened to `azureclaw my-agent model set gpt-4.1`. The `azure/` prefix is implied. Auto-complete for model names. |

### 12.2 The `azureclaw up` Command

The most critical UX decision: **one command to go from zero to running agent.**

```bash
azureclaw up
```

What it does:
1. Checks Azure CLI auth (prompts `az login` if needed)
2. Creates a resource group (`azureclaw-<region>`) if none exists
3. Deploys AKS + ACL + ACR + Key Vault + Azure OpenAI via Bicep
4. Installs the AzureClaw Helm chart
5. Creates a default sandbox (`my-assistant`) with `gpt-4.1` and the `developer` policy preset
6. Prints connection instructions

The entire flow is idempotent. Running `azureclaw up` again is a no-op if everything is healthy.

Advanced options:
```bash
azureclaw up --model Phi-4              # use a different model
azureclaw up --name code-agent          # custom sandbox name
azureclaw up --policy web               # use the 'web' policy preset
azureclaw up --region westeurope        # deploy in a specific region
azureclaw up --confidential             # enable TEE (SEV-SNP)
```

### 12.3 The `azureclaw dev` Command (local development)

For iterating without a cluster. Runs the same Azure Linux 4 sandbox image locally via Docker:

```bash
azureclaw dev                          # pull pre-built sandbox image
azureclaw dev --build                  # build from local Dockerfile (Azure Linux 4 base)
```

What it does:
1. Pulls (or builds) the sandbox container image based on Azure Linux 4
2. Starts a Docker container with the same seccomp profile, network policy simulation, and inference routing
3. Opens a shell into the sandbox
4. Model calls are routed to Azure OpenAI (using your local `az login` credentials)

The key guarantee: **the container you develop in locally is the same one that runs on AKS**. Same Azure Linux 4 base, same packages, same SELinux context, same seccomp profile. The only difference is the host: your Docker daemon vs ACL nodes on AKS.

### 12.4 Policy Presets

Instead of writing YAML from scratch, start with a preset and extend:

| Preset | What's allowed | Use case |
|--------|---------------|----------|
| `minimal` | Inference router only. Nothing else. | Maximum lockdown, testing |
| `developer` | + GitHub, npm, pip, ClawHub, OpenClaw docs | Day-to-day agent development |
| `web` | + Common SaaS APIs (Stripe, Twilio, SendGrid, etc.) | Agents that interact with web services |
| `azure` | + All declared Azure services via Managed Identity | Agents that use Azure resources |
| `custom` | User-defined from scratch | Full control |

```bash
# Start with a preset
azureclaw launch my-agent --policy developer

# Extend it
azureclaw policy extend my-agent --allow api.stripe.com:443 --methods POST
```

---

## 13. Outcomes — What AzureClaw Delivers

> **The measure of AzureClaw is not how many technologies it uses, but how few things the user has to think about.**

AzureClaw should be communicated through **outcomes**, not components. Here's the framing:

### For agent developers:

| Outcome | One-liner |
|---------|-----------|
| **Run your agent safely** | `azureclaw up` — one command, secure by default. No security expertise needed. |
| **Use any AI model** | 1800+ models, switch instantly, never manage credentials. |
| **Connect to Azure services** | Storage, Search, Cosmos, and more — just declare what you need, Managed Identity handles the rest. |
| **See what your agent does** | Real-time traces, cost tracking, approval flow for unknown endpoints. |
| **Iterate fast** | `azureclaw dev` for local development, `azureclaw deploy` for production. Same policies, same experience. |

### For platform operators:

| Outcome | One-liner |
|---------|-----------|
| **Multi-agent, multi-tenant** | Each agent in its own namespace. Policy isolation. Shared infrastructure. |
| **Compliance on autopilot** | Node OS hardened by default, CIS/STIG baselines enforced continuously (TODO). |
| **Cost control** | Per-sandbox token budgets, compute attribution, alerts on spend anomalies. |
| **Audit everything** | Every network call, model call, file access, and policy change is logged. |
| **Production-grade** | 99.95% SLA, multi-region, autoscale, signed images, auto-patching. |

### For leadership / decision-makers:

| Question | Answer |
|----------|--------|
| "Why not just use NemoClaw?" | NemoClaw is single-node, single-model (NVIDIA), no Azure integration, no compliance story. AzureClaw is enterprise-grade on Azure with 1800+ models and native Azure service access. |
| "Why not just run OpenClaw on a VM?" | No sandboxing, no policy enforcement, no identity management, no cost tracking, no compliance. AzureClaw adds the governance layer. |
| "Is it complex?" | `azureclaw up` — one command. Complexity is hidden behind good defaults. Operators who need control get it via CRDs and policies. |
| "What about vendor lock-in?" | MIT-licensed open source. Uses standard Kubernetes. OpenClaw is the same everywhere. Azure services are optional (but seamless when used). |

### The Experience Hierarchy

AzureClaw should present a **layered experience** — not a flat list of technologies:

```
┌─────────────────────────────────────────────────────────────┐
│  Layer 1: "azureclaw up"                                    │
│  For: Agent developers who just want to run their agent     │
│  Knows about: nothing — it just works                       │
├─────────────────────────────────────────────────────────────┤
│  Layer 2: CLI commands + policy presets                     │
│  For: Developers who want to customize model, policy, etc.  │
│  Knows about: models, endpoints, policy presets             │
├─────────────────────────────────────────────────────────────┤
│  Layer 3: YAML policies + ClawSandbox CRD                   │
│  For: Platform operators managing multiple agents/tenants   │
│  Knows about: CRDs, network policies, Azure services        │
├─────────────────────────────────────────────────────────────┤
│  Layer 4: Bicep/Helm + infrastructure config                │
│  For: Cloud architects designing the underlying platform    │
│  Knows about: AKS, ACL, Bicep, Helm, Inspektor Gadget     │
└─────────────────────────────────────────────────────────────┘
```

Each layer builds on the one below it. Most users never go past Layer 2. The README speaks to Layer 1. The docs cover Layers 2–3. The PLAN covers Layer 4. **Technologies are never mentioned in user-facing surfaces unless the user is at the layer where they need to know.**

---

## 14. Success Criteria

| Metric | Target |
|--------|--------|
| **Time to first agent** | < 5 minutes from `azureclaw up` to agent responding |
| **Commands to get started** | 1 (`azureclaw up`) — vs NemoClaw's 2 |
| **Azure service integration** | `DefaultAzureCredential()` works inside sandbox, zero config |
| **Model switching** | Instant, no restart, no credential changes |
| **Local dev experience** | `azureclaw dev` runs sandbox locally with same policies |
| **Security by default** | All protections on without user configuration |
| **Supported models** | 1800+ via Azure AI Foundry (vs NemoClaw's 1) |
| **Compliance baselines** | CIS AKS Optimized + DISA STIGs via azure-osconfig (TODO) |
| **Container startup** | < 15s for sandbox pod (cold start) |
| **Cost transparency** | Per-sandbox compute + inference cost visible in real time |
| **NemoClaw migration** | Policy format + CLI semantics compatible |
| **Open-source community** | 50+ stars in first month, external PRs within 3 months |
