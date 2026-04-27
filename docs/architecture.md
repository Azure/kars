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
│  │   ├─ AzureClaw plugin (tools: spawn, mesh, Foundry, http_fetch)   │     │
│  │   ├─ Python 3 (43 packages: pandas, scipy, pdfplumber, Pillow, …)   │     │
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
| enhanced (default) | (default runc) | Localhost `azureclaw-strict` (219 allowed syscalls) | `sandbox` |
| confidential | `kata-vm-isolation` | RuntimeDefault (VM is the boundary) | `sandbox-kata` |

**Isolation inheritance:** The controller exports `SANDBOX_ISOLATION` as an environment variable into every pod. When a sub-agent is spawned via `/sandbox/spawn`, the inference router reads the parent's isolation level from this env var and applies it as the default for the child. Downgrading from `confidential` to a lower isolation level is blocked — the spawn request returns an error.

**Kata auto-provisioning:** `azureclaw add --isolation confidential` checks for a Kata-capable nodepool. If none exists, it offers to provision one automatically via `az aks nodepool add` with `--workload-runtime KataVmIsolation`, SKU `Standard_D4as_v6`, and appropriate labels/taints.

### Inference Router (Rust, axum)

Per-sandbox router on port 8443. Runs as UID 1001 (unrestricted network). The agent (UID 1000) can only reach `localhost:8443` — the router is the sole external path.

**Request pipeline (inference):**

```
Agent → Token budget check (429) → IMDS/WI token (cached per scope)
      → Forward to Azure OpenAI / Foundry → Foundry guardrails (Content Safety + Prompt Shields, server-side)
      → Parse content filter annotations → Report flags to AGT → Extract usage → Prometheus metrics → Agent
```

**Embedding model routing:** The `/v1/embeddings` endpoint extracts the `model` field from the request body (e.g., `text-embedding-3-small`) and routes to that specific deployment. This prevents embedding requests from being sent to the default chat model, which would fail. Both AKS (Workload Identity) and dev (API key) code paths handle this.

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
- **Plugin:** AzureClaw tools (`spawn`, `mesh`, `Foundry`, `http_fetch`), provider `azure-openai` routing to `localhost:8443`
- **Native delegation:** Sub-agent tasks received via AGT mesh are delegated to the full OpenClaw agent loop (`openclaw agent --message`), giving sub-agents access to all registered tools (Foundry, exec, web_search, etc.)
- **Python 3:** 43 packages pre-installed — pandas, numpy, scipy, sympy, matplotlib, seaborn, requests, httpx, beautifulsoup4, lxml, cssselect, aiohttp, websockets, rich, tabulate, pdfplumber, pypdf, python-docx, openpyxl, python-pptx, Pillow, jinja2, pydantic, jsonpath-ng, xmltodict, markdown, html2text, chardet, python-dateutil, pyyaml, toml, python-dotenv, sqlalchemy, cryptography, tiktoken, dnspython, networkx, geopy, ftfy, unidecode, qrcode, fpdf2, html5lib
- **Channels:** Telegram, Slack, Discord, WhatsApp (via `/egress/fetch` proxy)
- **Filesystem:** Read-only rootfs, writable `/sandbox` + `/tmp` (emptyDir, `/tmp` is tmpfs 1Gi)
- **Explicit proxy:** `proxy-bootstrap.js` is preloaded via `NODE_OPTIONS="--require ..."` before any OpenClaw code runs. It sets undici's `EnvHttpProxyAgent` as the global fetch dispatcher so all outbound HTTP/HTTPS requests (Telegram polling, model pricing, etc.) honor `HTTPS_PROXY`/`NO_PROXY` env vars.

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
| GET | `/agt/relay` | WebSocket bridge to agentmesh-relay (E2E encrypted) |
| GET/POST | `/agt/registry/*` | AgentMesh registry proxy |
| GET | `/agt/status` | Governance status |

### AGT Mesh Connection Model

Each container maintains **one AGT mesh connection**, created by the gateway process. All other processes in the container skip mesh initialization:

| Process | AGT Mesh | Environment |
|---------|----------|-------------|
| Gateway (OpenClaw) | ✅ Creates mesh connection | Default `HOME` |
| Node host | ❌ Skips mesh init | `AGT_SKIP_INIT=1 HOME=/tmp/node-host-home` |
| Approvals command | ❌ Skips mesh init | `AGT_SKIP_INIT=1` |
| Delegated sub-agent tasks | ❌ Skips mesh init | `AGT_SKIP_INIT=1 HOME=/tmp/agt-delegate-home` |

