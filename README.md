<div align="center">

# 🔱 AzureClaw

**Secure AI Agent Runtime for Azure**

[![License: MIT](https://img.shields.io/badge/License-MIT-0078D4.svg)](LICENSE)
[![CI](https://github.com/Azure/azureclaw/actions/workflows/ci.yml/badge.svg)](https://github.com/Azure/azureclaw/actions/workflows/ci.yml)
[![Azure](https://img.shields.io/badge/Azure-AKS%20%7C%20Foundry%20%7C%20Kata-0078D4)](https://azure.microsoft.com)

Run AI agents in hardened sandboxes on AKS with defense-in-depth security.<br>
Zero-credential inference through Azure AI Foundry. Optional Kata VM isolation. Multi-agent governance via AGT.

</div>

---

> **Not a fork.** AzureClaw extends [OpenClaw](https://openclaw.ai) using its native
> plugin API and `tools.deny` config — no OpenClaw source is modified, patched, or
> vendored. Any upstream OpenClaw release is drop-in compatible. See
> [Upstream Alignment](docs/upstream-alignment.md) for the full rationale.

---

## What is AzureClaw?

AzureClaw is a **secure runtime for AI agents on Azure Kubernetes Service**. It answers a single question: *how do you give an AI agent real tools without giving it the keys to the kingdom?*

Every agent runs inside a hardened sandbox pod. A Rust inference router sits in front of every external call — Azure model inference, web fetches, peer messaging — and applies **defense-in-depth controls** at the network, kernel, identity, content-safety, and governance layers. Agents never see Azure credentials. All inter-agent messaging is end-to-end encrypted with the Signal Protocol. One CLI command (`azureclaw up`) takes you from zero to a fully provisioned, secured runtime.

AzureClaw is **not a fork of OpenClaw** — it extends OpenClaw via its native plugin API and `tools.deny` config, so any upstream OpenClaw release is drop-in compatible. See [Upstream Alignment](docs/upstream-alignment.md).

### Who is this for?

- **Platform teams** who need to host LLM agents on AKS with the same operational rigour as the rest of their workloads — namespace isolation, RBAC, NetworkPolicies, audit, signed admission.
- **Security teams** who want a single, opinionated, layered control plane (egress, content safety, governance, mesh trust) instead of stitching point products together.
- **Agent builders** who want to ship without writing the boring-but-load-bearing infrastructure: identity, secret rotation, policy, trust, audit, multi-tenant isolation.

### What problems does it solve?

1. **Credential blast radius** — agents talk to Azure via Workload Identity through the router, not via API keys. Compromise of an agent does not compromise the cloud account.
2. **Tool-call governance** — every shell exec / HTTP fetch / sub-agent spawn passes through a policy decision point with audit. No invisible side effects.
3. **Inter-agent trust** — agents talk over a Signal-Protocol mesh with explicit KNOCK trust handshake, trust scoring, and tamper-evident audit chain. No plaintext fallback.
4. **Operational footprint** — `azureclaw up` provisions AKS + ACR + Foundry + Foundry-side Content Safety + sandbox in one go; `azureclaw operator` gives a live TUI for running fleets.
5. **Multi-runtime future** — see [Roadmap](#roadmap-extending-beyond-openclaw) below: protocol scaffolding (MCP, A2A, AP2) is in place so the same sandbox can host non-OpenClaw agents over the wire.

> 📖 **See [`docs/use-cases.md`](docs/use-cases.md)** for the four end-to-end scenarios — AzureClaw-native agents, **any-OpenClaw → AzureClaw cloud offload** (no AzureClaw CLI on the laptop), AzureClaw ↔ AzureClaw mesh, and the roadmap for non-OpenClaw runtimes via MCP / A2A / AP2. For deployment shapes (developer inner-loop, enterprise self-hosted, managed public offload, cross-org federation, sovereign / air-gapped) with topology + trust-boundary + flow diagrams, see [`docs/blueprints/`](docs/blueprints/00-index.md).

---

## Architecture

```
                    ┌──────────┐
                    │ User     │
                    │ TUI / TG │
                    └────┬─────┘
                         │
   ┌─────────────────────▼───────────────────────────────────────────────────┐
   │  AKS Cluster (Azure Linux · Cilium)                                     │
   │                                                                         │
   │  ┌─ Sandbox Pod (per agent) ─────────────────────────────────────────┐  │
   │  │                                                                   │  │
   │  │  init: egress-guard (iptables)                                    │  │
   │  │   └─ agent process → localhost + DNS only                         │  │
   │  │                                                                   │  │
   │  │  ┌──────────────┐   localhost    ┌──────────────────┐             │  │
   │  │  │  OpenClaw    │──────:8443────►│ Inference Router  │────────────┼──┼──► Azure AI Foundry
   │  │  │  (agent)     │               │ (Rust)            │            │  │     (200+ models)
   │  │  └──────────────┘               │                   │            │  │
   │  │   read-only rootfs              │ • WI/IMDS auth    │            │  │
   │  │   drop ALL caps                 │ • Content Safety   │            │  │
   │  │   no Azure credentials          │ • Token budgets    │            │  │
   │  │                                 │ • Domain blocklist │            │  │
   │  │                                 │ • Egress proxy     │            │  │
   │  │                                 │ • AGT governance   │            │  │
   │  │                                 │   (native Rust)    │            │  │
   │  │                                 │   • PolicyEngine   │            │  │
   │  │                                 │   • TrustManager   │            │  │
   │  │                                 │   • AuditLogger    │            │  │
   │  │                                 │   • RateLimiter    │            │  │
   │  │                                 └────────────────────┘            │  │
   │  │  NetworkPolicy: default-deny egress                               │  │
   │  └───────────────────────────────────────────────────────────────────┘  │
   │                                                                         │
   │  ┌─ AgentMesh ───────────────────────────────────────────────────────┐  │
   │  │  agent-alpha ◄──E2E encrypted──► agent-beta                       │  │
   │  │  (Signal Protocol · X3DH + Double Ratchet · trust-gated)          │  │
   │  │                                                                    │  │
   │  │  agentmesh-relay (WebSocket :8765) — routes encrypted messages     │  │
   │  │  agentmesh-registry (REST :8080 + PostgreSQL) — discovery/prekeys  │  │
   │  └───────────────────────────────────────────────────────────────────┘  │
   │                                                                         │
   │  Controller (Rust/kube-rs) — reconciles ClawSandbox CRDs               │
   └─────────────────────────────────────────────────────────────────────────┘
```

> 📐 **[Architecture & Flow Diagrams](docs/architecture-diagrams.md)** — Mermaid diagrams for all core flows: pod architecture, agent creation, sub-agent spawning, E2E encrypted communication, inference routing, egress control, bidirectional handoff with sub-agents, and defense-in-depth layers.

---

## 🚀 Get started in 60 seconds

```bash
# Clone, install CLI
git clone https://github.com/Azure/azureclaw.git && cd azureclaw/cli
npm install && npm run build && npm link

# Local dev (Docker, no Azure needed)
azureclaw dev

# Or deploy to AKS (provisions AKS + ACR + Foundry end-to-end)
azureclaw up
```

Full instructions, prerequisites, and the **Path A (local Docker)** vs **Path B (production AKS)** breakdown are in the [Quick Start](#quick-start) section below.

---

## Docker Images

| Image | Language | Purpose |
|-------|----------|---------|
| `azureclaw-controller` | Rust | K8s operator — reconciles `ClawSandbox` + `ClawPairing` CRDs into pods; periodic federated-credential reaper GCs orphan credentials against the Azure 20-fedcred-per-MI cap |
| `azureclaw-inference-router` | Rust | Inference proxy — Workload Identity auth, Content Safety, AGT governance, egress filtering |
| `azureclaw-sandbox` (built from `sandbox-images/openclaw`) | Node.js | Main agent container (OpenClaw + AGT SDK + Python tools) |
| `agentmesh-relay` | Rust | WebSocket relay for E2E encrypted inter-agent messaging — see *AgentMesh & vendoring* below |
| `agentmesh-registry` | Rust + PostgreSQL | Agent discovery, prekey storage, React admin UI — see *AgentMesh & vendoring* below |

`azureclaw push` builds the 5 images above by default. The shared
`sandbox-base` image is built only when `--include-base` is passed. A separate
`sandbox-images/nemoclaw/` image exists for any-OpenClaw-host clients (laptop,
NemoClaw, etc.) that want to offload to AzureClaw — see
[`docs/any-openclaw-cloud-offload.md`](docs/any-openclaw-cloud-offload.md).

All images build on Azure Linux 3 (`mcr.microsoft.com/azurelinux/base/core:3.0`).

---

## Key Features

### 🔒 Security (defense-in-depth)

**Always on (all isolation levels):**

- **iptables egress guard** — agent process can only reach `localhost`; all external traffic forced through router
- **NetworkPolicy** — default-deny egress at the cluster level
- **Read-only rootfs** — non-root, drop ALL capabilities, no privilege escalation
- **Domain blocklist** — 51k+ domains auto-refreshed from OISD + URLhaus every 6h
- **Content Safety + Prompt Shields** — Azure AI Content Safety on every inference call
- **Zero Azure credentials** — agents never see Azure auth tokens; the router authenticates via IMDS/Workload Identity

**Per isolation level (`--isolation`):**

| Level | Runtime | What it adds |
|-------|---------|-------------|
| `standard` | runc | Kernel-default seccomp filter |
| `enhanced` (default) | runc | Custom strict seccomp profile (~219 allowed syscalls) |
| `confidential` | Kata VM | Per-pod dedicated kernel on AMD SEV-SNP hardware — container escapes hit a VM boundary |

> **Note on plugin credentials:** Channel tokens (Telegram, Slack) and third-party API keys (Brave, Tavily) are accessible to the agent process — plugins need them to function. However, the agent cannot exfiltrate them: iptables blocks all outbound traffic except through the governed router. Azure auth tokens remain isolated in the router at all times.

### 🤖 AI Agent

- **Messaging channels** — Telegram, Slack, Discord, WhatsApp (auto-configured via CLI flags)
- **Third-party plugins** — Brave, Tavily, Exa, Firecrawl, Perplexity (API key → auto-enabled)
- **Foundry web search** — Bing Grounding via Responses API (zero-config, no API key needed)
- **Sub-agent spawning** — agents create child agents via CRD (isolated, governed, full tool access via native delegation)
- **10 Foundry skills** — web search (Bing Grounding), code execute, image generation, file search, memory, conversations, evaluations, knowledge, and more — all via Workload Identity (no API keys)
- **Python 3** — 43 packages pre-installed: pandas, numpy, scipy, matplotlib, pdfplumber, pypdf, python-docx, openpyxl, python-pptx, Pillow, sqlalchemy, tiktoken, cryptography, networkx, and more
- **200+ models** — hot-switch between GPT-4.1, GPT-5-mini, DeepSeek-V3.2, Phi-4, Llama, etc.
- **Multi-frontend** — TUI, Telegram, Web UI at `localhost:18789`

### 🏛️ Governance (AGT — native Rust)

- **Native governance** — policy evaluation, trust management, and audit run in-process inside the Rust inference router (no sidecar, <1µs eval latency)
- **Trust scoring** — per-agent scores 0–1000, threshold 500, clamped ±200/update, Ed25519 signed
- **Policy engine** — YAML-driven rules (hot-reloaded) covering shell safety, inference rate-limiting, content safety, mesh trust gates
- **Audit trail** — SHA-256 Merkle tree append-only chain with tamper detection and integrity verification
- **Components** — PolicyEngine, TrustManager, AuditLogger, RateLimiter, BehaviorMonitor (native Rust, compiled into the inference router)
- **Prometheus metrics** — `azureclaw_agt_policy_evaluations_total`, `azureclaw_agt_eval_latency_seconds`, `azureclaw_agt_behavior_alerts_total`, and more

### 🔐 E2E Encryption (Signal Protocol)

- **X3DH key exchange** — identity, signed-prekey, and one-time prekey bundles for session setup
- **Double Ratchet** — per-message forward secrecy via ratchet rotation
- **KNOCK protocol** — policy-gated session establishment (trust score ≥ 500 required)
- **AgentMesh relay** — untrusted WebSocket relay (`:8765`) routes encrypted payloads without decryption
- **AgentMesh registry** — agent discovery and prekey storage (REST `:8080` + PostgreSQL)

### ⚙️ Operations

- **One-command deploy** — `azureclaw up` provisions AKS + ACR + Foundry + sandbox end-to-end, with a preflight RBAC check that fails fast (~30 s) if your Azure permissions are insufficient
- **Live handoff** — `azureclaw handoff <name> --to cloud|local` migrates agents between local Docker and AKS with sub-agent state, E2E encrypted workspace transfer, and task resumption
- **Operator dashboard** — `azureclaw operator` launches a live TUI for managing all agents
- **Credential management** — `azureclaw credentials update` rotates tokens for running sandboxes; gateway tokens are mounted via `secretKeyRef`, never in plain pod env
- **Image pipeline** — `azureclaw push` builds and pushes images to ACR with optional rollout
- **Monitoring** — Prometheus metrics, OpenTelemetry GenAI semantic conventions on every router span, Log Analytics, eBPF tracing via `azureclaw trace`
- **Federated-credential reaper** — controller periodically GCs orphan federated credentials so sandbox managed identities never hit the Azure 20-fedcred-per-MI cap

---

## Roadmap — extending beyond OpenClaw

AzureClaw was built first as the secure runtime for OpenClaw agents. The next chapter is making it the secure runtime for **any** agent framework that speaks open protocols — so platform teams can host SDK-native agents (Foundry, OpenAI Agents SDK, Anthropic Agent SDK, Google ADK, Strands) on the same AKS substrate, with the same governance and isolation guarantees.

The protocol scaffolding for that future is being landed now in tightly scoped, well-audited modules. **Most of it is not yet wired into a default-on user-facing flow** — it is intentionally opt-in and gated, so existing OpenClaw deployments are unaffected:

| Surface | Status | What it enables |
|---|---|---|
| **MCP 2026 Streamable HTTP** | Scaffolded in `inference-router/src/mcp/`; off by default | A future `McpServer` CRD lets cluster operators publish private/internal MCP tools to agents over OAuth 2.1 |
| **A2A 1.0.0 (Agent-to-Agent)** | Scaffolded in `inference-router/src/a2a/`; ingress is gateway-only and opt-in via `ClawSandbox.spec.a2a.expose: true` ([ADR-0001](docs/adr/0001-a2a-ingress-front-edge.md)) | Future cross-vendor agent interop with signed Agent Cards |
| **AP2 commerce mandates** | Scaffolded alongside A2A | Future signed-mandate trust boundary for agentic commerce |
| **Pluggable governance providers** | `PolicyDecisionProvider`, `AuditSink`, `SigningProvider` traits live; in-tree implementations are the production path today | Future swap-in of AGT's Rust SDK alternates without rewriting call sites; multi-tenant per-capability provider selection |
| **`McpServer` / `ToolPolicy` CRDs** | Schema-only in this branch; reconcilers planned for the next phase | Declarative tool-server publication and per-tool policy (rate limits, commerce caps, allowlists) |

The full plan for these surfaces — what is implemented today, what is wiring-pending, and what is deferred — is captured in [`docs/phase-0-1-capabilities.md`](docs/phase-0-1-capabilities.md). For the four end-to-end scenarios — three shipping today, plus a roadmap track for non-OpenClaw runtimes — see [`docs/use-cases.md`](docs/use-cases.md).

---

## AgentMesh & vendoring (transitional)

Inter-agent messaging today runs on a vendored fork of [AgentMesh](https://github.com/amitayks/agentmesh) (relay + registry + SDK). AgentMesh is pre-release; while integrating it we contributed bug fixes and protocol corrections that are tracked in this tree until they land upstream. Each fix is documented in `vendor/<component>/README.md`, and an index lives at [`docs/agt-vendored-patch-audit.md`](docs/agt-vendored-patch-audit.md).

**Direction of travel:** Microsoft's Agent Governance Toolkit (AGT) is shipping a first-party AgentMesh transport. Once it stabilises, AzureClaw's `MeshProvider` seam (defined plugin-side; the router has no in-tree mesh implementation) will allow operators to switch to the AGT mesh per-tenant without breaking existing deployments. Until then, the vendored stack is the supported production path.

---

## Engineering & quality posture

We treat security and code health as product-grade concerns:

- **Six blocking CI gates** — LOC budget, anti-stub (no `TODO`/`unimplemented!` on production paths), no custom crypto outside provider seams, no `Null*` providers in production, mandatory security-audit document per capability-introducing PR, vendored-patch re-audit on every AGT SDK bump.
- **Per-capability security audits** — every PR that introduces a new CRD, router route, admission policy, or sandbox-image change ships a `docs/security-audits/<date>-<slug>.md` with threat-model delta, OWASP mapping, AuthN/Z path, secret custody, audit events, failure mode, and two engineer sign-offs.
- **Behavioral conformance corpus** — `tests/conformance/` covers Signal Protocol (X3DH / KNOCK / negative cases), sandbox isolation, and the protocol scaffolding above with mandatory negative tests (tampered ciphertext, replayed message, expired mandate).
- **Compat suite** — `tests/compat/` regression-tests user-visible flows (today: the operator TUI; growing per phase).
- **Fuzz targets** — cargo-fuzz coverage for handoff state deserialization, chat sanitisation, JWS parsing, base64url decoding, streaming response parsing.

A complete inventory of these controls is in [`docs/phase-0-1-capabilities.md`](docs/phase-0-1-capabilities.md).

---

## Quick Start

### Prerequisites

| Tool | Version | Required For |
|------|---------|--------------|
| Node.js | 22+ | CLI (both paths) |
| Docker | Latest | Local dev + image builds |
| Azure CLI | 2.60+ | AKS path only |
| kubectl | 1.28+ | AKS path only |
| Helm | 3.14+ | AKS path only |
| Rust | 1.88+ (edition 2024) | Building from source (both paths) |

> **Azure RBAC:** `azureclaw up` needs `Contributor` **and** `User Access Administrator` at subscription scope (or `Owner`). See [`docs/permissions.md`](docs/permissions.md) for the full breakdown, a least-privilege custom role, and common failure modes. The CLI runs a preflight check automatically and fails fast in ≤30s if anything is missing.

### Step 1: Install the CLI

```bash
git clone https://github.com/Azure/azureclaw.git
cd azureclaw

# Build the CLI
cd cli && npm ci && npm run build && npm link
cd ..

# Verify
azureclaw --help
```

---

### Path A: Local Dev (Docker) — no Azure needed

Start a sandboxed agent locally in under a minute:

```bash
# Build the sandbox image and start it
azureclaw dev --build

# First run will prompt for Azure OpenAI credentials:
#   Endpoint: https://your-resource.openai.azure.com
#   API Key:  sk-...
# Or set them beforehand:
azureclaw credentials

# You're now in a chat session with a governed AI agent
🦞 You: What's the latest news about AI security?
```

**Optional enhancements:**

```bash
# Add Telegram channel
azureclaw dev --build --channels telegram --telegram-token "BOT_TOKEN"

# Add third-party search plugins
azureclaw dev --build --brave-api-key "KEY" --tavily-api-key "KEY"

# Use a specific model
azureclaw dev --build --model gpt-5-mini
```

> **What happens:** Docker builds the sandbox image (Azure Linux 3 + inference router + OpenClaw), starts a container with iptables egress filtering, and connects you via the TUI. No Kubernetes needed.

---

### Path B: Deploy to AKS (Production)

Full production deployment with all 9 security layers:

```bash
# 1. Login to Azure
az login

# 2. Build all 5 container images (controller, router, sandbox, relay, registry)
#    First run takes ~10 min; subsequent builds are cached
azureclaw push

# 3. Deploy everything — AKS cluster, ACR, Key Vault, Azure OpenAI, Helm chart
#    Prompts for region, subscription, and agent name
azureclaw up

# 4. Verify the cluster is healthy
azureclaw operator    # live TUI — press 'c' for cluster health

# 5. Connect to your first agent
azureclaw connect my-assistant
```

**What `azureclaw up` does (in order):**
1. Preflight checks (az, kubectl, helm, subscription, SKU availability)
2. Deploys Azure infrastructure via Bicep (AKS, ACR, Key Vault, AOAI)
3. Installs AzureClaw Helm chart (CRD, controller, RBAC, seccomp profiles)
4. Deploys AgentMesh (relay + registry) for E2E encrypted inter-agent comms
5. Creates your first agent sandbox with native AGT governance

**After deployment:**

```bash
# Add more agents
azureclaw add research-bot --model gpt-4.1 --governance --learn-egress

# Add channels
azureclaw credentials update research-bot --telegram-token "BOT_TOKEN"

# Add a confidential agent (auto-provisions Kata nodepool)
azureclaw add helper --model gpt-5-mini --isolation confidential

# Review egress activity
azureclaw egress research-bot --learned

# Multi-agent communication (E2E encrypted)
azureclaw connect research-bot
🦞 You: @helper can you review this code for security issues?
```

### Operator Dashboard

```bash
azureclaw operator
```

Live TUI for managing all agents across your cluster:

```
┌─────────────── 🔱 AzureClaw Operator │ azureclaw-aks │ ● API 5/5 ──┐
│ ● research-bot    Running   gpt-4.1     enhanced   tg    2h         │
│ └ kernel-checker  Running   gpt-5-mini  enhanced          45m       │
│ ● helper          Running   gpt-5-mini  confidential      30m       │
├── Security ────────┬── Egress ──────────┬── Activity ────────────────┤
│ Isolation enhanced │ ●P api.openai.com  │ ✓ Approved 3 domains      │
│ Seccomp   strict   │ ✓A graph.microsoft │ ↻ Refreshed 3 agents      │
├────────────────────┴────────────────────┴────────────────────────────┤
│ [Tab] Focus [↑↓] Nav [Enter] Connect [c] Cluster [n] Spawn [q] Quit│
└─────────────────────────────────────────────────────────────────────-┘
```

**Keyboard:** `Enter` connect to agent TUI · `Tab` switch panels · `a` approve egress · `Shift+A` approve all · `d` delete/deny · `e` enforce egress · `n` spawn agent · `m` switch model · `c` cluster health · `l` logs · `r` refresh · `q` quit

### Update Credentials

Rotate channel tokens or plugin API keys on running sandboxes without redeploying:

```bash
azureclaw credentials update my-agent \
  --telegram-token "NEW_TOKEN" \
  --brave-api-key "NEW_KEY"
```

---

## CLI Reference

`azureclaw` ships **21 commands** (`cli/src/commands/`):
`a2a · add · connect · convert · credentials · destroy · dev · egress · eval · handoff · list · logs · mesh · model · operator · pair · policy · push · status · trace · up`.

| Command | Description |
|---|---|
| **Lifecycle** | |
| `azureclaw up` | Deploy full stack — preflight, AKS + ACR + Foundry + sandbox |
| `azureclaw up --upgrade` | Fast upgrade — reuse cached context, Helm + RBAC + fedcred sync |
| `azureclaw dev` | Local Docker sandbox with same security controls |
| `azureclaw add <name>` | Add sandbox to existing cluster |
| `azureclaw destroy [name]` | Tear down sandbox or entire resource group (`--all`) |
| `azureclaw push` | Build and push 5 images to ACR (`--apply` restarts deployments, `--only <image>` for single image, `--include-base` to also build the shared base) |
| `azureclaw convert` | Skeleton (Phase 0) — translate between Native and `sigs/agent-sandbox` shapes; full converter in Phase 2 |
| **Operations** | |
| `azureclaw operator` | Live TUI dashboard — agents, egress, security, cluster health |
| `azureclaw connect <name>` | TUI, shell (`--shell`), or Web UI (`--web`) — surfaces `kubectl` stderr on port-forward failure |
| `azureclaw handoff <name> --to cloud` | Live-migrate agent + sub-agents from local Docker to AKS |
| `azureclaw handoff <name> --to local` | Live-migrate agent + sub-agents from AKS back to local Docker |
| `azureclaw handoff <name> --status` | Show current handoff progress |
| `azureclaw status <name>` | Health, model, tokens used |
| `azureclaw list` | All sandboxes across Docker and AKS |
| `azureclaw logs <name>` | Stream logs (`-f`, `--service router\|gateway\|openclaw`) |
| **Configuration** | |
| `azureclaw credentials` | Set Azure OpenAI credentials (interactive) |
| `azureclaw credentials update <name>` | Rotate channel/plugin keys on running sandbox |
| `azureclaw model set <name> <model>` | Switch model (hot-swap, no restart) |
| `azureclaw model get <name>` | Show current model |
| `azureclaw model list [name]` | List available Foundry models |
| `azureclaw policy allow <name> <host>` | Add allowed egress endpoint |
| `azureclaw policy get <name>` | Show active policy |
| `azureclaw policy deny <name> <host>` | Remove allowed endpoint |
| `azureclaw egress <name>` | Egress management (`--learned`, `--pending`, `--approve`, `--enforce`) |
| **Observability** | |
| `azureclaw trace <name>` | eBPF tracing (`--network`, `--dns`, `--files`, `--exec`) |
| `azureclaw eval <name>` | Run Foundry evaluations against agent |
| **Multi-Agent** | |
| `azureclaw mesh auth` | Authenticate with global AgentMesh registry (OAuth) |
| `azureclaw mesh status` | Show mesh connectivity and registered agents |
| `azureclaw mesh send <amid>` | Send E2E encrypted message to another agent |
| `azureclaw pair <a> <b>` | Pair two existing sandboxes via `ClawPairing` CR |
| `azureclaw a2a list-exposed` | List sandboxes that expose A2A 1.0.0 (Phase 1 scaffold) |
| `azureclaw a2a schema` | Print the local A2A schema (Phase 1 scaffold) |

### Common Flags

These flags are shared across `dev`, `add`, and `credentials update`:

| Flag | Description |
|---|---|
| `--channels telegram,slack,discord,whatsapp` | Enable messaging channels |
| `--telegram-token`, `--slack-token`, `--discord-token` | Channel credentials |
| `--brave-api-key`, `--tavily-api-key`, `--exa-api-key` | Search plugins |
| `--firecrawl-api-key`, `--perplexity-api-key`, `--openai-api-key` | Additional plugins |
| `--governance` / `--no-governance` | AGT governance (trust, policy, audit) |
| `--learn-egress` | Enable egress learn mode |
| `--isolation standard\|enhanced\|confidential` | Pod isolation level |
| `--model <model>` | AI model (default: `gpt-4.1`) |

---

## Channels & Plugins

### Messaging Channels

| Channel | Flag | Credential |
|---------|------|-----------|
| Telegram | `--channels telegram` | `--telegram-token` (BotFather) |
| Slack | `--channels slack` | `--slack-token` (Bot OAuth) |
| Discord | `--channels discord` | `--discord-token` |
| WhatsApp | `--channels whatsapp` | QR code pairing at runtime |

On AKS, channel tokens are stored as K8s secrets and injected into the sandbox pod automatically.

### Third-Party Plugins

| Plugin | Flag |
|--------|------|
| Brave Search | `--brave-api-key` |
| Tavily | `--tavily-api-key` |
| Exa | `--exa-api-key` |
| Firecrawl | `--firecrawl-api-key` |
| Perplexity | `--perplexity-api-key` |
| OpenAI | `--openai-api-key` |

Plugins auto-activate when their API key is present. No additional configuration needed.

### Foundry Web Search (Bing Grounding)

Built-in web search via Azure AI Foundry's Responses API. **No API key needed** — auto-discovers the Foundry project's Bing connection at runtime.

See [docs/channels-plugins.md](docs/channels-plugins.md) for setup and details.

---

## Documentation

| Document | Description |
|---|---|
| [Use Cases](docs/use-cases.md) | Three canonical scenarios: AzureClaw-native, any-OpenClaw → AzureClaw offload, AzureClaw ↔ AzureClaw mesh |
| [Phase 0 + 1 Capability Index](docs/phase-0-1-capabilities.md) | Evidence-based manifest for PR #44; every claim cites code + audit doc |
| [Architecture](docs/architecture.md) | Component design, CRD schema, API routes, four-seam providers, MCP/A2A modules, operator dashboard, auth flow |
| [Architecture Diagrams](docs/architecture-diagrams.md) | Mermaid flow diagrams: pod layout, agent creation, spawn, mesh, egress, inference |
| [Security](docs/security.md) | Defense-in-depth model, OWASP coverage, threat mitigations, CI gates, security-audit framework |
| [Threat Model — Routes](docs/threat-model.md) | Per-route auth tier, input validation, blast-radius analysis |
| [AGT Vendored-Patch Audit](docs/agt-vendored-patch-audit.md) | Index of fixes applied to the vendored AgentMesh stack pending AGT mesh shipping |
| [`sigs/agent-sandbox` Compat](docs/sigs-agent-sandbox-compat.md) | Translate / Overlay mode design; opt-in, no upstream dependency |
| [OWASP MCP Top 10 (2025)](docs/security-mcp-top10.md) | Controls matrix for the new MCP 2026 surface |
| [ADR-0001 — A2A ingress front-edge](docs/adr/0001-a2a-ingress-front-edge.md) | Gateway-only, surgical opt-in posture for inbound A2A |
| [Channels & Plugins](docs/channels-plugins.md) | Telegram, Slack, Discord, search plugins, Foundry Bing |
| [Egress Proxy](docs/egress-proxy.md) | Blocklist, allowlist, learn mode, approval flow |
| [E2E Encryption](docs/e2e-encryption-proof.md) | Signal Protocol inter-agent encryption proof |
| [Multi-Tenant](docs/multi-tenant.md) | Namespace isolation, credential and channel separation |
| [Security Validation](docs/security-validation.md) | Live cluster evidence for every security layer |
| [Permissions](docs/permissions.md) | Required Azure RBAC for `azureclaw up` |
| [Demo](docs/DEMO.md) | "Operation Claw Shield" — multi-tenant attack simulation |

---

## Project Structure

```
azureclaw/
├── ci/                   # 6 blocking CI gates + LOC budget
├── cli/                  # TypeScript CLI (azureclaw — 21 commands)
│   ├── skills/           # Foundry skill definitions (10 skills: 8 Foundry + agt-governance + azureclaw-spawn)
│   └── policies/         # AGT governance policy YAML (default rules)
├── controller/           # Rust K8s operator
│   └── src/{crd,reconciler,mesh_peer,status,providers,fedcred,fedcred_reaper}.rs
├── inference-router/     # Rust inference proxy (axum)
│   └── src/{a2a,mcp,providers,routes,handoff,governance,...}/
│   └── fuzz/             # 5 cargo-fuzz targets
├── sandbox-images/       # OpenClaw + nemoclaw container images
├── policy-engine/        # Seccomp profiles & security policies
├── deploy/               # Bicep IaC, Helm charts (incl. VAP/MAP set), AgentMesh K8s manifests
├── docs/                 # Architecture, security, threat model, ADRs, security-audits/
├── examples/             # Sample agents (basic, confidential, telegram, demo)
├── tests/                # compat/, conformance/, e2e/
└── vendor/               # AgentMesh SDK (21 patches), relay (4), registry (1)
```

> **About `vendor/`:** AzureClaw is *not* a fork of OpenClaw. The `vendor/` directory only carries our patched copies of the pre-release AgentMesh stack (relay, registry, SDK) — see *AgentMesh & vendoring* above. Each patch is documented in `vendor/<component>/README.md`, indexed in [`docs/agt-vendored-patch-audit.md`](docs/agt-vendored-patch-audit.md), and re-validated on every AGT SDK version bump.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build instructions, test process, and PR guidelines.

```bash
make build    # Rust + TypeScript
make test     # Unit tests
make lint     # clippy + oxlint
make images   # Docker images
```

---

## License

[MIT](LICENSE) · [Code of Conduct](CODE_OF_CONDUCT.md)
