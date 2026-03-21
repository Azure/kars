# AzureClaw Architecture

AzureClaw is a Kubernetes-native runtime for running OpenClaw AI agents safely on Azure. Three pillars:

- **OpenClaw** — agent framework (authoring, orchestration, local tools, channels)
- **Azure AI Foundry** — managed AI services replacing unsafe built-ins (models, memory, knowledge, web, code, evaluations, guardrails)
- **AGT** (opt-in) — behavioral governance for multi-agent (trust, policy, inter-agent, kill switch)
- **AzureClaw** — infrastructure security (seccomp, iptables, Kata VM, NetworkPolicy, IMDS, Content Safety, token budgets)

## Design Principles

1. **OpenClaw owns the agent.** Users build agents with AGENTS.md/SOUL.md/skills. AzureClaw doesn't reinvent agent authoring.
2. **Use Foundry standalone APIs, not hosted agents.** Memory via Memory Store APIs (not threads). Knowledge via Foundry IQ / AI Search (not file_search runs). No Foundry hosted agent required — OpenClaw stays the orchestrator.
3. **AzureClaw is the runtime layer, not the AI platform.** Foundry provides managed AI services. AzureClaw provides sandboxed execution.
4. **AGT is opt-in and non-overlapping.** If AzureClaw enforces at kernel/router level, AGT doesn't touch it. AGT only handles tool-level policy the router can't see.
5. **CRD is single source of truth.** No dual configuration. Token budgets, content safety, tools, governance — all in one CRD.
6. **Per-sandbox sidecar, not shared gateway.** Each sandbox gets its own inference router process. No cross-tenant blast radius.

## Foundry Integration Model — Standalone APIs

AzureClaw uses Foundry services as standalone APIs through the inference router. **No Foundry hosted agent is created or managed.** OpenClaw is the only orchestrator.

| Foundry Capability | API Type | Needs Hosted Agent? | How AzureClaw uses it |
|---|---|---|---|
| **Model Catalog (200+)** | `/v1/chat/completions` | No | Router proxies inference |
| **Content Safety** | Content Safety REST API | No | Router calls on every request |
| **Prompt Shields** | Content Safety REST API | No | Router calls on every request |
| **Memory Store** | `/memory_stores/*` REST API | No | Skill teaches agent to store/search memories. Async update + semantic search with embeddings. |
| **Code Interpreter** | `/openai/responses` + `code_interpreter` tool | No | Responses API with `tools: [{type: "code_interpreter"}]`. No hosted agent needed. |
| **Web Search** | `/openai/responses` + `bing_grounding` tool | No | Responses API with `tools: [{type: "bing_grounding"}]`. No hosted agent needed. |
| **Memory Search** | `/openai/responses` + `memory_search` tool | No | Responses API with `tools: [{type: "memory_search"}]`. Cross-session memory recall. |
| **Knowledge (Foundry IQ)** | `/openai/responses` + `file_search` / `azure_ai_search` | No | Responses API with agentic retrieval tools. RAG over uploaded or indexed documents. |
| **Evaluations** | `/openai/evals`, `/evaluators`, `/evaluationrules` | No | Full eval lifecycle via REST API. |
| **Conversations** | `/openai/conversations` | No | Persistent multi-turn conversation threads. |
| **Agents** | `/agents` | No | Prompt agent CRUD (used by Foundry, not OpenClaw orchestration). |
| **Deployments** | `/deployments` | No | Query deployed models, versions, capabilities. |
| **Connections** | `/connections` | No | Query project data connections (AI Search, etc.). |
| **Datasets** | `/datasets` | No | Upload/manage evaluation datasets. |
| **Indexes** | `/indexes` | No | Knowledge indexes for agentic retrieval. |
| **Insights** | `/insights` | No | Agent monitoring and usage analytics. |
| **Guardrails** | Content Safety custom policies | No | Configurable via router |
| **Fine-tuning** | `/openai/fine-tuning/jobs` | No | Fine-tune REST API (regional availability varies). |

**Key insight:** ALL Foundry capabilities work as standalone APIs via the Responses API — no hosted Foundry agents are required. OpenClaw is the sole orchestrator; Foundry provides the managed AI services. The router proxies 18 distinct API groups with IMDS authentication.

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
| `/memory_stores`, `/memory_stores/{*path}` | Memory Store — CRUD, search, update memories. |
| `/openai/responses`, `/openai/responses/{*path}` | Responses API — Code Interpreter, Web Search, Memory Search, Knowledge. |
| `/openai/conversations`, `/openai/conversations/{*path}` | Persistent multi-turn conversations. |
| `/openai/evals`, `/openai/evals/{*path}` | OpenAI Evaluations — create evals, runs. |
| `/openai/fine-tuning/{*path}` | Fine-tuning jobs. |
| `/agents`, `/agents/{*path}` | Foundry Agents — prompt agent CRUD. |
| `/evaluators`, `/evaluators/{*path}` | Evaluator catalog. |
| `/evaluationrules`, `/evaluationrules/{*path}` | Evaluation rules. |
| `/evaluationtaxonomies`, `/evaluationtaxonomies/{*path}` | Evaluation taxonomies. |
| `/indexes`, `/indexes/{*path}` | Knowledge indexes (Foundry IQ). |
| `/connections`, `/connections/{*path}` | Project data connections. |
| `/deployments`, `/deployments/{*path}` | Model deployments. |
| `/datasets`, `/datasets/{*path}` | Datasets (upload, versions). |
| `/insights`, `/insights/{*path}` | Agent monitoring insights. |
| `/knowledgebases`, `/knowledgebases/{*path}` | Knowledge bases (agentic retrieval). |
| `/redTeams/runs`, `/redTeams/runs/{*path}` | Red team runs. |
| `/schedules`, `/schedules/{*path}` | Scheduled jobs. |
| `GET /healthz` | Readiness probe. |
| `GET /readyz` | Deep readiness (token + Content Safety check). |
| `GET /metrics` | Prometheus metrics. |
| `GET /blocklist/status` | Blocklist health: domain count + enabled state. |
| `POST /blocklist/check` | Check if a domain/URL is blocked by threat intelligence. |
| `GET /egress/learned` | List learned egress domains (learn mode). |
| `POST /egress/learned/clear` | Clear learned domains. |
| `POST /agt/evaluate` | AGT policy evaluation. |
| `GET /agt/trust`, `GET /agt/trust/{agent_id}` | Trust store queries. |
| `GET /agt/audit`, `GET /agt/audit/verify` | Audit log + integrity verification. |
| `POST /agt/mesh/send`, `GET /agt/mesh/inbox`, `POST /agt/mesh/receive` | Inter-agent AGT mesh. |
| `GET /agt/status` | AGT governance status. |
| `POST /sandbox/spawn` | Create sub-agent ClawSandbox CRD via K8s API. |
| `GET /sandbox/list` | List sub-agents spawned by this sandbox. |
| `GET /sandbox/{name}/status` | Get sub-agent status. |
| `DELETE /sandbox/{name}` | Tear down a sub-agent sandbox. |

All Foundry project routes use the `foundry_proxy` handler: acquire IMDS token (audience: `https://ai.azure.com`), forward request with original path + query string, log method/path/status.

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