**Why `AGT_SKIP_INIT=1`:** The AGT SDK creates a device fingerprint and Signal Protocol key store on init. Multiple processes sharing the same `HOME` would cause fingerprint conflicts and corrupt the key store. Only the gateway needs a mesh connection — it handles all inter-agent communication via the plugin's `onMessage` handler.

**Why separate `HOME` dirs:** Processes that run the OpenClaw runtime (node host, delegated tasks) get an isolated `HOME` to prevent any residual SDK state from colliding with the gateway's key material.

**Relay listener:** There is no separate relay listener process. Incoming mesh messages are handled by the AzureClaw plugin's built-in `onMessage` handler inside the gateway, which delegates tasks to the native agent loop (`openclaw agent --message`).

**Handler registration order:** All mesh handlers (`onMessage`, `onKnock`, `onError`, `onE2EVerified`) are registered BEFORE `connect()` to prevent a race condition where early messages arrive with no handler.

**No plaintext fallback:** The HTTP mesh routes (`/agt/mesh/send`, `/agt/mesh/receive`) have been removed. All inter-agent communication is E2E encrypted via the Signal Protocol relay. If encryption fails, messages are rejected — never delivered in cleartext.

### Global Registry Deployment

For cross-environment handoff (local ↔ cloud), the AgentMesh relay and registry need public endpoints. Two deployment modes:

| Mode | Flag | Registry Location | Handoff |
|------|------|-------------------|---------|
| **Local** (default) | — | In-cluster (`agentmesh` namespace) | ❌ |
| **Global** | `--global-registry <url>` | External (public endpoint) | ✅ |

**Exposing the registry:**

```
azureclaw up --expose-registry   # deploys AGIC Ingress + NetworkPolicy
```

This creates Application Gateway Ingress for `registry.<domain>` (HTTPS) and `relay.<domain>` (WSS) with Azure-managed TLS and WAF rate limiting.

**4-layer authentication chain:**

```
Internet → [WAF rate limit] → [TLS termination] → [Ed25519 signature] → [Registry lookup] → Connected
                                                         ↑                      ↑
                                                    Proves AMID              Confirms agent
                                                    ownership               is registered
```

| Layer | Component | What it stops |
|-------|-----------|---------------|
| 1. WAF | Application Gateway | DDoS, connection floods |
| 2. Ed25519 | Relay `handle_auth()` | Impersonation, replay attacks |
| 3. Registry check | Relay `RegistryVerifier` | Unregistered/anonymous/revoked agents |
| 4. OAuth | Registry `oauth.rs` | Controls who can register (GitHub, Entra ID, Google) |

**NetworkPolicy enforcement:**
- PostgreSQL: inbound only from registry pods (port 5432)
- Registry: inbound only from Application Gateway subnet + internal sandbox pods
- Relay: inbound only from Application Gateway subnet + internal sandbox pods

**Identity management:** `azureclaw mesh auth --registry <url> --provider github|entra` generates Ed25519 keypair, runs browser-based OAuth, stores encrypted identity in `~/.azureclaw/mesh-identity.json` (AES-256-GCM, machine-bound key).

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

---

## Channels Architecture

Messaging channels (Telegram, Slack, Discord, WhatsApp) connect to the agent through the gateway running inside the sandbox. All channel traffic flows through the egress proxy — the agent has no direct network access.

```
                  Telegram API ◄──┐
                  Slack API    ◄──┤
                  Discord API  ◄──┤  Egress proxy (/egress/fetch)
                  WhatsApp     ◄──┘       ▲
                                          │
┌─ Sandbox Pod ──────────────────────────────────────────────────┐
│                                                                │
│  openclaw (UID 1000)              inference-router (UID 1001)  │
│  ┌──────────────────┐            ┌──────────────────────┐      │
│  │ Gateway :18789   │            │ /egress/fetch        │      │
│  │  ├─ Telegram     │──http_fetch──►│ blocklist → allow │      │
│  │  ├─ Slack        │            │  → learn/pending     │      │
│  │  ├─ Discord      │            └──────────────────────┘      │
│  │  └─ WhatsApp     │                                          │
│  └──────────────────┘                                          │
└────────────────────────────────────────────────────────────────┘
```

