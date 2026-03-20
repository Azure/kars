# AzureClaw Architecture

AzureClaw is a Kubernetes-native runtime for running OpenClaw AI agents safely on Azure. It replaces NVIDIA's single-node NemoClaw approach with a production-grade architecture built on AKS and Azure AI Foundry.

## Design Principles

1. **AzureClaw is the runtime layer, not the AI platform.** Foundry provides models, agent orchestration, memory, and evaluation. AzureClaw provides sandboxed execution.
2. **Don't duplicate Azure AI platform services.** Use Content Safety, not a custom filter. Use Foundry Agent API, not custom Cosmos/Search integrations.
3. **Security on by default.** Every layer is enabled out of the box. Operators opt out, not in.
4. **Per-sandbox sidecar, not shared gateway.** Each sandbox gets its own inference router process. No cross-tenant blast radius.

---

## System Overview

```
                            azureclaw up / azureclaw add
                                      │
                                      ▼
┌─ AKS Cluster (Azure Linux, Cilium CNI) ──────────────────────────────────┐
│                                                                           │
│  azureclaw-system namespace                                               │
│  ┌──────────────────────────────────────────────────────┐                 │
│  │ Controller (Rust/kube-rs) × 2 replicas               │                 │
│  │  • Watches ClawSandbox CRDs                          │                 │
│  │  • Reconciles → namespace, pod, NetworkPolicy, SA    │                 │
│  │  • Installs iptables egress-guard init container     │                 │
│  │  • Injects Content Safety, token budgets, IMDS creds │                 │
│  └──────────────────────────────────────────────────────┘                 │
│  seccomp DaemonSet → installs azureclaw-strict.json on every node        │
│  ClawSandbox CRD (v1alpha1)                                               │
│                                                                           │
│  azureclaw-<agent> namespace (one per sandbox)                            │
│  ┌──────────────────────────────────────────────────────────────────┐     │
│  │ Pod (2 containers + 1 init)                                      │     │
│  │                                                                  │     │
│  │  init: egress-guard                                              │     │
│  │   └─ iptables: UID 1000 → localhost + DNS only                   │     │
│  │                                                                  │     │
│  │  container: openclaw (UID 1000)                                  │     │
│  │   ├─ OpenClaw gateway + TUI                                      │     │
│  │   ├─ Read-only rootfs, writable /sandbox + /tmp                  │     │
│  │   ├─ AzureClaw plugin (slash commands)                           │     │
│  │   └─ All inference → localhost:8443                              │     │
│  │                                                                  │     │
│  │  container: inference-router (UID 1001)                          │     │
│  │   ├─ Auth: IMDS / Workload Identity (zero keys)          ──────►│─► Azure AI Foundry
│  │   ├─ Content Safety + Prompt Shields                      ──────►│─► Azure AI Content Safety
│  │   ├─ Token budgets (daily + per-request, 429)                    │     │
│  │   ├─ SSE streaming (when stream: true)                           │     │
│  │   ├─ Foundry Agent API proxy (/agents/*)                  ──────►│─► Foundry Agent Service
│  │   └─ Prometheus metrics (:8443/metrics)                          │     │
│  └──────────────────────────────────────────────────────────────────┘     │
│  NetworkPolicy: default-deny egress + allowlist                           │
│  ServiceAccount: Workload Identity annotation                             │
│                                                                           │
└───────────────────────────────────────────────────────────────────────────┘
```

---

## Components

### CLI (TypeScript)

User-facing interface for the full lifecycle: infrastructure provisioning, sandbox management, policy control, and observability.

**12 commands:**

| Command | Implementation |
|---------|----------------|
| `up` | Deploys Bicep (AKS, ACR, KV, AOAI, Monitor), pushes images to ACR, Helm installs controller + CRD, creates first sandbox. Auto-detects existing AKS to skip infra. |
| `add` | Creates a ClawSandbox CRD in an existing cluster. No infra redeploy. |
| `dev` | Starts a local Docker sandbox with the same security policies (seccomp, rootfs, non-root). |
| `connect` | Attaches to OpenClaw TUI via kubectl exec (AKS) or docker exec (local). Supports `--shell`. |
| `status` | Shows health, isolation level, model, uptime, token usage. |
| `logs` | Streams kubectl/docker logs from sandbox containers. |
| `model` | Get/set model. Hot-reloads via CRD patch → rolling update. |
| `trace` | eBPF tracing: TCP, DNS, file access, process exec via Inspektor Gadget. |
| `policy` | Allow/deny/get network endpoints. Hot-reload via CRD merge patch. |
| `approve` | List/approve/deny pending egress requests (ConfigMap-based). |
| `onboard` | Interactive wizard: Azure OpenAI endpoint + key configuration. Stores in `~/.azureclaw/`. |
| `destroy` | Tear down sandbox, or entire resource group + soft-deleted resources. |

