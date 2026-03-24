# Architecture

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
│  │  • Reconciles → NS, SA, NetworkPolicy, Deployment    │                 │
│  │  • Creates egress-guard init, blocklist CM, CronJob  │                 │
│  │  • Optional: AGT Service + policy CM + mesh ingress  │                 │
│  └──────────────────────────────────────────────────────┘                 │
│  seccomp DaemonSet → azureclaw-strict.json on every node                 │
│  ClawSandbox CRD (v1alpha1)                                               │
│                                                                           │
│  azureclaw-<name> namespace (one per sandbox)                             │
│  ┌──────────────────────────────────────────────────────────────────┐     │
│  │ Pod (2 containers + 1 init)                                      │     │
│  │                                                                  │     │
│  │  init: egress-guard (runs as root, then exits)                   │     │
│  │   └─ iptables: UID 1000 → localhost + DNS + ESTABLISHED only     │     │
│  │                                                                  │     │
│  │  container: openclaw (UID 1000, network-restricted)              │     │
│  │   ├─ Gateway :18789 (WebSocket + Control UI)                     │     │
│  │   ├─ TUI :18791                                                  │     │
│  │   ├─ Read-only rootfs, writable /sandbox + /tmp                  │     │
│  │   ├─ AzureClaw plugin (tools: spawn, mesh, http_fetch)           │     │
│  │   └─ All external access → localhost:8443 only                   │     │
│  │                                                                  │     │
│  │  container: inference-router (UID 1001, unrestricted network)    │     │
│  │   ├─ Inference proxy (/v1/*)              ───────────────────────┼──► Azure OpenAI / Foundry
│  │   ├─ Egress proxy (/egress/fetch)         ───────────────────────┼──► External HTTP (audited)
│  │   ├─ AGT relay (/agt/relay)               ───────────────────────┼──► agentmesh-relay (WS)
│  │   ├─ Content Safety + Prompt Shields      ───────────────────────┼──► Azure AI Content Safety
│  │   ├─ Token budgets, audit logging, Prometheus metrics            │     │
│  │   └─ Sub-agent spawn (/sandbox/spawn)     ───────────────────────┼──► K8s API (CRD create)
│  └──────────────────────────────────────────────────────────────────┘     │
│  NetworkPolicy: default-deny egress + allowlist                           │
│  ServiceAccount: Workload Identity (azure.workload.identity/client-id)    │
│  Blocklist ConfigMap + CronJob (6h refresh from OISD + URLhaus)           │
│                                                                           │
└───────────────────────────────────────────────────────────────────────────┘
```

---

## Components

### Controller (Rust, kube-rs)

Watches `ClawSandbox` CRDs and reconciles into running sandboxes. Source: `controller/src/reconciler.rs`.

**Reconciliation steps:**

| Step | Resource Created | Key Details |
|------|-----------------|-------------|
| 1 | Namespace | `azureclaw-<name>`, PodSecurity labels (enforce: privileged, audit/warn: baseline) |
| 2 | ServiceAccount | Workload Identity annotation, `azure.workload.identity/client-id` |
| 2a | ClusterRoleBinding | `azureclaw-sandbox-spawner` — grants SA permission to create ClawSandbox CRDs |
| 2b | Secret | `gateway-token` — shared auth token for TUI↔gateway (idempotent, reused on reconcile) |
| 3 | NetworkPolicy | Default-deny egress; allows: DNS, IMDS, HTTPS :443, mesh :8443, relay :8765/:8080 |
| 4 | Deployment | 1 init + 2 containers (see below), blocklist CM volume, optional AGT policy volume |
| 4b | SA annotations | Azure Services RBAC annotations (reserved, no bindings yet) |
| 4c | Service + ConfigMap + NP ingress | AGT governance infra (when `governance.enabled: true`) |
| 4d | ConfigMap + CronJob | Blocklist seed + 6h refresh job |
| 5 | CRD status | phase=Running, sandboxPod, namespace, inferenceEndpoint |

**Isolation levels:**

| Level | RuntimeClass | seccomp | Node Pool |
|-------|-------------|---------|-----------|
| standard | (default runc) | RuntimeDefault | `sandbox` |
| enhanced (default) | (default runc) | Localhost `azureclaw-strict` (~150 syscalls) | `sandbox` |
| confidential | `kata-vm-isolation` | RuntimeDefault (VM is the boundary) | `sandbox-kata` |

### Inference Router (Rust, axum sidecar)

Per-sandbox sidecar on port 8443. Runs as UID 1001 (unrestricted network). The agent (UID 1000) can only reach `localhost:8443` — the router is the sole external path.

**Request pipeline (inference):**

```
Agent → Token budget check (429) → Content Safety (fail-open)
      → Prompt Shields (fail-open) → IMDS/WI token (cached per scope)
      → Forward to Azure OpenAI / Foundry → Extract usage → Prometheus metrics → Agent
```

**Egress proxy pipeline:** `POST /egress/fetch`

```
Agent → Blocklist check (hard deny) → Learn mode? (log + allow)
      → Allowlist check (approved domains pass) → Unknown? deny + create PendingApproval
```

See [`egress-proxy.md`](egress-proxy.md) for full details.

### OpenClaw Sandbox (Node.js)

Runs as UID 1000 (all outbound blocked by iptables except localhost + DNS + ESTABLISHED replies).

- **Gateway** on port 18789 — WebSocket + Control UI
- **TUI** on port 18791
- **Plugin:** AzureClaw tools (`spawn`, `mesh`, `http_fetch`), provider `azure-openai` routing to `localhost:8443`
- **Channels:** Telegram, WhatsApp (via `/egress/fetch` proxy)
- **Filesystem:** Read-only rootfs, writable `/sandbox` + `/tmp` (emptyDir, `/tmp` is tmpfs 1Gi)

### Egress Guard (init container)

Runs as root with `NET_ADMIN` + `NET_RAW`, then exits. Installs iptables OUTPUT rules for UID 1000:

```
iptables -A OUTPUT -m owner --uid-owner 1000 -o lo -j ACCEPT
iptables -A OUTPUT -m owner --uid-owner 1000 -p udp --dport 53 -j ACCEPT
iptables -A OUTPUT -m owner --uid-owner 1000 -p tcp --dport 53 -j ACCEPT
iptables -A OUTPUT -m owner --uid-owner 1000 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
iptables -A OUTPUT -m owner --uid-owner 1000 -j DROP
```

The `ESTABLISHED,RELATED` rule allows reply packets (SYN-ACK) for inbound connections to the gateway — required for WebUX and channel connections to work.

---

## Network Architecture

```
                        ┌─────────────────────────────────┐
                        │       iptables (per-UID)         │
                        │                                  │
  UID 1000 (openclaw)   │  lo ✓  DNS ✓  ESTABLISHED ✓     │
                        │  everything else ✗ (DROP)        │
                        │                                  │
  UID 1001 (router)     │  no iptables restrictions        │
                        │  governed by NetworkPolicy only  │
                        └─────────────────────────────────┘
                                       │
                        ┌──────────────┴──────────────┐
                        │     NetworkPolicy (pod)      │
                        │                              │
                        │  Egress: DNS, IMDS, HTTPS    │
                        │    :443, mesh :8443,          │
                        │    relay :8765/:8080          │
                        │  Ingress: mesh :8443,         │
                        │    gateway :18789/:18791      │
                        │    (AGT-enabled only)         │
                        └──────────────────────────────┘
```

**Three enforcement layers work together:**
1. **iptables** — per-container (UID-based), blocks agent from any external network
2. **NetworkPolicy** — per-pod, default-deny with allowlist
3. **Inference-as-network-policy** — router is sole egress path; agent has no credentials

---

## CRD Schema

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: my-agent
  namespace: azureclaw-system
spec:
  openclaw:                          # version, image, config
  sandbox:
    isolation: enhanced              # standard | enhanced | confidential
    seccompProfile: azureclaw-strict # custom Localhost profile
    readOnlyRootFilesystem: true
    runAsNonRoot: true
    allowPrivilegeEscalation: false
    writablePaths: ["/sandbox", "/tmp"]
  inference:
    provider: azure-openai           # azure-openai | azure-ai-foundry | self-hosted
    model: gpt-4.1
    contentSafety: true
    promptShields: true
    tokenBudget:
      daily: 0                       # 0 = unlimited
      perRequest: 0
  networkPolicy:
    defaultDeny: true
    approvalRequired: true
    learnEgress: true                # observe domains (blocklist still enforced)
    allowedEndpoints: []
  governance:                        # AGT (opt-in)
    enabled: false
    toolPolicy: default              # policy profile name
    trustThreshold: 500              # 0-1000
  agent:                             # Foundry agent tools
    tools: []                        # file_search, web_search, code_interpreter
  azureServices: []                  # RESERVED — annotations only, no RBAC yet
  resources:
    requests: {cpu: 500m, memory: 1Gi}
    limits: {cpu: "2", memory: 4Gi}
status:
  phase: Running                     # Pending | Creating | Running | Failed | Terminating
  sandboxPod: my-agent-xxx
  namespace: azureclaw-my-agent
  inferenceEndpoint: localhost:8443
  tokensUsed: {input: 0, output: 0}
  pendingApprovals: 0
  foundryAgentId: ""                 # if Foundry agent created
```

Short names: `cs`, `claw`. Print columns: Phase, Model, Isolation, Age.

---

## API Endpoints

All endpoints served by the inference router on `:8443`.

### Inference (proxied to Azure OpenAI / Foundry)

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/v1/chat/completions` | Chat inference (SSE streaming supported) |
| POST | `/v1/completions` | Text completions |
| POST | `/v1/embeddings` | Embedding generation |
| GET | `/v1/models` | Model catalog |

### Foundry Standalone APIs (18 API groups, IMDS auth)

| Path prefix | Purpose |
|-------------|---------|
| `/memory_stores/*` | Memory Store — persistent long-term memory |
| `/knowledgebases/*` | Foundry IQ — agentic retrieval |
| `/openai/responses/*` | Responses API (Code Interpreter, Web Search, Memory Search) |
| `/openai/conversations/*` | Persistent multi-turn conversations |
| `/openai/evals/*` | Evaluations |
| `/openai/fine-tuning/*` | Fine-tuning jobs |
| `/agents/*` | Foundry Agents CRUD |
| `/evaluators/*`, `/evaluationrules/*`, `/evaluationtaxonomies/*` | Evaluation ecosystem |
| `/indexes/*`, `/connections/*`, `/deployments/*` | Knowledge indexes, connections, deployments |
| `/datasets/*`, `/insights/*` | Datasets, monitoring |
| `/redTeams/runs/*`, `/schedules/*` | Red teams, scheduled jobs |

### Egress Proxy

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/egress/fetch` | Audited HTTP proxy (blocklist → allowlist → pending) |
| GET | `/egress/allowlist` | List approved domains |
| GET | `/egress/pending` | List pending approval requests |
| POST | `/egress/approve` | Approve a domain |
| POST | `/egress/deny` | Deny a pending domain |
| GET | `/egress/learned` | Domains observed in learn mode |
| POST | `/egress/learned/clear` | Clear learned domains |

### AGT Governance

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/agt/evaluate` | Policy evaluation |
| GET | `/agt/trust`, `/agt/trust/{agent_id}` | Trust store queries |
| GET | `/agt/audit`, `/agt/audit/verify` | Audit log + integrity verification |
| POST | `/agt/mesh/send` | Send inter-agent message |
| GET | `/agt/mesh/inbox` | Receive mesh messages |
| POST | `/agt/mesh/receive` | Auto-receive webhook |
| GET | `/agt/relay` | WebSocket bridge to agentmesh-relay (E2E encrypted) |
| GET/POST | `/agt/registry/*` | AgentMesh registry proxy |
| GET | `/agt/status` | Governance status |

### Sub-Agent Spawning

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/sandbox/spawn` | Create child ClawSandbox CRD |
| GET | `/sandbox/list` | List child sandboxes |
| GET | `/sandbox/{name}/status` | Child status |
| DELETE | `/sandbox/{name}` | Tear down child sandbox |

### Blocklist & Health

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/blocklist/status` | Domain count + enabled state |
| POST | `/blocklist/check` | Check domain against threat intel |
| GET | `/healthz` | Liveness probe |
| GET | `/readyz` | Deep readiness (token + Content Safety) |
| GET | `/metrics` | Prometheus metrics |
