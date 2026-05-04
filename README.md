<div align="center">

# 🔱 AzureClaw

**Secure AI Agent Runtime for Azure**

[![License: MIT](https://img.shields.io/badge/License-MIT-0078D4.svg)](LICENSE)
[![CI](https://github.com/Azure/azureclaw/actions/workflows/ci.yml/badge.svg)](https://github.com/Azure/azureclaw/actions/workflows/ci.yml)
[![Azure](https://img.shields.io/badge/Azure-AKS%20%7C%20Foundry%20%7C%20Kata-0078D4)](https://azure.microsoft.com)

Run AI agents in hardened sandboxes on AKS with defense-in-depth security.<br>
Zero-credential inference through Azure AI Foundry. Optional Kata VM isolation. Multi-agent governance via AGT.<br>
Eight Kubernetes CRDs, a public-ingress A2A gateway, and an inference router that runs in the data path of every external call.

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

> **Today: a multi-runtime hosting platform.** AzureClaw 2.x ships first-class adapters for OpenClaw (default), OpenAI Agents Python, and Microsoft Agent Framework, plus a documented BYO contract for any custom runtime image. The guardrails — Workload-Identity-fronted inference, Confidential-Container sandboxing, Signal-Protocol mesh, AGT governance, tamper-evident audit, signed OCI egress allowlists — apply uniformly across runtimes via `ClawSandbox.spec.runtime.kind`. **MCP**, **A2A 1.2**, and **AP2** are wired and exercised by CI, with `McpServer` / `A2AAgent` / `ToolPolicy` / `InferencePolicy` / `ClawMemory` / `ClawEval` as first-class CRDs. See [Capabilities](#capabilities--phase-2-shipped) and [Scenario 4 in `docs/use-cases.md`](docs/use-cases.md).

### Who is this for?

- **Platform teams** who need to host LLM agents on AKS with the same operational rigour as the rest of their workloads — namespace isolation, RBAC, NetworkPolicies, audit, signed admission.
- **Security teams** who want a single, opinionated, layered control plane (egress, content safety, governance, mesh trust) instead of stitching point products together.
- **Agent builders** who want to ship without writing the boring-but-load-bearing infrastructure: identity, secret rotation, policy, trust, audit, multi-tenant isolation.

### What problems does it solve?

1. **Credential blast radius** — agents talk to Azure via Workload Identity through the router, not via API keys. Compromise of an agent does not compromise the cloud account.
2. **Tool-call governance** — every shell exec / HTTP fetch / sub-agent spawn passes through a policy decision point with audit. No invisible side effects.
3. **Inter-agent trust** — agents talk over a Signal-Protocol mesh with explicit KNOCK trust handshake, trust scoring, and tamper-evident audit chain. No plaintext fallback.
4. **Operational footprint** — `azureclaw up` provisions AKS + ACR + Foundry + Foundry-side Content Safety + sandbox in one go; `azureclaw operator` gives a live TUI for running fleets.
5. **Multi-runtime out of the box** — see [Capabilities](#capabilities--phase-2-shipped): native adapters for OpenClaw, OpenAI Agents Python, and Microsoft Agent Framework, plus a BYO contract — same governance, same isolation, same audit chain.
6. **Hardware-isolated cloud offload** — run customer agents in Kata + AMD SEV-SNP confidential containers so even a compromised cluster-admin cannot read prompts in flight. See [Blueprint 03 — Managed public offload](docs/blueprints/03-managed-public-offload.md): not just for enterprises, but for **any managed provider** — small MSPs, indie SaaS, hobbyist co-ops — letting end users offload tasks that don't fit a home setup (heavier models, longer runs, parallel fan-out, jobs requiring premium quota) to a sandbox they don't have to operate themselves.

> 📖 **See [`docs/use-cases.md`](docs/use-cases.md)** for the four end-to-end scenarios — AzureClaw-native agents, **any-OpenClaw → AzureClaw cloud offload** (no AzureClaw CLI on the laptop), AzureClaw ↔ AzureClaw mesh, and the roadmap for non-OpenClaw runtimes via MCP / A2A / AP2. For deployment shapes (developer inner-loop, enterprise self-hosted, managed public offload, cross-org federation, sovereign / air-gapped) with topology + trust-boundary + flow diagrams, see [`docs/blueprints/`](docs/blueprints/00-index.md).

---

## Architecture

```
  External A2A peer ──┐                ┌──── User (TUI / Telegram / Web UI)
                      │                │
                      ▼                ▼
   ┌──────────────────────────────────────────────────────────────────────────┐
   │  AKS Cluster (Azure Linux · Cilium)                                      │
   │                                                                          │
   │  ┌─ a2a-gateway ─────────────────┐   (opt-in, ADR-0001 · mTLS to router) │
   │  │  Public ingress edge          │                                       │
   │  │  Verifies inbound A2A 1.2 JWS │                                       │
   │  └──────────────┬────────────────┘                                       │
   │                 │ mTLS (cluster-internal)                                │
   │                 ▼                                                        │
   │  ┌─ Sandbox Pod (per agent · runtime: OpenClaw│OpenAIAgents│MAF│BYO) ─┐  │
   │  │                                                                    │  │
   │  │  init: egress-guard (iptables, UID 1000 → localhost + DNS only)    │  │
   │  │                                                                    │  │
   │  │  ┌──────────────┐   localhost    ┌──────────────────────┐          │  │
   │  │  │  Agent       │──────:8443────►│  Inference Router    │──────────┼──┼──► Azure AI Foundry
   │  │  │  (runtime    │                │  (Rust · in-data-path)│         │  │     (200+ models)
   │  │  │   adapter)   │                │                       │         │  │
   │  │  └──────────────┘                │ • WI/IMDS auth        │         │  │
   │  │   read-only rootfs               │ • Content Safety floor│         │  │
   │  │   drop ALL caps                  │ • Token budgets       │         │  │
   │  │   no Azure credentials           │ • Egress allowlist    │         │  │
   │  │                                  │   (signed OCI ref)    │         │  │
   │  │                                  │ • Platform MCP shim   │         │  │
   │  │                                  │   (Foundry tools)     │         │  │
   │  │                                  │ • AGT governance      │         │  │
   │  │                                  │   PolicyEngine /      │         │  │
   │  │                                  │   TrustManager /      │         │  │
   │  │                                  │   AuditLogger /       │         │  │
   │  │                                  │   RateLimiter /       │         │  │
   │  │                                  │   BehaviorMonitor     │         │  │
   │  │                                  └───────────────────────┘         │  │
   │  │  NetworkPolicy: default-deny egress · seccomp strict               │  │
   │  └────────────────────────────────────────────────────────────────────┘  │
   │                                                                          │
   │  ┌─ AgentMesh (E2E encrypted, Signal Protocol) ─────────────────────────┐│
   │  │  agent-alpha ◄── X3DH + Double Ratchet, KNOCK trust-gated ──► agent-β││
   │  │  agentmesh-relay (WS) · agentmesh-registry (REST + Postgres)         ││
   │  └──────────────────────────────────────────────────────────────────────┘│
   │                                                                          │
   │  Controller (Rust · kube-rs) — reconciles 8 CRDs:                        │
   │    ClawSandbox · ClawPairing · McpServer · ToolPolicy ·                  │
   │    InferencePolicy · A2AAgent · ClawMemory · ClawEval                    │
   │    + leader-election · SSA field managers · jittered backoff · metrics   │
   └──────────────────────────────────────────────────────────────────────────┘
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
| `azureclaw-controller` | Rust | K8s operator — reconciles all 8 CRDs (ClawSandbox, ClawPairing, McpServer, ToolPolicy, InferencePolicy, A2AAgent, ClawMemory, ClawEval); periodic federated-credential reaper GCs orphan credentials against the Azure 20-fedcred-per-MI cap |
| `azureclaw-inference-router` | Rust | Inference proxy — Workload Identity auth, Content Safety floor, AGT governance, signed-OCI egress allowlist, platform MCP shim for Foundry tools |
| `azureclaw-a2a-gateway` | Rust | Public ingress edge for inbound A2A 1.2 federation (axum + rustls; mTLS to router; opt-in via `azureclaw up --enable-a2a-ingress`; see [ADR-0001](docs/adr/0001-a2a-ingress-front-edge.md)) |
| `azureclaw-sandbox` (built from `sandbox-images/openclaw`) | Node.js | Default OpenClaw runtime container (OpenClaw + AGT SDK + Python tools) |
| `agentmesh-relay` | Rust | WebSocket relay for E2E encrypted inter-agent messaging — see *AgentMesh & vendoring* below |
| `agentmesh-registry` | Rust + PostgreSQL | Agent discovery, prekey storage, React admin UI — see *AgentMesh & vendoring* below |

`azureclaw push` builds the 5 images above by default. The shared
`sandbox-base` image is built only when `--include-base` is passed. A separate
`sandbox-images/nemoclaw/` image exists for any-OpenClaw-host clients (laptop,
NemoClaw, etc.) that want to offload to AzureClaw — see
[`docs/internal/any-openclaw-cloud-offload.md`](docs/internal/any-openclaw-cloud-offload.md).

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

## Capabilities — Phase 2 shipped

AzureClaw started as a secure runtime for OpenClaw agents. Phase 2 generalised the substrate so the same governance and isolation guarantees apply to any agent runtime that speaks open protocols (MCP, A2A, AP2). The platform-level work is **shipped, default-on where safe, and exercised by CI** — not scaffolding.

| Capability | State | Surface |
|---|---|---|
| **Multi-runtime hosting** | ✅ shipped | `ClawSandbox.spec.runtime.kind ∈ { OpenClaw, OpenAIAgents, MicrosoftAgentFramework, BYO }` — `OpenClaw` is default; `BYO` requires the documented [runtime contract](docs/runtime-contract.md). OpenAI Agents Python and Microsoft Agent Framework adapters live under `runtimes/` |
| **MCP 2026 server CRD** | ✅ shipped | `McpServer` reconciler emits JWKS Secret + signing keypair; router mounts `/mcp` once Secret exists; per-tool OAuth 2.1 scope checks |
| **A2A 1.2 + AP2** | ✅ shipped | `A2AAgent` reconciler signs and publishes Agent Cards at `/.well-known/agent.json`; `ToolPolicy` carries AP2 `commerce` / `approval` / `rateLimit` precedence rules; inbound federation goes through the dedicated `a2a-gateway` component (opt-in) |
| **Inference policy as a CRD** | ✅ shipped | `InferencePolicy` carries token budgets, content-safety floor, model preference; hot-reloads into the router via informer; admission policy enforces a cluster-level Content Safety floor |
| **Memory binding** | ✅ shipped | `ClawMemory` is a *binding* CR over Foundry Memory Store — never an in-cluster store. Scope, retention, RBAC, delete-on-sandbox-delete are preserved in the compiled binding |
| **Eval-as-CRD** | ✅ shipped | `ClawEval` runs one-shot or scheduled eval suites (promptfoo / inspect-ai / Foundry Evals) over a `sandboxRef` and reports pass/score on status |
| **Signed OCI egress allowlist** | ✅ shipped | `azureclaw egress … --sign` builds a canonical allowlist artifact, pushes it to ACR, cosign-signs it (keyless / OIDC token / KMS) and patches `ClawSandbox.spec.networkPolicy.allowlistRef`. Controller verifies signer identity against a `SignerPolicy` ConfigMap and derives `allowedEndpoints` fail-closed |
| **Operator TUI** | ✅ shipped | `azureclaw operator` renders all 8 CRDs + provider status with per-CRD modular panels |
| **`kubectl claw attest`** | ✅ shipped (read surface) | `azureclaw attest <name>` returns spec hash, SSA field-owner map, most-recent reconcile-span trace, policy version, AGT receipt id (signed-chain *emission* lands in Phase 3) |
| **CNCF K8s AI Conformance v1.35+** | ✅ shipped | Suite wired into CI under `tests/cncf-conformance/`; evidence archived per run. Public certification filing is deferred to post-OSS |
| **Chaos / fault-injection tier** | ✅ shipped | `tests/chaos/` exercises K8s API flakes, Foundry 429 storms, Entra token rotation races, AGT provider timeouts; reconcilers asserted idempotent + eventually consistent |
| **Sigs/agent-sandbox compat** | ✅ shipped | `ClawSandbox.spec.upstreamCompatibility ∈ { Native, Translate, Overlay }`; `azureclaw convert` translates ClawSandbox ↔ upstream `Sandbox` CR; `azureclaw migrate from-kagent` ports kagent CRs |
| **Pluggable governance providers** | ✅ shipped | `PolicyDecisionProvider`, `AuditSink`, `SigningProvider`, `MeshProvider` traits; in-tree implementations are the production path; native AGT-Rust 3.x compiled in |

What is *not* in the box for v1.0 (some of these are tracked as `[GAP-V1]` in source for reviewer visibility):

- Cosign-on-admission for pod images, SLSA-on-CRs, signed reconcile audit chain *emission* (Phase 2 ships only the read surface).
- Confidential controller; router-mediated controller egress.
- Native runtime adapters beyond OpenAI Agents Python + MAF (Semantic Kernel, Anthropic, LangGraph, Google ADK, Pydantic AI, Strands).
- Public AAIF / CNCF Sandbox filing (gated on OSS publication).

---

## AgentMesh & vendoring (transitional)

Inter-agent messaging today runs on a vendored fork of [AgentMesh](https://github.com/amitayks/agentmesh) (relay + registry + SDK). AgentMesh is pre-release; while integrating it we contributed bug fixes and protocol corrections that are tracked in this tree until they land upstream. Each fix is documented in `vendor/<component>/README.md`, and an index lives at [`docs/internal/agt-vendored-patch-audit.md`](docs/internal/agt-vendored-patch-audit.md).

**Direction of travel:** Microsoft's Agent Governance Toolkit (AGT) is shipping a first-party AgentMesh transport. Once it stabilises, AzureClaw's `MeshProvider` seam (defined plugin-side; the router has no in-tree mesh implementation) will allow operators to switch to the AGT mesh per-tenant without breaking existing deployments. Until then, the vendored stack is the supported production path.

---

## Engineering & quality posture

We treat security and code health as product-grade concerns:

- **Six blocking CI gates** — LOC budget, anti-stub (no `TODO`/`unimplemented!` on production paths), no custom crypto outside provider seams, no `Null*` providers in production, mandatory security-audit document per capability-introducing PR, vendored-patch re-audit on every AGT SDK bump.
- **Per-capability security audits** — every PR that introduces a new CRD, router route, admission policy, or sandbox-image change ships a `docs/internal/security-audits/<date>-<slug>.md` with threat-model delta, OWASP mapping, AuthN/Z path, secret custody, audit events, failure mode, and two engineer sign-offs.
- **Behavioral conformance corpus** — `tests/conformance/` covers Signal Protocol (X3DH / KNOCK / negative cases), sandbox isolation, and the protocol scaffolding above with mandatory negative tests (tampered ciphertext, replayed message, expired mandate).
- **Compat suite** — `tests/compat/` regression-tests user-visible flows (today: the operator TUI; growing per phase).
- **Fuzz targets** — cargo-fuzz coverage for handoff state deserialization, chat sanitisation, JWS parsing, base64url decoding, streaming response parsing.

A complete inventory of these controls is in [`docs/architecture.md`](docs/architecture.md) and the per-slice security audits under [`docs/internal/security-audits/`](docs/internal/security-audits/).

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

`azureclaw` ships **23 top-level commands** (`cli/src/commands/`):
`a2a · add · attest · connect · convert · credentials · destroy · dev · egress · eval · handoff · list · logs · mesh · migrate · model · operator · pair · policy · push · status · trace · up`.

| Command | Description |
|---|---|
| **Lifecycle** | |
| `azureclaw up` | Deploy full stack — preflight, AKS + ACR + Foundry + sandbox |
| `azureclaw up --upgrade` | Fast upgrade — reuse cached context, Helm + RBAC + fedcred sync |
| `azureclaw up --enable-a2a-ingress` | Provision the public A2A gateway component (off by default) |
| `azureclaw dev` | Local Docker sandbox with same security controls |
| `azureclaw add <name>` | Add sandbox to existing cluster (`--runtime openclaw\|openai-agents\|microsoft-agent-framework\|byo`) |
| `azureclaw destroy [name]` | Tear down sandbox or entire resource group (`--all`) |
| `azureclaw push` | Build and push images to ACR (`--apply` restarts deployments, `--only <image>` for single image, `--include-base` to also build the shared base) |
| `azureclaw convert` | Translate between `ClawSandbox` and `sigs/agent-sandbox` `Sandbox` shapes |
| `azureclaw migrate to-overlay\|from-overlay\|to-translate\|to-observe\|to-native` | Switch a sandbox's `upstreamCompatibility` mode in place |
| `azureclaw migrate from-kagent <file>` | Port a kagent CR to a `ClawSandbox` |
| **Operations** | |
| `azureclaw operator` | Live TUI dashboard — renders all 8 CRDs + provider status |
| `azureclaw connect <name>` | TUI, shell (`--shell`), or Web UI (`--web`) — surfaces `kubectl` stderr on port-forward failure |
| `azureclaw handoff <name> --to cloud\|local` | Live-migrate agent + sub-agents between local Docker and AKS |
| `azureclaw handoff <name> --status\|--abort` | Show progress / abort an in-flight handoff |
| `azureclaw status <name>` | Health, model, tokens used |
| `azureclaw list` | All sandboxes across Docker and AKS |
| `azureclaw logs <name>` | Stream logs (`-f`, `--service router\|gateway\|openclaw`) |
| `azureclaw attest <name>` | Read-side attestation — spec hash, SSA owner map, reconcile-trace, policy version, AGT receipt id |
| **Configuration** | |
| `azureclaw credentials` | Set Azure OpenAI credentials (interactive) |
| `azureclaw credentials update <name>` | Rotate channel/plugin keys on running sandbox |
| `azureclaw model set\|get\|list` | Switch / inspect model (hot-swap, no restart) |
| `azureclaw policy allow\|deny\|get` | Manage per-sandbox egress policy |
| `azureclaw egress <name>` | Egress management (`--learned`, `--pending`, `--blocked`, `--approve`, `--enforce`) |
| `azureclaw egress sign <name>` | Build a canonical allowlist artifact, push to ACR, cosign-sign (keyless / OIDC token / KMS), patch `allowlistRef`. Add `--emit-manifest` for GitOps |
| **Observability** | |
| `azureclaw trace <name>` | eBPF tracing (`--network`, `--dns`, `--files`, `--exec`) |
| `azureclaw eval <name>` | Run Foundry evaluations against agent (one-shot or via `ClawEval` CR) |
| **Multi-Agent** | |
| `azureclaw mesh auth\|identity\|oauth\|health\|promote` | Mesh identity / OAuth / health / promotion subcommands |
| `azureclaw mesh send <amid>` | Send E2E encrypted message to another agent |
| `azureclaw pair <a> <b>` | Pair two existing sandboxes via `ClawPairing` CR |
| `azureclaw a2a list-exposed\|tail\|schema` | List exposed A2A endpoints, tail gateway access logs, print local A2A schema |

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
| [Use Cases](docs/use-cases.md) | Canonical scenarios: AzureClaw-native, any-OpenClaw → AzureClaw offload, AzureClaw ↔ AzureClaw mesh, multi-runtime |
| [Architecture](docs/architecture.md) | Component design, CRD schema, API routes, four-seam providers, MCP/A2A modules, operator dashboard, auth flow |
| [Architecture Diagrams](docs/architecture-diagrams.md) | Mermaid flow diagrams: pod layout, agent creation, spawn, mesh, egress, inference, A2A ingress |
| [CRD Reference](docs/api/crd-reference.md) | Full schema, validation, conditions and field-manager split for all 8 CRDs |
| [CLI Reference](docs/cli-reference.md) | Every flag, option, and subcommand of the 23 top-level commands |
| [Runtime Contract](docs/runtime-contract.md) | The `org.azureclaw.runtime.contract=v1` BYO contract — what any custom runtime image must satisfy |
| [Blueprints](docs/blueprints/00-index.md) | Five deployment shapes: developer inner-loop, enterprise self-hosted, managed public offload, cross-org federation, sovereign / air-gapped |
| [Security](docs/security.md) | Defense-in-depth model, OWASP coverage, threat mitigations, CI gates, security-audit framework |
| [Threat Model — Routes](docs/internal/threat-model.md) | Per-route auth tier, input validation, blast-radius analysis |
| [AGT Vendored-Patch Audit](docs/internal/agt-vendored-patch-audit.md) | Index of fixes applied to the vendored AgentMesh stack pending AGT mesh shipping |
| [`sigs/agent-sandbox` Compat](docs/internal/sigs-agent-sandbox-compat.md) | Native / Translate / Overlay modes; `azureclaw convert` and `azureclaw migrate` |
| [OWASP MCP Top 10 (2025)](docs/security-mcp-top10.md) | Controls matrix for the MCP 2026 surface |
| [ADR-0001 — A2A ingress front-edge](docs/adr/0001-a2a-ingress-front-edge.md) | Gateway-only, surgical opt-in posture for inbound A2A |
| [Channels & Plugins](docs/channels-plugins.md) | Telegram, Slack, Discord, search plugins, Foundry Bing |
| [Egress Proxy](docs/egress-proxy.md) | Blocklist, allowlist, learn mode, approval flow, signed-OCI allowlist refs |
| [E2E Encryption](docs/internal/e2e-encryption-proof.md) | Signal Protocol inter-agent encryption proof |
| [Multi-Tenant](docs/multi-tenant.md) | Namespace isolation, credential and channel separation |
| [Security Validation](docs/security-validation.md) | Live cluster evidence for every security layer |
| [Permissions](docs/permissions.md) | Required Azure RBAC for `azureclaw up` |
| [Demo](docs/internal/DEMO.md) | "Operation Claw Shield" — multi-tenant attack simulation |

---

## Project Structure

```
azureclaw/
├── ci/                   # blocking CI gates + LOC budget
├── cli/                  # operator CLI (TypeScript · @azureclaw/cli — 23 commands)
├── runtimes/openclaw/    # AzureClaw runtime adapter for OpenClaw (in-sandbox plugin + skills)
├── controller/           # Rust K8s operator (kube-rs) — reconcilers for all 8 CRDs
│   └── src/{crd,reconciler,mesh_peer,status,providers,fedcred,fedcred_reaper,
│            mcp_server_reconciler,tool_policy_reconciler,inference_policy_reconciler,
│            a2a_agent_reconciler,claw_memory_reconciler,claw_eval_reconciler,
│            policy_fetcher,leader_election,backoff,metrics,...}.rs
├── inference-router/     # Rust inference proxy (axum) — in the data path of every external call
│   └── src/{a2a,mcp,providers,routes,handoff,governance,trust,audit,
│            rate_limiter,behavior_monitor,safety,budget,...}/
│   └── fuzz/             # cargo-fuzz targets
├── a2a-gateway/          # Rust public ingress edge — JWS verifier, mTLS to router (opt-in)
├── azureclaw-a2a-core/   # Pure A2A JWS verifier + types, shared by router and gateway
├── sandbox-images/       # OpenClaw + nemoclaw container images
├── deploy/               # Bicep IaC, Helm chart (CRDs · admission · network policies · gateway)
├── docs/                 # Architecture, security, threat model, ADRs, security-audits/, blueprints/
├── examples/             # Sample agents (basic, confidential, telegram, demo)
├── tests/                # compat/, conformance/, e2e/, chaos/, cncf-conformance/
└── vendor/               # AgentMesh SDK (21 patches), registry (4), relay (transitional fixes)
```

> **About `vendor/`:** AzureClaw is *not* a fork of OpenClaw. The `vendor/` directory only carries our patched copies of the pre-release AgentMesh stack (relay, registry, SDK) — see *AgentMesh & vendoring* above. Each patch is documented in `vendor/<component>/README.md`, indexed in [`docs/internal/agt-vendored-patch-audit.md`](docs/internal/agt-vendored-patch-audit.md), and re-validated on every AGT SDK version bump.

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

## Third-Party Notices

AzureClaw bundles four vendored upstream packages under `vendor/`. Full license texts, copyright notices, and a summary of local patches applied to each are in [`THIRD_PARTY_NOTICES.txt`](THIRD_PARTY_NOTICES.txt).

## License

[MIT](LICENSE) · [Code of Conduct](CODE_OF_CONDUCT.md)

---

## Trademarks

This project may contain trademarks or logos for projects, products, or services. Authorized use of Microsoft
trademarks or logos is subject to and must follow
[Microsoft's Trademark & Brand Guidelines](https://www.microsoft.com/en-us/legal/intellectualproperty/trademarks/usage/general).
Use of Microsoft trademarks or logos in modified versions of this project must not cause confusion or imply Microsoft sponsorship.
Any use of third-party trademarks or logos are subject to those third-party's policies.

---

## Data Collection

The software may collect information about you and your use of the software and send it to Microsoft.
Microsoft may use this information to provide services and improve our products and services.
You may turn off the telemetry as described in the repository.
There are also some features in the software that may enable you and Microsoft to collect data from users of your applications.
If you use these features, you must comply with applicable law, including providing appropriate notices to users of your applications together with a copy of Microsoft's privacy statement.
Our privacy statement is located at <https://go.microsoft.com/fwlink/?LinkID=824704>.
You can learn more about data collection and use in the help documentation and our privacy statement.
Your use of the software operates as your consent to these practices.

### AzureClaw telemetry details

#### What is collected

AzureClaw is a **self-hosted Kubernetes operator**. The inference router (`azureclaw-inference-router`) emits the following telemetry within your own cluster:

**Prometheus metrics** (scraped from the pod's `/metrics` endpoint by your own Prometheus instance):

| Metric | Labels | Description |
|---|---|---|
| `azureclaw_inference_requests_total` | `sandbox`, `model`, `status` | Counter of inference requests |
| `azureclaw_inference_latency_seconds` | `sandbox`, `model` | Latency histogram (buckets: 0.1–30 s) |
| `azureclaw_tokens_total` | `sandbox`, `model`, `direction` (`input`/`output`) | Token counts from the model's `usage` field |
| `azureclaw_upstream_retries_total` | `sandbox`, `reason` (`transport`/`status`) | Upstream retry count |
| `azureclaw_agt_policy_evaluations_total` | `decision` | AGT governance policy decisions |
| `azureclaw_agt_eval_latency_seconds` | — | AGT policy evaluation latency |
| `azureclaw_agt_known_agents` | — | Agents in the trust store |
| `azureclaw_agt_audit_entries_total` | — | Cumulative AGT audit log entries |
| `azureclaw_agt_content_flags_total` | `category` | Content Safety flags |
| `azureclaw_agt_behavior_alerts_total` | — | Behavior anomaly alerts |
| `azureclaw_agt_policy_rules` | — | Loaded policy rules |
| `azureclaw_agt_redactions_total` | `kind` | Credential redactions from output |
| `azureclaw_agt_response_threats_total` | `type` | Response threat detections |
| `azureclaw_agt_tool_rate_limits_total` | `tool` | Per-tool rate-limit denials |
| `azureclaw_agt_message_signatures_total` | `action` | Ed25519 sign/verify operations |
| `azureclaw_handoff_pending_events_total` | `action` | Handoff lifecycle events |
| `azureclaw_handoff_phase_transitions_total` | `from`, `to`, `result` | Handoff session phase transitions |

**Structured JSON logs** (written to pod stdout via `tracing`). Each log line contains: timestamp, severity, `trace_id` (16-hex correlation id), `sandbox`, `model`, HTTP status, latency, Azure correlation ids (`x-ms-request-id`, `apim-request-id`), and AGT governance verdicts.

**OTel GenAI Semantic Convention span attributes** are defined in `inference-router/src/telemetry/gen_ai.rs` following the [OpenTelemetry GenAI SemConv specification](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/gen-ai.md). Call-site wiring for OTLP span emission will be added in a future release; as of this writing, no OTLP spans are exported.

#### What is NOT collected

**No prompt text or completion text is ever captured.** The router reads only the `usage` object (`prompt_tokens`, `completion_tokens`) from upstream responses to record token counts. Message content (`messages[].content`, `choices[].message.content`) is forwarded transparently to the agent and is never read, logged, or stored by the router.

Source reference: `inference-router/src/proxy.rs` — `record_metrics()` and the SSE streaming path both parse only `body_json["usage"]["prompt_tokens"]` and `body_json["usage"]["completion_tokens"]`.

#### Where telemetry goes — NOT sent to Microsoft by default

AzureClaw runs entirely in **your own AKS cluster**. Microsoft has no cloud-side ingestion of these metrics or logs.

- **Prometheus metrics** are scraped by whichever Prometheus instance you configure (or Azure Monitor managed Prometheus if you enable `monitoring.containerInsights: true` in `deploy/helm/azureclaw/values.yaml`). If no scraper is configured, the `/metrics` endpoint is never read.
- **Structured logs** go to your cluster's log pipeline (e.g., Azure Monitor Container Insights, your SIEM). Microsoft only receives these logs if you configure Azure Monitor Container Insights.
- **OTLP spans** are not currently emitted. When OTLP emission is added, it will respect `OTEL_EXPORTER_OTLP_ENDPOINT`; if that variable is unset, no spans leave the pod.

#### How to disable telemetry / opt out

**Option 1 — Disable Prometheus scraping (Helm):**

```yaml
# deploy/helm/azureclaw/values.yaml
monitoring:
  enabled: false
  prometheus:
    enabled: false
  containerInsights: false
```

**Option 2 — Do not configure a Prometheus scraper:**

Simply do not configure a Prometheus `ServiceMonitor` or pod-annotation scraping for the `azureclaw-inference-router` pods. The `/metrics` endpoint exists but is never read.

**Option 3 — Disable Azure Monitor Container Insights:**

If you are not using Azure Monitor Container Insights to collect container logs, structured log output stays within your cluster's internal log pipeline and is not forwarded to Microsoft.

**Future OTLP opt-out (when span emission is added):**

Do not set `OTEL_EXPORTER_OTLP_ENDPOINT` on the inference-router pods. When that environment variable is absent, the OpenTelemetry SDK will be configured with a no-op exporter and no spans will leave the pod.

#### Source-of-truth files

| File | What it documents |
|---|---|
| `inference-router/src/metrics.rs` | All Prometheus metric definitions, labels, and descriptions |
| `inference-router/src/proxy.rs` | Token count extraction — confirms only `usage.*_tokens` fields are read |
| `inference-router/src/telemetry/gen_ai.rs` | OTel GenAI SemConv constants (not yet emitted) |
| `inference-router/src/main.rs` | JSON structured log initialisation (`tracing_subscriber`) |
| `deploy/helm/azureclaw/values.yaml` | `monitoring.*` Helm knobs for Prometheus and Container Insights |