### Controller (Rust, kube-rs)

Kubernetes operator that watches `ClawSandbox` custom resources and reconciles them into running sandboxes.

**Reconciliation pipeline (5 steps):**

1. **Namespace** — creates `azureclaw-<name>` with PodSecurity labels (`enforce: privileged` for egress-guard NET_ADMIN, `audit/warn: restricted`)
2. **ServiceAccount** — annotated with `azure.workload.identity/client-id` for Workload Identity
3. **NetworkPolicy** — default-deny egress + per-sandbox allowlist from CRD spec (DNS + IMDS always allowed for router)
4. **Pod** — 2 containers + 1 init container:
   - `egress-guard` init: runs iptables rules restricting UID 1000 to localhost + DNS
   - `openclaw`: agent container (UID 1000, read-only rootfs, drop ALL capabilities)
   - `inference-router`: sidecar proxy (UID 1001, env vars injected: endpoint, model, content safety, token budgets)
5. **Status** — updates CRD status: phase=Running, sandboxPod, inferenceEndpoint

**Isolation level handling:**

| Level | RuntimeClass | seccomp | nodeSelector |
|-------|-------------|---------|--------------|
| standard | (none) | RuntimeDefault | `agentpool: clawpool` |
| enhanced | (none) | Localhost `azureclaw-strict` | `agentpool: clawpool` |
| confidential | `kata-vm-isolation` | RuntimeDefault | `agentpool: katapool` |

**CRD validation:** Rejects invalid isolation levels, missing model/endpoint, malformed URLs.

**Tests:** 9 unit tests (isolation scheduling, pod security context, token defaults, image defaults).

### Inference Router (Rust, axum)

Per-sandbox sidecar that intercepts every inference call. The agent container (UID 1000) can only reach `localhost:8443` — the router is the sole path to external AI services.

**Routes:**

| Route | Function |
|-------|----------|
| `POST /v1/chat/completions` | Forward to Foundry. SSE streaming when `stream: true`. |
| `POST /v1/completions` | Forward to Foundry. |
| `POST /v1/embeddings` | Forward to Foundry. |
| `GET /v1/models` | Query Foundry model catalog. |
| `GET/POST /agents`, `/agents/{*path}` | Foundry Agent API proxy (threads, memory, files, runs). |
| `GET /healthz` | Readiness probe. |
| `GET /metrics` | Prometheus metrics. |

**Request pipeline:**

```
Agent request → Token budget check (429 if exceeded)
             → Content Safety analysis (fail-open)
             → Prompt Shields jailbreak detection (fail-open)
             → IMDS/WI token acquisition (cached per scope)
             → Forward to Foundry /openai/v1/ (or AOAI in dev mode)
             → Extract token usage from response
             → Record Prometheus metrics
             → Return to agent
```

**Metrics:**

| Metric | Type | Labels |
|--------|------|--------|
| `inference_requests` | Counter | sandbox, model, status |
| `inference_latency` | Histogram | sandbox, model |
| `tokens_used` | Counter | sandbox, model, direction (input/output) |

**Concurrency:** 64 concurrent requests max (`ConcurrencyLimitLayer`).

**Tests:** 5 unit tests (budget enforcement, per-sandbox isolation, usage tracking).

### Policy Engine

Multi-layer enforcement stack — each layer is independently active:

| Layer | Technology | What It Enforces |
|-------|-----------|-----------------|
| Kernel | seccomp (Localhost for enhanced) | ~150 allowed syscalls. Blocks mount, ptrace, bpf, unshare, module loading. |
| Container | Pod Security Standards | Read-only rootfs, non-root, drop ALL, no privilege escalation. |
| Per-container egress | iptables init container | UID 1000 → localhost + DNS only. UID 1001 → governed by NetworkPolicy. |
| Namespace network | Kubernetes NetworkPolicy | Default-deny egress. Allowlist managed via `azureclaw policy`. |
| Inference | Content Safety + Prompt Shields | Input/output content filtering, jailbreak detection. |
| Token governance | Budget enforcement | Daily and per-request limits with HTTP 429. |
| VM isolation | Kata Containers (confidential) | Per-pod dedicated kernel via Cloud Hypervisor. |
| Observability | Inspektor Gadget (optional) | eBPF tracing of syscalls, network, files, DNS. |

