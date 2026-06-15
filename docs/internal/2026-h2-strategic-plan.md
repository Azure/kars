# Where kars is heading — 2026 H2 strategic plan

**Date:** 2026-06-15
**Status:** internal canonical plan. Replaces 7 separate strategy docs as the first-read entry point; those docs remain as deep-dive references.
**Owner:** Pal Lakatos-Toth (@pallakatos)
**Author:** drafted by Copilot, reviewed by @pallakatos.

> **Pre-read context.** This plan synthesises seven strategy documents written between 2026-06-11 and 2026-06-15:
> [`competitive-positioning-2026-06.md`](competitive-positioning-2026-06.md),
> [`agentgateway-parity-plan.md`](agentgateway-parity-plan.md),
> [`agentgateway-vs-kars-router-analysis.md`](agentgateway-vs-kars-router-analysis.md),
> [`dev-experience-design-note.md`](dev-experience-design-note.md),
> [`sota-agentic-ai-capability-map.md`](sota-agentic-ai-capability-map.md),
> [`agt-boundary.md`](agt-boundary.md),
> [`blog/01-kars-in-10-minutes.md`](blog/01-kars-in-10-minutes.md).
> Read them when you need depth; this doc is what you read first.

---

## 1. What kars is

Kars (**Agent Reference Stack for Kubernetes**) is a hardened, opinionated runtime for AI agents on Kubernetes. Each agent runs in its own namespace. Each agent's network egress is confined by an iptables-based egress-guard and redirected through a per-pod policy enforcer (the *inference router*) the agent cannot bypass and from which the agent cannot read upstream credentials. Eleven CRDs compose into a complete governance picture — model budget, tool allow-list, memory binding, mesh trust topology, egress allowlist, eval runs. Inter-agent messaging is end-to-end encrypted using Signal Protocol via Microsoft AGT's AgentMesh. Eight agent frameworks are supported via runtime adapters that all sit behind the same trust boundary.