Channels are enabled via CLI flags (e.g., `--channels telegram --telegram-token "..."`) which inject environment variables into the pod. The entrypoint reads these at startup and configures the gateway adapters automatically. See [channels-plugins.md](channels-plugins.md) for setup details.

---

## Plugin Auto-Discovery

The sandbox entrypoint (`sandbox-images/openclaw/entrypoint.sh`) auto-discovers plugins from environment variables at startup. No manual configuration files are needed.

**Discovery flow:**

1. CLI flags (e.g., `--brave-api-key`) or env vars (e.g., `BRAVE_API_KEY`) are set
2. On AKS, the CLI stores these as K8s secrets; the controller mounts them via `envFrom`
3. At pod startup, the entrypoint iterates through known plugin/env-var pairs
4. For each set env var, the plugin is added to `plugins.allow` and `plugins.entries` in the OpenClaw config
5. The gateway loads only the enabled plugins

**Plugin mapping:**

| Plugin ID | Environment Variable | CLI Flag |
|-----------|---------------------|----------|
| `brave` | `BRAVE_API_KEY` | `--brave-api-key` |
| `tavily` | `TAVILY_API_KEY` | `--tavily-api-key` |
| `exa` | `EXA_API_KEY` | `--exa-api-key` |
| `firecrawl` | `FIRECRAWL_API_KEY` | `--firecrawl-api-key` |
| `perplexity` | `PERPLEXITY_API_KEY` | `--perplexity-api-key` |
| `openai` | `OPENAI_API_KEY` | `--openai-api-key` |

Source: `sandbox-images/openclaw/entrypoint.sh` (plugin loop at lines 286–301)

---

## Foundry Integration

### Bing Grounding (Web Search)

The `foundry_web_search` tool uses Azure AI Foundry's Responses API with Bing Grounding. Unlike third-party plugins, it requires **no API key** — it auto-discovers the Bing connection from the Foundry project at runtime via Workload Identity.

**Setup:** Create a Bing Grounding resource in Azure Portal → connect it to your Foundry project → deploy. The tool appears automatically.

**Manual override:** If auto-discovery fails (e.g., multiple Bing connections), set `BING_CONNECTION_ID` explicitly.

