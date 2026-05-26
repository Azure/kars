<div align="center">

# 🔱 AzureClaw

**A secure runtime for AI agents on Azure Kubernetes Service.**

[![License: MIT](https://img.shields.io/badge/License-MIT-0078D4.svg)](LICENSE)
[![CI](https://github.com/Azure/azureclaw/actions/workflows/ci.yml/badge.svg)](https://github.com/Azure/azureclaw/actions/workflows/ci.yml)
[![Azure](https://img.shields.io/badge/Azure-AKS%20%7C%20Foundry%20%7C%20Kata-0078D4)](https://azure.microsoft.com)

Hardened sandbox per agent. Zero credentials in the agent. Every external call goes through a Rust router that enforces identity, content safety, governance, and audit. End-to-end encrypted inter-agent messaging. One CLI for the whole loop — laptop to AKS.

[**Try it on your laptop →**](#try-it-in-five-minutes) &nbsp;·&nbsp; [**Run it on AKS →**](docs/getting-started.md#step-2--deploy-to-aks) &nbsp;·&nbsp; [**Architecture →**](docs/architecture.md) &nbsp;·&nbsp; [**Blueprints →**](docs/blueprints/00-index.md)

</div>

---

## What problem does this solve?

Giving an AI agent real tools today means giving it real credentials and a real network. That blast radius is unacceptable for any production workload — one prompt-injected agent and your Azure subscription, your GitHub org, your customer data are reachable.

AzureClaw is the runtime that lets you ship agents with the same operational discipline you ship the rest of your services: namespace isolation, NetworkPolicies, signed admission, audit, RBAC. In production (AKS) the agent process runs under a different UID than the router and never sees an Azure key — the router holds the credential and brokers every call. (In `azureclaw dev` the agent and router are co-located in one container; see [Two modes, one mental model](#two-modes-one-mental-model) for the security boundary in each.) Every model call, every web fetch, every peer message passes through a control plane you can reason about, version, and roll back.

It is built for three audiences:

- **Platform teams** — host LLM agents on AKS without inventing a new operational model.
- **Security teams** — one opinionated, layered control plane for identity, egress, content safety, governance, mesh trust.
- **Agent builders** — build the agent, not the boring-but-load-bearing infrastructure underneath it.

### What makes it different

- **The router is the security boundary, not the agent process.** Every external call is mediated by a typed Rust proxy under a different UID, with credentials the agent never sees and a CRD-driven allowlist on every request. iptables + NetworkPolicy are safety nets, not the policy layer.
- **Governance is a first-class CRD layer.** Approval gates, rate limits, tool allowlists, content-safety floors, token budgets, and trust topology are all declarative Kubernetes resources you commit to a repo and reconcile with Argo / Flux — no out-of-band config store.
- **End-to-end encrypted agent-to-agent mesh.** Signal Protocol (X3DH + Double Ratchet) with KNOCK trust gating. The relay sees only ciphertext.
- **Provider- and runtime-agnostic.** Seven first-class runtimes (OpenClaw, OpenAI Agents SDK, Microsoft Agent Framework, LangGraph in Python and TypeScript, Anthropic, Pydantic-AI) plus BYO; GitHub Copilot, Azure AI Foundry, Azure OpenAI, and GitHub Models as backends; native Anthropic-shape passthrough for Claude.
- **Same code path in dev and prod.** `azureclaw dev` runs the same router, the same audit chain, the same governance profile as `azureclaw up`. The dev-to-prod jump is one CLI command, not a re-architecture.

---

## How it works (in one diagram)

```
                    ┌──────────────── Sandbox pod ────────────────┐
   User / TUI ────► │  agent container (UID 1000, no network)     │
                    │              │                              │
                    │              │  localhost only              │
                    │              ▼                              │
                    │  ┌────────────────────────────────────┐     │
                    │  │  Inference Router (Rust)           │     │
                    │  │                                    │     │
                    │  │  Identity (Workload Identity)      │     │
                    │  │  Content Safety (Foundry inline)   │     │
                    │  │  Token budget · rate limit         │     │
                    │  │  Tool policy · governance (AGT)    │     │
                    │  │  Audit (tamper-evident chain)      │     │
                    │  └─────────────────┬──────────────────┘     │
                    │                    │                        │
                    │            init: egress-guard               │
                    │      (iptables safety net: agent UID         │
                    │       can only reach the router locally)     │
                    └────────────────────┼────────────────────────┘
                                         │
                            Workload Identity (no keys)
                                         │
                  ┌──────────────────────┼─────────────────────────┐
                  ▼                      ▼                         ▼
           Inference backend       AgentMesh relay            A2A peers
           ┌─────────────────┐     (Signal-Protocol           (signed
           │ GitHub Copilot  │      E2E messages)              AgentCards)
           │ (Claude · GPT · │
           │  Gemini · …)    │  ◄── recommended for dev (one OAuth login)
           ├─────────────────┤
           │ Azure AI Foundry│
           │ / Azure OpenAI  │  ◄── full feature set (Memory, Agents, CS)
           ├─────────────────┤
           │ GitHub Models   │  (free tier · PAT · small context)
           ├─────────────────┤
           │ + more soon     │  ◄── feature-request via GitHub issues
           └─────────────────┘
```

**The agent has no network of its own.** Every byte that leaves the pod leaves through the router — that's the policy point where egress, governance, content-safety, token budgets, and audit are enforced. The K8s `NetworkPolicy` and the `egress-guard` iptables init container are **safety nets** that contain blast radius if the router is bypassed or compromised — they are not the policy layer. Compromise of the agent does not compromise the cloud account, the model, the audit log, or the peer mesh.

**Pluggable inference backend.** Three providers are wired in today:

- **GitHub Copilot** — recommended for the [inner loop](docs/architecture.md#dev-mode-azureclaw-dev). One device-code OAuth login (no Azure account, no PAT to manage). Picks from the full Copilot model catalogue, including current Claude, GPT, Gemini, and reasoning-class models. Native Anthropic-shape passthrough for Claude (no shape translation, full tool-calling fidelity). Largest context windows in the lineup. See [Sandbox pod — dev mode](docs/architecture-diagrams.md#1-sandbox-pod--dev-mode) for the runtime shape.
- **Azure AI Foundry / Azure OpenAI** — the [production-grade](docs/architecture.md#prod-mode-azureclaw-up) default. Unlocks the full feature set: Memory Store, Agents, Evaluations, Indexes, Datasets, inline Content Safety, and the rest of the Foundry data-plane the router proxies. Use this when you need anything beyond plain chat completions, or when running on AKS. See [Sandbox pod — prod mode](docs/architecture-diagrams.md#2-sandbox-pod--prod-mode) and [The data path of one model call](docs/architecture-diagrams.md#3-the-data-path-of-one-model-call).
- **GitHub Models** — free, PAT-only, no subscription. Convenient for trivial demos; smaller context windows and tight rate limits make it a poor fit for real agents. Foundry-only routes return `501`; inline Content Safety is not enforced (see [security.md](docs/security.md#what-we-do-not-defend-against) and the [data path](docs/architecture.md#the-data-path-of-one-external-call) for what each provider routes through).

Adding more providers (Bedrock, direct Anthropic, third-party OpenAI-compatible gateways) is mostly an endpoint+auth recipe in `inference-router/src/proxy.rs::build_upstream_url` plus a CLI prompt branch — please open a GitHub issue / feature request.

For the full picture (control plane, data plane, mesh, A2A, MCP), see **[`docs/architecture.md`](docs/architecture.md)** and **[`docs/architecture-diagrams.md`](docs/architecture-diagrams.md)**.

---

## Two modes, one mental model

You write the same `ClawSandbox` YAML for both. The difference is where it runs and what isolates it.

| Aspect | **Dev mode** (`azureclaw dev`) | **Prod mode** (`azureclaw up` → AKS) |
|---|---|---|
| Where | One Docker container on your laptop | An AKS cluster in your subscription |
| Pod shape | **Single container** — agent + router co-located in one image | **Multi-container pod** — agent (UID 1000) + router (UID 1001) + init `egress-guard` |
| Network isolation | Docker network, no egress guard | Router is the policy point; `NetworkPolicy` + `egress-guard` initContainer act as safety nets containing blast radius |
| Identity | Provider credential — Copilot OAuth token, Foundry resource key, or GitHub PAT (mounted from a local secret) | Workload Identity (federated, no keys on disk) |
| Optional VM isolation | n/a | Kata + AMD SEV-SNP (Confidential Containers) |
| Use it for | Inner-loop dev, plugin authoring, demos | Real workloads, multi-tenant, production |

Same CRDs. Same router code path. Same audit format. Same governance profiles. The graduation from `dev` to `up` is a one-line CLI change, not a port to a new system.

---

## Try it in five minutes

**Fastest path (recommended): GitHub Copilot.** If you have an active Copilot seat (Individual / Business / Enterprise), the only thing you need beyond Docker is one device-code login. No Azure account, no PAT, no key files.

```bash
# Build the CLI (Node 22+, Rust 1.88+, Docker)
git clone https://github.com/Azure/azureclaw.git && cd azureclaw
cd cli && npm ci && npm run build && npm link

# Launch a sandbox locally — Docker only, no Azure, no AKS
azureclaw dev
```

On first run `azureclaw dev` shows a 3-way provider picker:

```
$ azureclaw dev

  ╭────────────────────────────────────────────────╮
  │  AzureClaw · Local Sandbox                     │
  │  Secure AI Agent Runtime on Azure              │
  ╰────────────────────────────────────────────────╯

  👋 First time? Pick an inference provider — no Azure account needed for the GitHub options.
  Copilot is the default (largest context). You can change later with `azureclaw credentials`.

? Which inference provider do you want to use?
❯ GitHub Copilot                    (recommended; needs an active Copilot seat — large context, Claude/GPT/Gemini)
  Azure AI Foundry / Azure OpenAI   (full feature set: Memory Store, agents, Content Safety, etc.)
  GitHub Models                     (free; just need a GitHub PAT — small context, Foundry features disabled)
```

1. **GitHub Copilot** *(default)* — one device-code login at `https://github.com/login/device`, then pick from the Copilot model catalogue. No Azure, no PAT, no key files. **This is the fastest path to a working agent on a real frontier model.**
2. **Azure AI Foundry / Azure OpenAI** — paste an endpoint, deployment, and resource-level API key. Required for Memory Store, agents, evaluations, and inline Content Safety.
3. **GitHub Models** — paste a GitHub PAT with `models:read`. Free; small context windows.

Your choice is saved to `~/.azureclaw/config.json` and reused on subsequent runs. Switch later with `azureclaw credentials`.

The first run also prompts for an **agent name** (default `dev-agent` — hit Enter to accept). Use that name in subsequent commands:

```bash
# Talk to the agent (TUI auto-opens; or use the CLI directly)
azureclaw connect dev-agent
```

The TUI drops you into a chat window. Type *"list the files in my workspace"* or *"write a Python script that prints the current Azure subscription"* — every tool call the agent makes is governed by the same router code path that runs in production.

> **Don't have an Azure AI Foundry deployment yet?** If you picked Copilot or Models above, you don't need one. If you want the full Foundry feature set, two `az` commands get you both — see **[Getting started — prerequisites](docs/getting-started.md#dont-have-an-azure-ai-foundry-deployment-yet)**.

When you are ready for the real thing:

```bash
azureclaw up --name prod-agent --region swedencentral
```

`azureclaw up` provisions the AKS cluster, ACR, Foundry resource, Foundry-side Content Safety, controller, A2A gateway, Microsoft AGT AgentMesh relay+registry, and your first sandbox — Workload Identity wired end-to-end. See **[`docs/getting-started.md`](docs/getting-started.md)** for the full walkthrough including how to bring your own AKS / Foundry / ACR.

---

## What is built in

### Nine CRDs

`ClawSandbox` is the unit of work — one CRD per agent. Everything else binds policy, identity, or peer relationships to it.

| CRD | Purpose |
|---|---|
| **`ClawSandbox`** | The agent itself: runtime kind, model, tools, mesh membership, governance profile. |
| **`A2AAgent`** | Public-ingress A2A 1.0.0 endpoint for peer-to-peer agent communication. |
| **`McpServer`** | An external MCP server the agent is allowed to call, with OAuth + allow-listed tools. |
| **`ToolPolicy`** | Per-tool gate (approval / rate-limit / commerce caps / AGT profile). |
| **`InferencePolicy`** | Per-tenant model routing, content-safety floor, and token budgets. |
| **`ClawMemory`** | Foundry Memory Store binding with project-MI auth (operator-provisioned today). |
| **`ClawEval`** | Reproducible evaluation runs against a sandbox spec. |
| **`TrustGraph`** | Cross-namespace / cross-cluster trust topology for the AgentMesh layer. *(`v1alpha1` — reconciler-only; router-side **mesh-admission gating** against the projected graph is on the [roadmap](docs/roadmap.md). KNOCK accept/deny stays agent-side — the router cannot decrypt the Signal session.)* |
| **`EgressApproval`** | Ephemeral, TTL-bounded extra egress hosts overlaid on the baseline allowlist. |

Plus the controller-internal `ClawPairing` record (10th kind) used to bind sandboxes to AgentMesh registry IDs.

Full schema in **[`docs/api/crd-reference.md`](docs/api/crd-reference.md)**.

### Seven first-class agent runtimes (plus BYO)

You pick the runtime via `ClawSandbox.spec.runtime.kind`. The router, governance, isolation, and audit chain are identical across all of them.

| Runtime | Language | Image dir | Status |
|---|---|---|---|
| **OpenClaw** (default) | Python | `sandbox-images/openclaw/` | ✅ |
| **OpenAI Agents SDK** | Python | `sandbox-images/openai-agents/` | ✅ |
| **Microsoft Agent Framework** | Python | `sandbox-images/maf-python/` | ✅ (`.NET` deferred) |
| **LangGraph** | Python | `sandbox-images/langgraph/` | ✅ |
| **LangGraph.js** | TypeScript | `sandbox-images/langgraph-ts/` | ✅ |
| **Anthropic Claude Agent SDK** | Python | `sandbox-images/anthropic/` | ✅ |
| **Pydantic-AI** | Python | `sandbox-images/pydantic-ai/` | ✅ |
| **BYO** | any | your image, our contract | ✅ |

The BYO contract is documented in **[`docs/runtimes.md`](docs/runtimes.md)**. Semantic Kernel and MAF .NET are wired in the CRD enum but the adapter images are deferred — the controller emits a clear `ShapeInvalid` condition rather than silently mis-imaging the pod.

### One mesh, one gateway, one CLI

- **AgentMesh** — Signal Protocol (X3DH + Double Ratchet) inter-agent messaging with KNOCK trust handshake and per-message forward secrecy. No plaintext fallback. **The Signal session lives in the agent process**, not the router: the OpenClaw plugin layer (and every other supported runtime) installs `@microsoft/agent-governance-sdk` from npm at sandbox-image build time and owns X3DH / Double Ratchet / KNOCK end to end. The inference router links the [`agentmesh`](https://crates.io/crates/agentmesh) crate from crates.io only for shared governance primitives (`AuditLogger`, `PolicyEngine`, `TrustManager`, MCP rate-limit / redactor) — never for mesh crypto — and acts as a transparent WebSocket bridge to the relay for the encrypted bytes. There is no in-tree fork of either SDK.
- **A2A gateway** — public-ingress for peer-to-peer agent traffic with tenant routing, audit, and rate limiting. AgentCard signature verification (`azureclaw_a2a_core::verify_inbound_card`) ships as a library and is unit-tested; today the gateway authorises inbound traffic via the `X-A2A-Agent-Subject` header set by the upstream mTLS layer. Wiring the verifier as an axum layer inside the gateway binary is tracked in the [roadmap](docs/roadmap.md).
- **CLI (`azureclaw …`)** — 30+ commands covering the whole lifecycle: `dev`, `up`, `add`, `connect`, `handoff`, `mesh`, `policy`, `egress`, `eval`, `attest`, `audit`, `inspect`, `migrate`, `operator` (live TUI), `destroy`, and more. Full reference in **[`docs/cli-reference.md`](docs/cli-reference.md)**.

---

## What it is *not*

- **Not a fork of OpenClaw.** AzureClaw extends [OpenClaw](https://openclaw.ai) through its native plugin API and `tools.deny` config. No OpenClaw source is modified, patched, or vendored. Any upstream OpenClaw release is drop-in compatible. See **[`docs/upstream-alignment.md`](docs/upstream-alignment.md)**.
- **Not a managed service.** It is a runtime you operate yourself, in your subscription, in your AKS cluster.
- **Not a model provider.** Models come from Azure AI Foundry (or any compatible provider through the BYO contract). AzureClaw governs the data path; it does not host the model.

---

## Documentation

| If you want to… | Read |
|---|---|
| Understand the design in 15 minutes | [`docs/architecture.md`](docs/architecture.md) |
| See the diagrams (dev, prod, mesh, A2A) | [`docs/architecture-diagrams.md`](docs/architecture-diagrams.md) |
| Pick a deployment shape | [`docs/blueprints/00-index.md`](docs/blueprints/00-index.md) |
| Read the CRD schema | [`docs/api/crd-reference.md`](docs/api/crd-reference.md) |
| Understand security guarantees | [`docs/security.md`](docs/security.md) |
| Build your own runtime | [`docs/runtimes.md`](docs/runtimes.md) |
| Look up a CLI command | [`docs/cli-reference.md`](docs/cli-reference.md) |
| Operate a fleet | [`docs/operations/`](docs/operations/) |

The full site index is in **[`docs/README.md`](docs/README.md)**.

---

## Project status

`v0.1.0`. The core data path (router, controller, A2A gateway, mesh) is feature-complete and exercised by CI (Kind E2E + manual matrix). See **[`CHANGELOG.md`](CHANGELOG.md)** for the change log and **[`docs/roadmap.md`](docs/roadmap.md)** for what's next.

## Known limitations

We would rather you find these in this list than in production. None of them block the core promise (one router, one audit chain, one CRD shape across runtimes), but they shape how you should run the rc:

- **Mesh trust tiers default to anonymous.** Sub-agents register with the AgentMesh registry as the *anonymous* tier unless the tenant administrator provisions an Entra app registration with `api://agentmesh` as an identifier URI. The router fails open: failed token-exchange logs `registering as anonymous tier` and the agent continues to function — KNOCK gating still happens, just against trust-score `0`. Resolution is one CLI command (`azureclaw mesh setup-trust`, idempotent, needs tenant admin); see **[`docs/security.md#trust-tiers-and-the-apiagentmesh-prerequisite`](docs/security.md#trust-tiers-and-the-apiagentmesh-prerequisite)** for the details.
- **Multi-runtime images are not yet published to a public registry.** OpenClaw runs out of the box; the other six wired runtimes (OpenAI Agents SDK, Microsoft Agent Framework Python, LangGraph, LangGraph.js, Anthropic, Pydantic-AI) currently require `azureclaw push --build` against your own ACR before `azureclaw add --runtime <kind>` will succeed. The build pipeline is in tree (`sandbox-images/<kind>/Dockerfile`); the public distribution is what's pending.
- **Semantic Kernel and MAF .NET runtimes are CRD-wired but adapter-incomplete.** The CRD enum accepts the values, the controller emits a `ShapeInvalid` condition, the agent does not start. Treat them as future work, not silent breakage.
- **Attestation is router-and-audit only.** We sign and hash-chain audit entries; we do not yet emit cosign-signed runtime receipts (`attest sign`/`attest verify` are scaffolded — see `docs/roadmap.md`).
- **No managed-service equivalent.** This is a runtime you operate. There is no hosted control plane.

If a limitation surprised you in a way this list didn't warn about, that's a bug — please file it.

## Contributing & support

- Contributing guide: **[`CONTRIBUTING.md`](CONTRIBUTING.md)**
- Security policy: **[`SECURITY.md`](SECURITY.md)**
- Support: **[`SUPPORT.md`](SUPPORT.md)**
- Code of Conduct: **[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md)**

## License

MIT. See **[`LICENSE`](LICENSE)** and **[`THIRD_PARTY_NOTICES.txt`](THIRD_PARTY_NOTICES.txt)**.

## Data collection

AzureClaw does not collect telemetry, usage data, or crash reports. Nothing
in this repository — the CLI, controller, inference router, or sandbox
images — sends data to Microsoft or any third party.

Logs and traces emitted by the components stay inside your cluster. They are
visible only to whatever log/metrics pipeline you have wired up (Container
Insights, Loki, your own OTLP collector, etc.). No exporter endpoint is
configured by default.

When AzureClaw forwards a model call to Azure AI Foundry on your behalf,
that call is governed by your Azure agreement with Microsoft — not by this
project.

---

> *AzureClaw, the AzureClaw logo, and the trident mark are project marks. See **[`TRADEMARKS.md`](TRADEMARKS.md)** for usage guidance.*