### Sandbox Image

Multi-stage Docker build (`sandbox-images/openclaw/Dockerfile`):
- **Builder stage:** Node.js 22, installs OpenClaw globally
- **Runtime stage:** Azure Linux 3 base (`mcr.microsoft.com/azurelinux/base/core:3.0`), non-root user `sandbox:1000`, read-only rootfs
- Includes: OpenClaw binaries, inference-router binary (from separate build), AzureClaw plugin
- **Entrypoint:** Generates `openclaw.json` config pointing inference to `localhost:8443`

### OpenClaw Plugin (TypeScript)

Registered inside the sandbox as an OpenClaw provider and slash command handler:

- Provider: `azure-openai` — routes inference to localhost:8443 (the router)
- Models: GPT-4.1, GPT-5-mini, GPT-4o, DeepSeek-V3.2, Phi-4, Llama 3.1 405B, o3-mini
- Slash commands: `/azureclaw status`, `/azureclaw-agents` (Foundry agents), `/azureclaw-memory` (threads)

---

## CRD Schema (v1alpha1)

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: my-agent
  namespace: azureclaw-system
spec:
  openclaw:           # version, image, config
  sandbox:            # isolation (standard|enhanced|confidential), seccompProfile,
                      # readOnlyRootFilesystem, runAsNonRoot, writablePaths
  inference:          # endpoint, model, contentSafety, promptShields, tokenBudget
                      #   tokenBudget: {dailyLimit, perRequestLimit}
  networkPolicy:      # defaultDeny, approvalRequired, allowedEndpoints[]
  azureServices:      # [RESERVED — schema exists, controller does not create RBAC bindings yet]
  resources:          # requests, limits (CPU/memory)
status:
  phase:              # Pending | Running | Failed
  sandboxPod:         # pod name
  namespace:          # created namespace
  inferenceEndpoint:  # localhost:8443
  tokensUsed:         # cumulative
  pendingApprovals:   # count
```

Short names: `cs`, `claw`. Print columns: Phase, Model, Isolation, Age.

---

## Infrastructure (Bicep)

5 modules deployed by `azureclaw up`:

| Module | Resources | Key Configuration |
|--------|-----------|-------------------|
| `aks.bicep` | AKS cluster | Azure Linux nodes, Cilium CNI, Workload Identity, system + clawpool + optional katapool. RBAC: AcrPull, CS OpenAI User, KV Secrets User. |
| `acr.bicep` | Container Registry | Premium SKU, content trust enabled, network rules. |
| `openai.bicep` | Cognitive Services | OpenAI kind, model deployment, OIDC auth only (no API key), firewall. Supports soft-delete restore. |
| `keyvault.bicep` | Key Vault | RBAC access, soft-delete, purge protection. Supports recover. |
| `monitor.bicep` | Log Analytics + App Insights | 90-day retention. |

Helm chart deploys to AKS: ClawSandbox CRD, controller deployment (2 replicas), RBAC (ClusterRole + ServiceAccount), seccomp DaemonSet, NetworkPolicy ConfigMap template.

---

## Authentication Flow

```
                     AKS Pod
                       │
  ┌────────────────────┼────────────────────┐
  │ openclaw (UID 1000)│                    │
  │   → localhost:8443 ┤                    │
  │                    │                    │
  │ inference-router   │                    │
  │ (UID 1001)         │                    │
  │   ┌────────────────┤                    │
  │   │ 1. Read projected SA token          │
  │   │    /var/run/secrets/azure/tokens/    │
  │   │ 2. Exchange via WI federation       │──► Azure AD
  │   │    (or fall back to IMDS)           │──► IMDS endpoint
  │   │ 3. Cache token per resource scope   │
  │   │ 4. Inject Bearer header             │
  │   └────────────────┤                    │
  │                    ▼                    │
  │              Azure AI Foundry           │
  └─────────────────────────────────────────┘
```

**Key property:** The agent container (UID 1000) cannot reach IMDS — iptables blocks it. Only the inference router (UID 1001) can authenticate.

---

## CI/CD

| Workflow | Trigger | Jobs |
|----------|---------|------|
| `ci.yml` | Push/PR | rust-build (fmt, clippy, build, test), cli-build (typecheck, lint, build), bicep-validate, helm-lint, security-scan (Trivy FS), container-scan (Trivy image) |
| `image-sign-sbom.yml` | Tag (v*) | Docker build, Trivy scan, Notation signing (Azure KV), SBOM generation (Syft/SPDX) |