See [channels-plugins.md](channels-plugins.md#foundry-web-search-bing-grounding) for detailed setup instructions.

---

## Credentials Secret Pattern

Channel tokens and plugin API keys follow a consistent pattern from CLI to running pod:

```
CLI flag (--telegram-token)
    │
    ▼
K8s Secret (azureclaw-<name>/<name>-credentials)
    │
    ▼
envFrom in pod spec (controller injects all secrets)
    │
    ▼
entrypoint.sh reads env vars → configures channels/plugins
    │
    ▼
Agent process (never sees raw credentials)
```

| Step | Component | What Happens |
|------|-----------|-------------|
| 1 | `azureclaw add` | CLI creates K8s secrets in `azureclaw-<name>` namespace |
| 2 | Controller | Mounts secrets as env vars via `envFrom` in the pod spec |
| 3 | Entrypoint | Reads env vars, configures `openclaw.json`, enables channels/plugins |
| 4 | Agent | Interacts with pre-configured channels — never handles raw tokens |

Credentials are **namespace-scoped** — each sandbox's secrets are isolated. Use `azureclaw credentials update <name>` to rotate credentials on a running sandbox (updates the K8s secret and triggers a rolling restart).

---

## Operator Dashboard

`azureclaw operator` launches a terminal-based management UI (built with blessed/blessed-contrib) that provides a live view of all agents in the cluster.

### Architecture

```
┌─ Terminal (blessed TUI) ──────────────────────────────────────────────┐
│                                                                       │
│  Agent List Panel        Security Panel      Activity Panel           │
│  ┌───────────────┐       ┌─────────────┐     ┌─────────────┐         │
│  │ ● agent-1     │       │ Isolation   │     │ ✓ Approved  │         │
│  │ └ sub-agent-1 │       │ Seccomp     │     │ ↻ Refreshed │         │
│  │ ● agent-2     │       │ Egress mode │     │ ✗ Denied    │         │
│  └───────┬───────┘       └─────────────┘     └─────────────┘         │
│          │                                                            │
│  Egress Panel             Keybindings Panel                           │
│  ┌───────────────┐       ┌──────────────────────────────────┐         │
│  │ ●P pending    │       │ [Enter] Connect  [n] Spawn       │         │
│  │ ✓A approved   │       │ [a] Approve  [d] Delete/Deny     │         │
│  │ ✗D denied     │       │ [m] Model    [e] Enforce egress  │         │
│  └───────────────┘       └──────────────────────────────────┘         │
│                                                                       │
│  ┌─ Enter key ──────────────────────────────────────────────┐         │
│  │  spawnSync("openclaw", ["tui", ...])                     │         │
│  │  Blocks Node.js event loop entirely                      │         │
│  │  Terminal buffer: normalBuffer() → child → alternateBuffer()       │
│  └──────────────────────────────────────────────────────────┘         │
└───────────────────────────────────────────────────────────────────────┘
         │                    │                    │
         ▼                    ▼                    ▼
    K8s API              kubectl exec         kubectl port-forward
   (list CRDs,          (connect to pod)      (gateway/TUI access)
    CRUD sandboxes)
```

### Features

| Feature | Key | Description |
|---------|-----|-------------|
| Agent list | `↑↓` | Hierarchical view — parent agents + indented sub-agents |
| Connect | `Enter` | Shell out to `openclaw tui` with persistent session ID |
| Spawn agent | `n` | Multi-step wizard (name, model, isolation, governance) |
| Delete agent | `d` | Confirmation dialog + CRD deletion |
| Switch model | `m` | Hot-swap model on running agent (no restart) |
| Approve egress | `a` / `Shift+A` | Approve single / all pending domains |
| Deny egress | `d` (egress panel) | Deny pending domain request |
| Enforce egress | `e` | Lock down to learned domain set |
| Cluster health | `c` | Node count, API server status, resource usage |
| Logs | `l` | Stream agent logs |
| Refresh | `r` | Manual refresh |

### Connect Shell-Out

Pressing `Enter` on an agent uses `spawnSync` to launch `openclaw tui` with `stdio: "inherit"`. This blocks the Node.js event loop entirely, ensuring blessed doesn't interfere with the child process's terminal. The operator:

1. Saves cursor position and switches to the normal terminal buffer
2. Disables raw mode on stdin
3. Runs `openclaw tui` synchronously with a persistent session ID (`operator-{agentName}`)
4. Restores raw mode, alternate buffer, and cursor position on return

SIGINT (Ctrl+C) is trapped during the child process so it only terminates the TUI session, not the operator itself.

---

## Phase 1 architectural additions (PR #44)

### Four-seam provider architecture

Every cross-AGT-boundary call goes through one of four trait contracts. The
router-side three (`PolicyDecisionProvider`, `AuditSink`, `SigningProvider`)
have in-tree implementations on `Governance`. The fourth (`MeshProvider`) is
plugin-side by design — see [`docs/agt-boundary.md`](agt-boundary.md).

```
+-------------------------------------------------+
|             inference-router/src/               |
|                                                 |
|   AppState                                      |
|     ├── Arc<dyn PolicyDecisionProvider>  ──┐    |
|     ├── Arc<dyn AuditSink>                ─┤    |
|     ├── Arc<dyn SigningProvider>          ─┤    |
|     └── Arc<Governance>  ←─────────────────┘    |
|         (single instance; impls live in         |
|          providers/{policy,audit,signing}_impl) |
|                                                 |
|   Outage dispatch:  providers/outage.rs         |
|     Strict / CachedRead / DegradedDev           |
|     per ClawSandbox.spec.agt.outageMode         |
+-------------------------------------------------+
```

### MCP 2026 module

`inference-router/src/mcp/` (8 files): `streamable_http.rs`, `jsonrpc.rs`,
`oauth.rs`, `oauth_layer.rs` (mounted as `tower::Layer`), `initialize.rs`,
`pipeline.rs`, `tools.rs`, `error.rs`. Mounted at `POST /mcp`. OAuth 2.1 BCP
gated by `McpServer.spec.productionMode: true`.

### A2A 1.0.0 module

`inference-router/src/a2a/` (14 files) — `agent_card.rs`, `agent_projection.rs`,
`card_server.rs` (`/.well-known/agent.json`), `card_signing.rs`,
`card_verifier.rs`, `error.rs`, `jsonrpc_dispatch.rs`, `signature.rs`,
`snapshot_rebuild.rs`, `trust_store.rs`, plus AP2: `ap2.rs`,
`mandate_signing.rs`, `mandate_trust_store.rs`, `message_send_ap2.rs`. Schema:
<https://a2a-protocol.org/v1.0.0/specification>. Default ingress is no public
exposure; surgical opt-in via `ClawSandbox.spec.a2a.expose: true` — see
[ADR-0001](adr/0001-a2a-ingress-front-edge.md).

### CRD reconciliation status

| CRD | Reconciled | File | Notes |
|---|---|---|---|
| `ClawSandbox` | ✅ | `controller/src/reconciler/mod.rs` (1464 LOC) | Status subresource (KEP-1623 conditions + `observedGeneration`) |
| `ClawPairing` | ✅ | `controller/src/{pairing,pairing_reconciler}.rs` | Operator-assisted pairing as a K8s op |
| `McpServer` | schema-only (Phase 1) | `controller/src/mcp_server.rs` | Reconciliation in Phase 2; CEL via `crd_validations.rs` |
| `ToolPolicy` | schema-only (Phase 1) | `controller/src/tool_policy.rs` | Carries AP2 `commerce.{dailyCap,monthlyCap,counterpartyAllowlist}`; reconciliation in Phase 2 |

**Note on CEL.** kube-rs `CustomResource` derive does not emit the
`x-kubernetes-validations` field (kube-rs#1557), so CEL is post-processed in
`controller/src/crd_validations.rs` after schema generation.

### VAP / MAP set

Shipped in the controller Helm chart (`deploy/helm/azureclaw/templates/`):

- **VAP:** `pods/exec|attach|portforward` denied on sandbox namespaces;
  posture-downgrades blocked (isolation step-down, seccomp removal,
  `readOnlyRootFilesystem: false`); `azureclaw.azure.com/dev-only` label
  cannot be removed once applied; `provider: null/noop/disabled` requires
  `dev-only` label (mirror of `ci/no-null-provider-prod.sh`).
- **MAP:** auto-inject router sidecar on `azureclaw.azure.com/inject-router=true`
  pods; auto-set seccomp to `azureclaw-strict` if missing.

### Status subresource (KEP-1623)

`ClawSandbox.status` carries `conditions[]` (`Ready`, `Degraded`,
`Reconciling`, `Available`) and `observedGeneration`. Controller stamps
`Degraded=True` / `Ready=False` on the three validation-failure exits.
Code: `controller/src/status/{mod,conditions}.rs`.

### Identity provider seam — Microsoft Graph agent identity

`controller/src/providers/identity_*.rs` ships a production Graph client
calling:

- `POST /beta/servicePrincipals/microsoft.graph.agentIdentity` — provision
  agent identity SP
- `POST /beta/servicePrincipals/{id}/federatedIdentityCredentials` — bind
  fedcred for sandbox SA
- `DELETE /beta/servicePrincipals/{id}` — teardown on `ClawSandbox` deletion

Endpoints verified against `learn.microsoft.com` (commit `2114bf2`).

### Policy hot-reload

The router subscribes to `ToolPolicy` / `InferencePolicy` via K8s informers +
AGT SSE; new policy applies in-process without pod rollout. Flipping
`spec.agt.providers.{policy,audit,signing}` between `vendored` and `agt` also
hot-reloads (no rollout). Policy-change propagation is asserted within 5 s by
the conformance corpus.

### OTel GenAI SemConv 1.x

Every router span emits OTel GenAI SemConv 1.x attributes
(`gen_ai.system`, `gen_ai.request.model`, `gen_ai.usage.{input,output}_tokens`,
…). Enabled by default; export via `OTEL_EXPORTER_OTLP_ENDPOINT`.

### Federated-credential reaper

`controller/src/fedcred_reaper.rs` is the 4th `tokio::select!` arm of the
controller event loop (232 LOC, 5 unit tests). It periodically lists Azure
managed-identity federated credentials owned by the controller, cross-checks
against live `ClawSandbox` resources, and deletes orphans. Default cadence is
600 s; override via `FEDCRED_REAPER_INTERVAL_SECS`. This guards against the
**20-fedcred-per-MI Azure cap** that would otherwise block sandbox creation
once enough churn has accumulated.

### Gateway token via `secretKeyRef`

`OPENCLAW_GATEWAY_TOKEN` is mounted from a K8s `Secret` rather than plain env,
so a pod-spec leak no longer surfaces the token. A one-shot `warn!` is
emitted if a legacy plain-env path is exercised, so operators can migrate
in-flight tenants without breaking them.

### `registrationMode == full` gating

Mesh-side registration runs in `full` mode only when both relay and registry
are reachable; if registry is degraded, the controller falls back to relay-only
mode and stamps `Degraded=True` on `ClawSandbox.status.conditions`.