Kars ships as a Helm chart plus a small CLI. Source is at [github.com/Azure/kars](https://github.com/Azure/kars). It runs on stock Kubernetes; install is `helm install`.

The product question we answer: **"How do I run governed AI agents on Kubernetes for multiple teams against shared model deployments, with auditable per-agent isolation and end-to-end encrypted inter-agent messaging, regardless of which agent framework the team picked?"**

If your situation is one agent, one user, one team — kars is overkill. If you're running ≥5 agents from ≥2 teams against the same model fleet, kars is built for you.

---

## 2. The seven irreducible advantages

These are properties kars has *today* that no other agentic-AI runtime in this ecosystem (Orka, agentgateway, agent-sandbox SIG) has. They are the moat. Every plan in §6 reinforces these; we do not dilute them.

1. **Per-pod egress trust boundary with credentials outside the agent process.** iptables egress-guard confines UID 1000; only path out is the router sidecar (UID 1001) which holds upstream credentials. Even a fully prompt-injected agent has no API key in its env, file system, or process memory to exfiltrate.
2. **End-to-end encrypted inter-agent messaging via AgentMesh** (Signal Protocol: X3DH + Double Ratchet + KNOCK gate + trust-score progression). The broker sees DIDs and ciphertext, nothing else. Forward secrecy is per-message; post-compromise security restores secrecy after the next ratchet.
3. **Cross-runtime mesh interoperability.** Hermes (Python) ↔ OpenClaw (TypeScript) verified end-to-end on AKS using the same DID format, X3DH wire format, Double Ratchet headers, and KNOCK semantics. No other Kubernetes agent runtime combines per-agent sandbox governance with cross-runtime Signal-Protocol messaging.
4. **Multi-runtime adapter framework** for eight frameworks (OpenClaw, Hermes, Anthropic SDK, Microsoft Agent Framework, LangGraph Python + TypeScript, Pydantic AI, OpenAI Agents) behind one trust boundary, with a documented six-rule contract any framework can implement.
5. **Cosign-attested, compiled, deterministic policy bundles.** Per-sandbox compiled policy ConfigMaps + cosign signing + hot-reload + byte-deterministic compilation. Unique among the surveyed platforms.
6. **Confidential-VM sandboxes as a one-flag flip** (`spec.sandbox.isolation: confidential` → AMD SEV-SNP / Intel TDX). The trust boundary terminates at the pod, not the node — composable with K8s SIG `Sandbox` + Kata Containers / gVisor for layered isolation.
7. **Microsoft Entra Agent ID first-class integration** via `KarsAuthConfig` and the per-pod auth sidecar. Failed closed; no WI fallback when Agent ID mode is on, so downstream attribution is clean.

These properties are the answer to "why kars rather than a managed offering / a centralized gateway / a framework-bundled runtime?" Everything else in this plan exists to keep these properties safe while closing the gaps that block serious evaluators.

---

## 3. Where we deliberately do not compete

The bad outcome is kars trying to be everything and ending up worse than the specialists. Three categories are explicitly out of scope:

1. **Centralized model gateway.** [agentgateway](https://agentgateway.dev) (Solo.io / LF) has 9 enterprise sponsors, a year head-start, 10+ LLM providers, 6 guardrail integrations, virtual keys with per-key budgets, MCP federation, CEL-based RBAC, production deployments at T-Mobile and UBS. Trying to out-feature them in the gateway category is a losing battle. **We compose with agentgateway, we don't replace it.** If a customer wants an external OpenAI-compatible front-door endpoint they can point Continue/Cursor/Claude Code at, that's agentgateway. We will not ship `/openai/v1/chat/completions` ingress as a kars feature — see [agentgateway-parity-plan.md](agentgateway-parity-plan.md) for the explicit rejection.
2. **Sandbox workload primitive.** [kubernetes-sigs/agent-sandbox](https://github.com/kubernetes-sigs/agent-sandbox) (Google + Anthropic + community) owns the K8s `Sandbox` CRD — a `podTemplate` + `volumeClaimTemplates` + `lifecycle` abstraction. Stateful-singleton-pod-with-stable-identity is their lane. We compose on top via `spec.upstreamCompatibility.sigsAgentSandbox: overlay` and will contribute a kars-hardened `SandboxTemplate` upstream rather than build a competing primitive.
3. **In-house workflow / orchestration engine, no-code agent builder, marketplace.** Temporal / Argo Workflows / LangGraph-the-platform already solve graph-shaped agent workflows; we compose. Drag-and-drop UIs trade governance for accessibility; the CRD-authority model is essential to our story. Recipe catalogs are per-team; a community marketplace creates supply-chain risk we don't want to absorb.

---

## 4. Alignment story

Five upstream initiatives we explicitly align with. For each: **what we adopt**, **where we lean on them**, **where we deviate (with reason)**.

### 4.a Microsoft AGT (Agent Governance Toolkit)

**What AGT is.** Microsoft's open-source governance + secure-communication toolkit for AI agents across the M365 Copilot ecosystem and Azure. AgentMesh (Signal Protocol mesh) + governance hooks + policy primitives. Source at [microsoft/agent-governance-toolkit](https://github.com/microsoft/agent-governance-toolkit).

**Where we lean on AGT (no kars-side reinvention):**
- **AgentMesh transport** — kars uses upstream AGT AgentMesh as-is for all inter-agent messaging. No kars fork. We contribute fixes back. (Past contributions: Ed25519-Timestamp registry auth, proof-of-possession on WebSocket connect, prekey writer-lock, modern `did:mesh:` format, cross-runtime Python ↔ TypeScript wire-format alignment.)
- **AGT governance profile** — the inference-router's governance hook evaluates AGT-defined policy profiles. Tool allow-lists, KNOCK admission, trust-score floor — these vocabularies come from AGT.
- **AGT identity model** — `did:mesh:sha256(pub)[:32]` is the mesh-side identity; we plug Entra Agent ID into the same place via the auth sidecar.

**Where we add kars-specific layers on top of AGT (because they're K8s-native concerns AGT doesn't address):**
- Pod-shape policy enforcement (compiled ConfigMaps + cosign attestation + hot-reload). AGT defines the policy vocabulary; kars compiles + enforces inside the per-pod router.
- Iptables egress-guard. AGT has no concept of pod-level network confinement; that's pure kars.
- Per-pod ServiceAccount + Workload Identity / Entra Agent ID binding via K8s. AGT identity is mesh-level; kars binds it to K8s SA + federated credentials.
- Multi-runtime adapter framework. AGT speaks one wire format; kars adapts that wire to eight frameworks.
- `KarsSREAction` autonomous-remediation pattern with bounded short-lived RBAC. AGT has no equivalent.

**Where AGT covers a NIST/OWASP category for us (we don't reinvent):**

| NIST AI RMF Agentic Profile / OWASP Agentic Top 10 | Covered by AGT | Kars-side addition |
|---|---|---|
| Cross-agent communication encryption (ASI-05) | ✓ AgentMesh Signal Protocol | KNOCK gate enforcement inside the runtime adapter |
| Agent identity + DID | ✓ `did:mesh:...` format | Bind to K8s SA + Entra Agent ID |
| Mesh trust-score model | ✓ AGT scoring framework | Surface via `TrustGraph` CRD + UI |
| AGT policy profile evaluation | ✓ AGT decision hook | Compile per-sandbox + cosign-attest |
| Inter-agent message authn | ✓ X3DH + Double Ratchet headers | Mesh peer admission via `TrustGraph` CRD |

**Where AGT does *not* cover and kars provides the answer:**
- Pod-level egress confinement (ASI-07 sandbox escape mitigation) — pure kars iptables egress-guard + router.
- Upstream model API credential isolation — pure kars (router sidecar holds credentials; agent cannot read them).
- Per-sandbox token budget + content-safety enforcement at call surface — pure kars `InferencePolicy` + router.
- Sub-agent spawn governance with federated identity propagation — pure kars (router validates `spawn_policy`, controller creates child `KarsSandbox` CR).
- Cross-runtime adapter framework — pure kars.
- Autonomous SRE remediation (`KarsSREAction`) — pure kars.

**The rule of thumb:** if a capability is about *messages between agents* or *governance vocabulary across the M365 ecosystem*, lean on AGT and contribute back. If a capability is about *running agents inside Kubernetes pods with iptables-enforced isolation and CRD-driven governance*, that's kars and the work stays here. Where both apply, AGT defines the vocabulary; kars provides the K8s-native enforcement and audit substrate.

### 4.b kubernetes-sigs/agent-sandbox

**What it is.** The K8s SIG Apps subproject defining a `Sandbox` CRD (`agents.x-k8s.io/v1beta1`) — a `podTemplate` + `volumeClaimTemplates` + `lifecycle` + `operatingMode` abstraction for stateful singleton workloads.

**How kars composes:** `spec.upstreamCompatibility.sigsAgentSandbox` accepts four values (`off` / `observe` / `translate` / `overlay`). `overlay` is shipped today: upstream `Sandbox` owns the Pod; kars owns the governance overlay (namespace, ServiceAccount, NetworkPolicy, compiled policy ConfigMaps). Native (`off`) is the default; `observe` and `translate` are scaffolded.

**Honest gap:** today's overlay is *governance* overlay, not *hardening* overlay — the kars router sidecar and egress-guard init container are not injected when the upstream `Sandbox` owns the Pod. Closing this is on the roadmap with four paths (documented hardened `podTemplate` snippet → kars-shipped `SandboxTemplate` → optional MutatingAdmissionWebhook → upstream sidecar-profile primitive). See [agentgateway-vs-kars-router-analysis.md](agentgateway-vs-kars-router-analysis.md) and the parity plan.

**Active in-flight upstream PRs we're tracking:**
- [PR #854](https://github.com/kubernetes-sigs/agent-sandbox/pull/854) — `agents.x-k8s.io/trusted-init-containers` annotation; once merged, our egress-guard adds the annotation and the SIG VAP lets us through.
- [PR #967](https://github.com/kubernetes-sigs/agent-sandbox/pull/967) — Cilium egress example on GKE Dataplane v2; alternative egress confinement story for Cilium environments, composes with our iptables variant.
- [PR #850](https://github.com/kubernetes-sigs/agent-sandbox/pull/850) — Envoy + ext_proc data plane RFC; if adopted, kars governance hooks become a natural ext_proc filter.

### 4.c agentgateway (Solo.io / Linux Foundation)

**What it is.** LF-hosted, Solo.io-led centralised gateway data plane (Gateway API `GatewayClass`). Multi-vendor backed (Microsoft, Dell, CoreWeave, T-Mobile, UBS, Akamai, Nirmata). 10+ LLM providers, 6+ guardrail integrations, virtual keys, MCP federation, CEL-RBAC.

**How kars composes:** agentgateway sits at the cluster edge in front of the model-serving fleet; kars's router sits per-pod inside every agent sandbox. An agent's model call leaves the agent through the kars router (mints credentials, enforces semantic policy, decrements per-sandbox token budget), traverses the cluster network governed by Istio / mTLS, and may reach a model deployment fronted by agentgateway (LLM-side traffic management, load balancing, content-based routing, virtual keys for cost allocation). Each layer does what only it can do.

**Honest distinction:** the kars inference router is **not** a gateway in agentgateway's sense — it is a per-pod *trust boundary* with `cardinality = 1 caller per router instance` and `caller-is-adversarial` threat model. agentgateway is `cardinality = many callers per gateway` and `caller-is-authenticated-client` threat model. They sit at different layers and the difference is structural, not feature-list. See [agentgateway-vs-kars-router-analysis.md](agentgateway-vs-kars-router-analysis.md).

### 4.d NIST AI RMF Agentic Profile + OWASP Agentic Top 10

**What they are.** NIST AI RMF Agentic Profile (CSA draft March 2026) extends NIST AI RMF 1.0 with autonomy-tier-aware GOVERN/MAP/MEASURE/MANAGE extensions. OWASP Top 10 for Agentic Applications (ASI-01 .. ASI-10, 2026) is the authoritative threat taxonomy.

**Where kars maps:** best-in-class on ASI-05 (Inter-Agent Communication — via AGT AgentMesh) and ASI-07 (Unexpected Code Execution — via four-layer defense). Competitive on ASI-02 (Tool Misuse), ASI-03 (Identity), ASI-04 (Memory), ASI-06 (Supply Chain). Behind on ASI-08 (Cascading Failures), ASI-09 (Human-Trust), ASI-10 (Behavioral Drift). Eleven concrete gaps documented in [sota-agentic-ai-capability-map.md](sota-agentic-ai-capability-map.md) — sized at ~33–44 engineer-weeks total, sequenced into Tier 1/2/3 in §6 below.

**Where AGT helps us cover NIST/OWASP:** ASI-05 entirely (mesh encryption + KNOCK + trust scores), ASI-03 partially (DID format + trust progression). NIST AI RMF GOVERN-extension autonomy tiers are not in AGT; kars adds them via `KarsSandbox.spec.autonomy.level` (1..5) — see [`dev-experience-design-note.md`](dev-experience-design-note.md) Capability 1.

### 4.e Kubernetes baseline (KEP-753 sidecars, Pod Security restricted, NetworkPolicy)

We use **only standard, current K8s primitives**: KEP-753 native sidecar containers (1.28+; not pre-KEP hacks), Pod Security Standards `restricted` profile, `defaultDeny: true` NetworkPolicy, ServiceAccount-based Workload Identity, OpenTelemetry GenAI semantic-convention spans, Helm chart packaging, cosign-signed images. The egress-guard is the only init container that needs `CAP_NET_ADMIN` + `CAP_NET_RAW`, and it exits before workload containers start.

The one place we deviate from "use what K8s ships" is AgentMesh (we use Microsoft AGT Signal Protocol rather than mTLS-via-Istio) — the threat-model justification is in §4.a above.

---

## 5. Customer insertion paths

Six common situations. For each: what kars contributes; what stays the customer's existing investment; how installation looks.

### 5.a "I already run Istio"

Istio handles pod-to-pod mTLS, request-level authorization at the gateway, ambient-mode multicluster. **Keep Istio.** Kars adds the per-pod trust boundary inside agent pods (router sidecar + egress-guard) and the governance CRD plane. Compose: agent → kars router → out of pod → Istio handles wire — each layer does what only it can do. No conflict.

### 5.b "I already run agentgateway"

Keep agentgateway as the centralised LLM/MCP/A2A data plane. Kars adds the per-pod agent runtime + trust boundary + multi-runtime adapters + AgentMesh. The agent's model call: agent → kars router (mint credentials, enforce per-sandbox policy) → traverse cluster network → agentgateway (provider routing, virtual keys, guardrails) → model. The composition is documented in §4.c above; we will ship a worked example.

### 5.c "I already use the SIG `Sandbox` workload primitive"

Set `spec.upstreamCompatibility.sigsAgentSandbox: overlay` on your `KarsSandbox`. Upstream `Sandbox` continues to own the Pod, lifecycle, PVC, hostname identity. Kars provides the governance overlay (namespace, ServiceAccount, NetworkPolicy, compiled policy ConfigMaps). Hardening overlay (router + egress-guard injection) lands when we ship the kars `SandboxTemplate` upstream, then your existing `SandboxClaim`s can target the hardened template by reference.

### 5.d "I run agents on my own and have no agent infra yet"

`git clone Azure/kars && cd kars && make build && kars dev` brings up a working agent inside a kind cluster in ~3 minutes. For production: `kars up --resource-group <rg>` deploys to AKS with Foundry-shaped defaults. Greenfield path; nothing else to integrate.

### 5.e "I'm regulated / sovereign / airgapped"

The [`docs/blueprints/`](../blueprints/00-index.md) directory covers four scenarios: enterprise-self-hosted, sovereign-airgapped, cross-org-federation, managed-public. Each blueprint declares which kars features are required, which are optional, which network egress is allowed, and which compliance controls map to which kars CRDs. Cosign-attested allowlists + confidential-VM-per-sandbox + per-call audit trail are the differentiators that matter most for this audience.

### 5.f "I'm a developer who wants to run an agent for my own work"

Pick a recipe from the standard catalog (`kars task new research-brief "investigate X"`), or write a custom one. The intake orchestrator (when shipped — see [`dev-experience-design-note.md`](dev-experience-design-note.md)) picks the right recipe from natural-language description. Per-task chat surface via the kars-native conversation ingress. Artifacts (PR drafts, briefs, notebooks) land back attached to the `KarsTask` resource.

---

## 6. Roadmap

Two lenses: **by theme** (so an engineer can pick up the right work item) and **by tier/quarter** (so a manager can plan).

### 6.a By theme

| Theme | Items | Source doc |
|---|---|---|
| Provider matrix expansion | Anthropic / Bedrock / Gemini / Vertex / Ollama / vLLM native providers in router | [parity plan](agentgateway-parity-plan.md) items 3-6 |
| Guardrail coverage | Bedrock Guardrails / Model Armor / OpenAI Moderation / multi-layer chain | parity plan items 7-10 |
| Virtual-key budgets + cost tracking | Per-API-key budgets matching agentgateway | parity plan item 11 |
| MCP federation + CEL RBAC | Virtual-MCP + CEL-based tool authz | parity plan items 13-14 |
| Autonomy tier classification | `spec.autonomy.level: 1..5` on `KarsSandbox` + per-level HITL defaults | [SOTA gap](sota-agentic-ai-capability-map.md) GAP-6 + [DX](dev-experience-design-note.md) DX-0 |
| Behavioral drift detection | Per-sandbox anomaly score; flag rogue patterns | SOTA GAP-1 (ASI-10 + AAGATE Behavioral Analytics) |
| Delegation chain depth limit + monitoring | Cap depth; visualize tree; per-chain action-cost ceiling | SOTA GAP-3 (ASI-08 + NIST MEASURE) |
| Fleet-wide kill switch | Cluster-scoped CRD pauses all matching sandboxes via `spec.suspended` | SOTA GAP-8 (AAGATE GOA) |
| Human-in-the-loop framework | Beyond `KarsSREAction`; per-recipe / per-call gates | SOTA GAP-5 (ASI-09 + NIST GOVERN) |
| Tool poisoning detection | Attest MCP tool descriptions; detect mid-flight drift | SOTA GAP-4 (ASI-02 + MCPSHIELD) |
| Sub-agent spawn governance hardening | Validate target + inherit creds + propagate audit across spawn chains | parity plan item 1 (agent-runtime differentiator) |
| Unified per-agent action-cost ledger | Model + tool + MCP + mesh + spawn in one ledger (vs agentgateway's model-only) | parity plan item 2 |
| Mesh-aware QoS | Per-peer rate-limit + fair-share + KNOCK-aware budget | parity plan item 18 |
| SIG alignment | Hardened `podTemplate` snippet → ship `SandboxTemplate` → MutatingAdmissionWebhook | parity plan, [SIG section](competitive-positioning-2026-06.md) |
| DX foundation | `KarsRecipe` + `KarsTask` + per-task-kind handover patterns + kars-native conversation ingress | [DX design note](dev-experience-design-note.md) DX-1..DX-3 |
| DX visibility + state | Web UI task feed + tree + recipe browser; `KarsArtifact`; `KarsProject` (project brain); intake orchestrator | DX-4..DX-7 |
| Regret-free undo | Per-action undoability + explanation; closes the autonomy-tier loop | DX-8 |
| Continuous compliance + drift monitoring | AAGATE ComplianceAgent + QSAF analogs | SOTA GAP-9, GAP-10 (long horizon) |
| Information flow tracking | Taint propagation across mesh sends; aggregation-risk detection | SOTA GAP-11 (research grade) |

### 6.b By tier / quarter

**Tier 1 — Q3 2026 (6–8 weeks, high impact + low effort)**

1. `dx-DX-0` / `sota-GAP-6` — Autonomy Level 1..5 schema (foundational; one primitive closes both a NIST gap and the UX foundation; 2 weeks)
2. `sota-GAP-2` — Multi-layered guardrail chain (also parity item 10; 2-3 weeks)
3. `sota-GAP-5` — Human-in-the-loop framework beyond `KarsSREAction` (depends on GAP-6; 2-3 weeks)
4. `sota-GAP-8` — Fleet-wide millisecond kill-switch (1 week; small effort, large incident-response value)
5. Parity items 3-7 — Anthropic / Bedrock / Gemini native LLM providers + Bedrock Guardrails (~3-4 weeks)
6. SIG item — ship documented hardened `podTemplate` snippet for overlay mode

**Tier 2 — Q4 2026 (8–10 weeks, surface parity + foundational DX)**

1. DX-1 — `KarsRecipe` CRD + reconciler + standard catalog
2. DX-2 — `KarsTask` CRD + `kars task new` CLI
3. DX-3 — kars-native conversation ingress (router extension)
4. Parity item 5+8 — Gemini/Vertex + Model Armor
5. Parity items 11-12 — Virtual keys with per-key budgets + cost dashboard
6. Parity item 13 — MCP federation
7. SOTA GAP-1 — Behavioral baseline + drift detection
8. SIG item — ship kars-hardened `SandboxTemplate` upstream

**Tier 3 — Q1 2027 (procurement-grade + tail features)**

1. v1 API stability + CNCF Sandbox application
2. SOTA GAP-3 — Delegation chain depth limit + monitoring
3. SOTA GAP-7 — Principled decommissioning lifecycle
4. SOTA GAP-4 — Tool poisoning detection
5. DX-4..DX-8 — Web UI + artifacts + project memory + intake orchestrator + regret-free undo
6. Parity item 14-15 — CEL RBAC + OpenAI Realtime
7. SOTA GAP-9, GAP-10 — Continuous compliance evaluation + QSAF
8. SOTA GAP-11 — Cross-agent information flow tracking (research grade)

---

## 7. Guardrails — what we will NOT do

These prevent over-engineering and category drift. If a feature request triggers one of these, it gets rejected at the design-review gate, not after the work is done.

1. **We will NOT ship an OpenAI-compatible / Anthropic-compatible front-door endpoint** as a kars feature. That is agentgateway's category. Customers needing this run agentgateway in front of the cluster, kars inside the cluster.
2. **We will NOT build a centralized gateway alternative to agentgateway.** Our differentiation is *per-pod trust boundary*, not better-gateway-than-the-gateway-people.
3. **We will NOT displace the SIG `Sandbox` primitive.** Compose on top via overlay mode; contribute upstream when the integration is worth standardizing.
4. **We will NOT build an in-house workflow engine.** Temporal / Argo / LangGraph-the-platform exist; compose on top if a customer wants them.
5. **We will NOT ship a no-code drag-and-drop agent builder.** CRD authority is essential to the governance story; trading it for accessibility weakens what makes kars defensible.
6. **We will NOT host a community recipe marketplace.** Per-team catalogs only; community marketplaces are supply-chain risk we don't want to absorb.
7. **We will NOT fork AGT.** All AGT contributions go upstream. Our internal `vendor/agt/pin.json` is a tracking pin, not a fork point.
8. **We will NOT add features without a "what gap does this close" answer.** Every new CRD, controller, router module, or UX surface justifies itself against one of: the seven irreducible advantages (§2), the alignment story (§4), a documented SOTA gap (§6), or a documented customer-insertion path (§5). If it doesn't, it doesn't ship.

---

## 8. Open decision asks

The user's confirmation is required before we move from "plan" to "execution" on any of these.

| # | Decision | Default if not raised | Owner |
|---|---|---|---|
| D1 | Generator for the public docs site (Job 1 of the 2026-06-15 work plan) | MkDocs Material | @pallakatos |
| D2 | Domain for the docs site | `azure.github.io/kars` until v1; then `kars.dev` | @pallakatos |
| D3 | Versioned docs from day-1 vs add at v1 | Add at v1 | @pallakatos |
| D4 | Announcement-blog publish target | Internal HTML draft first; user publishes | @pallakatos |
| D5 | Confirm the 11 SOTA gaps are the right framing | Confirmed unless raised | @pallakatos |
| D6 | Confirm Tier 1 priorities (DX-0/GAP-6, GAP-2, GAP-5, GAP-8) | Confirmed unless raised | @pallakatos |
| D7 | KarsProject (cross-task memory) as a new CRD vs reuse `KarsMemory` scope | New CRD | @pallakatos |
| D8 | Kars-native ingress (router extension) vs leave to operator's reverse proxy | Build kars-native ingress | @pallakatos |
| D9 | v1 API stability commitment timing | Target Q4 2026 | @pallakatos |
| D10 | Engineer-week budget allocation across themes for Q3 2026 | TBD | @pallakatos |
| D11 | Whether to publish a public "kars and the OWASP Agentic Top 10" mapping doc | Yes, draft after v1 readiness | @pallakatos |
| D12 | Recruit non-Microsoft contributors goal (count + timeline) | 3 contributors by end of Q4 2026 | @pallakatos |

---

## 9. How to use this document

- **First-time reader (engineer)**: read §1, §2, §4.a (AGT), §6.a (your theme), then the deep-dive doc cited.
- **First-time reader (architect)**: read §1, §2, §3, §4, §5 (whichever insertion path matches the customer), §7.
- **First-time reader (manager / lead)**: read §1, §6.b, §7, §8.
- **Strategy review meeting**: walk through §8 decisions; each item has a default the meeting can either accept or escalate.
- **PR review on a new feature**: check §7 (guardrails) and §6 (does the feature map to a documented theme?). If neither check passes, the feature needs a section §2 update or it doesn't ship.

The seven deep-dive docs listed in the pre-read remain authoritative for their topics. This plan is the index + the synthesis; it does not replace them.
