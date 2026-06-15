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

**What AGT is.** Microsoft's open-source runtime governance + secure-communication toolkit for AI agents (GA April 2026). Source at [microsoft/agent-governance-toolkit](https://github.com/microsoft/agent-governance-toolkit). 37+ MB repo, 600+ forks at time of writing, very active. Significantly broader than "just AgentMesh" — AGT ships an entire runtime governance stack.

**AGT capability surface (verified via [microsoft.github.io/agent-governance-toolkit](https://microsoft.github.io/agent-governance-toolkit) + the [April 2026 announcement post](https://opensource.microsoft.com/blog/2026/04/02/introducing-the-agent-governance-toolkit-open-source-runtime-security-for-ai-agents/)):**

- **AgentMesh** — Signal Protocol mesh; the inter-agent transport kars uses.
- **Privilege Rings** — four CPU-style isolation tiers for agents based on risk.
- **Saga-Orchestration** — multi-step transaction handling with atomic rollback on policy violation.
- **Automatic Kill-Switch** — runtime termination of agents exhibiting policy-violating or anomalous behaviour. Manual override via dedicated API. Operates at Ed25519-identified per-agent granularity.
- **Policy Enforcement Runtime** — every agent action intercepted; supports YAML / OPA Rego / Amazon Cedar policy languages; tamper-evident audit log on every denial.
- **SRE & Compliance Overlay** — SLO monitoring + circuit breakers + chaos testing + progressive delivery; agents auto-killed or quarantined on SLO breach.
- **Runtime Evidence & Compliance** — cryptographically verifiable proofs on every kill / denial for EU AI Act / Colorado AI Act audit.
- **Framework adapters** — LangChain, CrewAI, OpenAI Agents, Microsoft Agent Framework, Google ADK, others; works as middleware / sidecar / standalone service.
- **All 10 OWASP Agentic Top 10 categories addressed** at the runtime layer per the official AGT messaging.

**Where we lean on AGT (no kars-side reinvention):**

- **AgentMesh transport** — kars uses upstream AGT AgentMesh as-is. No kars fork. We contribute fixes back. (Past contributions: Ed25519-Timestamp registry auth, proof-of-possession on WebSocket connect, prekey writer-lock, modern `did:mesh:` format, cross-runtime Python ↔ TypeScript wire-format alignment.)
- **AGT policy enforcement runtime** — the inference-router's governance hook evaluates AGT-defined policy profiles. Tool allow-lists, KNOCK admission, trust-score floor — these vocabularies come from AGT. We adopt AGT's policy languages (YAML / OPA Rego / Cedar) rather than invent another DSL.
- **AGT identity model** — `did:mesh:sha256(pub)[:32]` is the mesh-side identity; Ed25519 keypair per agent.
- **AGT kill-switch primitive** — for fleet-wide emergency termination of agents matching policy / behavioural criteria. We do **not** build a competing kill-switch in kars; we provide the K8s-native invocation path (CRD that label-selects sandboxes + calls AGT kill API).
- **AGT runtime evidence** — for cryptographic kill / denial proofs that satisfy EU AI Act / Colorado AI Act.

**Where we add kars-specific layers on top of AGT (K8s-native concerns AGT doesn't address):**

- Pod-shape policy enforcement (compiled ConfigMaps + cosign attestation + hot-reload). AGT defines the policy vocabulary; kars compiles + enforces inside the per-pod router.
- Iptables egress-guard. AGT's privilege rings are process / API-level isolation; kars adds kernel-level network confinement.
- Per-pod ServiceAccount + Workload Identity / Entra Agent ID binding via K8s. AGT identity is mesh-level; kars binds it to K8s SA + federated credentials.
- Multi-runtime adapter framework. AGT's framework adapters are integration points; kars provides the K8s-side runtime contract.
- `KarsSREAction` autonomous-remediation pattern with bounded short-lived RBAC. AGT has its own SRE overlay; kars's variant is K8s-CRD-native + ties into `kubectl` workflows.
- The cross-runtime mesh story (Hermes Python ↔ OpenClaw TypeScript verified end-to-end on AKS) — built on AGT primitives but stitched together by kars.

**Where AGT covers a NIST/OWASP category for us (we don't reinvent — we surface):**

| NIST AI RMF / OWASP Agentic Top 10 | Covered by AGT today | Kars-side addition |
|---|---|---|
| Inter-agent communication encryption (ASI-05) | ✓ AgentMesh Signal Protocol | KNOCK gate enforcement inside the runtime adapter |
| Agent identity + DID (ASI-03) | ✓ `did:mesh:` + Ed25519 | Bind to K8s SA + Entra Agent ID |
| Mesh trust-score model | ✓ AGT scoring framework | Surface via `TrustGraph` CRD + UI |
| Policy enforcement at action level (ASI-02) | ✓ Policy runtime + YAML / OPA / Cedar | Compile per-sandbox + cosign-attest, mount as ConfigMap |
| Inter-agent message authn | ✓ X3DH + Double Ratchet + Ed25519-Timestamp | Mesh peer admission via `TrustGraph` CRD |
| **Kill-switch (cascading-failure response, ASI-08)** | **✓ AGT automatic + manual kill** | **K8s-native invocation: `KarsKillSwitch` CRD label-selects sandboxes → calls AGT kill API** |
| **SLO breach → circuit breaker / quarantine** | **✓ AGT SRE overlay** | **K8s-native: SRE agent watches `KarsSandbox` workload health → AGT quarantine API on breach** |
| **Cryptographic kill / denial proofs (EU AI Act, Colorado AI Act)** | **✓ AGT runtime evidence** | **Stream proofs into K8s Events + per-sandbox audit volume** |
| Tamper-evident action audit log | ✓ AGT audit log per denial | Mirror into per-sandbox audit volume for K8s-Operator visibility |
| All 10 OWASP Agentic Top 10 at runtime layer | ✓ AGT messaging claim | Kars validates this end-to-end + adds the K8s-native enforcement surfaces |

**Where AGT does *not* cover and kars provides the answer:**

- Pod-level egress confinement (ASI-07 sandbox-escape mitigation) — pure kars iptables egress-guard.
- Upstream model API credential isolation (the agent never holds the credential, even AGT cannot enforce this at the API layer alone) — pure kars (router sidecar holds credentials; agent UID 1000 cannot read).
- Per-sandbox token budget enforcement at K8s namespace + Service surface — pure kars `InferencePolicy` + router.
- Sub-agent spawn via K8s `KarsSandbox` CR creation with federated identity propagation — pure kars (router validates `spawn_policy`, controller creates child CR).
- Cross-runtime adapter framework spanning Python / TypeScript / Go / Rust — pure kars's eight runtime adapters.
- Confidential-VM-per-sandbox flag on K8s workloads — pure kars `spec.sandbox.isolation: confidential`.

**The rule of thumb:** if a capability is about *agent action policy* or *messages between agents* or *runtime kill-switch* or *cryptographic compliance evidence*, lean on AGT and contribute back — AGT already provides this and is the cross-product M365-ecosystem standard. If a capability is about *running agents inside Kubernetes pods with kernel-level isolation, K8s CRDs, controllers, ServiceAccount-based identity, and operator workflows*, that's kars and the work stays here. AGT defines the agent-governance vocabulary; kars provides the K8s-native enforcement, audit, and operator-surface substrate. **We do not duplicate AGT primitives; we make them K8s-CRD-shaped.**

### 4.b kubernetes-sigs/agent-sandbox

**What it is.** The K8s SIG Apps subproject defining a `Sandbox` CRD (`agents.x-k8s.io/v1beta1`) — a `podTemplate` + `volumeClaimTemplates` + `lifecycle` + `operatingMode` abstraction for stateful singleton workloads.

**How kars composes:** `spec.upstreamCompatibility.sigsAgentSandbox` accepts four values (`off` / `observe` / `translate` / `overlay`). `overlay` is shipped today: upstream `Sandbox` owns the Pod; kars owns the governance overlay (namespace, ServiceAccount, NetworkPolicy, compiled policy ConfigMaps). Native (`off`) is the default; `observe` and `translate` are scaffolded.

**Honest gap:** today's overlay is *governance* overlay, not *hardening* overlay — the kars router sidecar and egress-guard init container are not injected when the upstream `Sandbox` owns the Pod. Closing this is on the roadmap with four paths (documented hardened `podTemplate` snippet → kars-shipped `SandboxTemplate` → optional MutatingAdmissionWebhook → upstream sidecar-profile primitive). See [agentgateway-vs-kars-router-analysis.md](agentgateway-vs-kars-router-analysis.md) and the parity plan.

**Active in-flight upstream PRs we're tracking:**
- [PR #854](https://github.com/kubernetes-sigs/agent-sandbox/pull/854) — `agents.x-k8s.io/trusted-init-containers` annotation; once merged, our egress-guard adds the annotation and the SIG VAP lets us through.
- [PR #967](https://github.com/kubernetes-sigs/agent-sandbox/pull/967) — Cilium egress example on GKE Dataplane v2; alternative egress confinement story for Cilium environments, composes with our iptables variant.
- [PR #850](https://github.com/kubernetes-sigs/agent-sandbox/pull/850) — Envoy + ext_proc data plane RFC; if adopted, kars governance hooks become a natural ext_proc filter.

### 4.c agentgateway (Solo.io / Linux Foundation)

**What it is.** LF-hosted, Solo.io-led centralised gateway data plane (Gateway API `GatewayClass`). Multi-vendor backed (Microsoft, Dell, CoreWeave, T-Mobile, UBS, Akamai, Nirmata). 10+ LLM providers, 6+ guardrail integrations, virtual keys, MCP federation, CEL-RBAC. They also position for inter-agent communication (A2A protocol routing at the gateway).

**The hardest evaluator question: is the kars router redundant if I have agentgateway?**

Structurally, no — but the marketing surface overlaps enough that the answer needs to be explicit. **Seven irreducible architectural differences** make the kars router non-redundant in the use cases kars exists to serve:

1. **What the agent process holds.** Without the kars sidecar, the agent process holds *some* credential (gateway-auth token, JWT, API key) to call agentgateway. With kars, iptables-level UID-based egress redirect means the agent process holds **nothing** — no credential to exfiltrate if prompt-injected. The whole kars threat model rests on "agent process holds zero credentials"; this is only deliverable with a per-pod sidecar + iptables redirect, which is not agentgateway's deployment shape.
2. **Egress confinement at kernel level.** NetworkPolicy operates on 5-tuples. The kars egress-guard operates on UID — even an in-process attacker who controls the agent's user-space cannot make an outbound connection that does not go through the sibling sidecar, regardless of destination. agentgateway-as-cluster-edge cannot deliver per-pod egress confinement; the agent can directly call any cluster service or external host NetworkPolicy permits.
3. **Per-pod blast radius.** A compromised cluster-edge gateway is a *single point of compromise* for every credential, every routing rule, every caller's traffic. A compromised kars sidecar is one sandbox. Per-pod isolation isn't a feature — it's a deployment-shape property.
4. **Sub-agent spawn governance.** `kars_spawn` is the only path by which an agent can create a sub-agent — which means: a new `KarsSandbox` CR, a new namespace, a federated credential, an inherited audit context. This is *agent-runtime work*, not gateway work. There is no version of agentgateway that does this because it isn't an agent runtime.
5. **Confidential-VM compatibility.** When `spec.sandbox.isolation: confidential` runs the sandbox on AMD SEV-SNP / Intel TDX, the trust boundary terminates at the pod. The kars router is inside the confidential VM with the agent — credentials never leave the encrypted memory region. Agentgateway is at the cluster edge, outside the confidential VM; reaching it defeats the isolation guarantee at the credential-mint step.
6. **Per-sandbox compiled policy + cosign attestation.** Agentgateway's policies are gateway-wide CRDs (xDS-distributed). Kars compiles *per-sandbox* policy bundles with byte-deterministic layout + cosign signatures + hot-reload via ConfigMap watch. Different policy distribution model; cosign-attested per-sandbox supply-chain proof is not something agentgateway provides.
7. **Multi-runtime governance.** The kars router knows the runtime kind (Hermes / OpenClaw / MAF / LangGraph / etc.) and applies runtime-specific policy (Hermes auto-responder env stripping, OpenClaw plugin-singleton guard, the cross-runtime mesh interop bridge). Agentgateway is runtime-agnostic by design — strength as a cluster-edge gateway, structural limit as an agent-runtime governance layer.

**Agentgateway's A2A / inter-agent communication is not a substitute for AGT mesh (via kars).** Agentgateway carries A2A protocol bytes at the gateway with TLS to the gateway, gateway-sees-plaintext, TLS to the peer. AGT mesh (which kars uses) uses Signal Protocol X3DH + Double Ratchet: broker sees only DIDs + ciphertext, forward secrecy is per-message, post-compromise security after the next ratchet step, KNOCK gate per-message, trust-score progression, cross-org without putting the broker in both orgs' trust sets. **agentgateway cannot reach AGT mesh's properties by adding features — it is a structural difference about where the encryption terminates** (same way an HTTPS proxy isn't equivalent to PGP-encrypted email). For intra-cluster A2A in a single-team / single-org / fully-trusted-broker setting, agentgateway's A2A routing is appropriate and kars composes with it. For multi-team / multi-org / sovereign / regulated deployments that cannot afford broker-in-trust-set, AGT mesh via kars is the architectural answer.

**When agentgateway IS sufficient on its own (the honest case).** If a customer matches NONE of: ≥2 teams sharing the cluster · adversarial-agent threat model (prompt injection assumed) · regulated / sovereign / airgapped · multi-runtime fleet · cross-org agent federation with E2E secrecy · sub-agent spawn governance · confidential-VM isolation — then agentgateway alone is sufficient and kars is overkill. We say so. We do not try to convert those customers; we want them to succeed with agentgateway.

**Where kars genuinely earns its keep alongside agentgateway.** The seven properties above each correspond to a customer scenario kars exists to serve and that agentgateway cannot. The composition story in those scenarios is: agentgateway at the cluster edge for LLM/MCP traffic management (provider matrix, virtual keys, content-based routing, guardrails); kars per-pod for trust boundary + agent-runtime governance + AGT-mesh inter-agent secrecy. The agent's model call: agent → kars router (mint credentials inside the pod, enforce per-sandbox policy, decrement per-sandbox token budget) → traverse cluster network governed by Istio + mTLS → agentgateway (provider routing, load balancing, virtual keys for cluster-wide cost allocation, gateway-level content-based routing) → model deployment. Each layer does what only it can do; neither is doing the other's work. See [agentgateway-vs-kars-router-analysis.md](agentgateway-vs-kars-router-analysis.md) for the full architectural comparison.

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

> **Purpose.** Every roadmap item answers one question: **"what specific customer outcome does this unblock, and what research evidence says it matters?"** Items without a clear answer don't ship. The mission this roadmap serves is precise: **be the number-one enterprise-focused secure runtime for running AI agents on Kubernetes — easy to adopt, surgically engineered for SOTA agentic use cases, and architecturally clear enough that the buyer immediately understands when kars fits and when it doesn't.**

### 6.a Evidence base — what we built this roadmap on

Five independent evidence streams. Every item in §6.b cites at least one.

**E1. Enterprise adoption blockers (analyst surveys, 2026):**
- *Gartner press release 2026-05-26*: **40% of organisations will retire autonomous agents by 2027** specifically because of governance failures discovered *after* incidents. Root cause: applying uniform governance instead of proportional (autonomy-tier-aware) governance.
- *Forrester State of Agentic AI 2026*: **49% of CISOs view agentic AI as a pressing risk**; **88% of agent pilots never reach production**, with governance friction + agentic observability gaps as the top-two cited reasons; **only 31% of enterprises have any agent in production today** despite 80%+ embedding "an AI agent" somewhere.
- *Anthropic 2026 State of AI Agents Report*: in-production agent platforms succeed when they ship **measurable ROI within 12 months** + **clear ownership ("AI agent owners / agentic ops leads")** + **multi-agent observability** + **graceful fallback / human-in-the-loop on ambiguity**.

**E2. SOTA security literature (verified 2026-06-15):**
- OWASP Top 10 for Agentic Applications 2026 (ASI-01..ASI-10) — see [sota-agentic-ai-capability-map.md](sota-agentic-ai-capability-map.md) for the per-category mapping.
- NIST AI RMF Agentic Profile (CSA draft, March 2026) — GOVERN extension demands **autonomy tier classification** with proportional oversight (exactly the Gartner finding above, but from a standards-body angle).
- AAGATE reference architecture (CSA, December 2025) — 8-component K8s overlay including behavioral analytics, QSAF cognitive drift, AGT-equivalent kill-switch.
- MCPSHIELD formal framework (arXiv 2604.05969) — no single defense covers >34% of MCP threat vectors; defense-in-depth is mandatory.

**E3. Enterprise use-case categories where agentic AI is in production today:**
1. **Coding** (JPMorgan 450+ use cases incl. PR review + autonomous bug-fix; software engineering automation).
2. **Research / knowledge work** (literature analysis, report generation, BI automation).
3. **SRE / DevOps** (autonomous incident triage, log analysis, runbook execution — exactly the kars-sre slice we just shipped).
4. **Customer ops** (Klarna's 853-FTE-equivalent customer agent; multi-channel escalation).
5. **Compliance / regulatory** (transaction screening, automated audit trail creation, regulatory reporting).
6. **Data analysis** (notebook-style multi-step analytical workflows).
7. **Security operations** (threat-model generation, vulnerability validation, patch generation — the Orka pattern).

**E4. Upstream capability landscape (what we don't need to build because someone else owns it):**
- **AGT** ships kill-switch, policy enforcement runtime, SLO-driven circuit breakers, tamper-evident audit log, cryptographic compliance proofs, framework adapters for 5+ frameworks. **We invoke AGT primitives via K8s-native CRDs rather than rebuilding them.**
- **agentgateway** ships 10+ LLM providers, 6 guardrails, virtual keys, MCP federation, CEL-based RBAC. **We compose, we don't replace.**
- **agent-sandbox SIG** ships the K8s `Sandbox` workload primitive + warm pools + lifecycle. **We compose via overlay mode.**

**E5. Kars-shipped + verified capabilities** (so we know our starting baseline):
- Iptables egress-guard + per-pod router + 11 CRDs + AgentMesh + 8 runtime adapters + cross-runtime mesh interop + cosign-attested allowlists + KarsSREAction + Entra Agent ID integration. See §2.

### 6.b Roadmap by outcome — what each item unblocks

Every item carries: **outcome** (the customer scenario it enables), **adoption blocker it removes**, **evidence trail**, **what kars-specific work this is vs what upstream covers**, **rough effort**. Items are grouped by the **enterprise use case** they primarily serve, not by the internal theme — this keeps the roadmap legible to the buyer, not just the engineer.

#### OUTCOME ① — "I can adopt kars in 30 minutes against my existing K8s cluster, and operators trust the result"

Targets the **88% pilot failure rate** (E1 Forrester) and the **30% / 70% gap** between embedded AI and production AI deployments. If onboarding to kars is harder than the value it delivers in the first afternoon, customers stay in pilot.

| Item | What unblocks | Evidence | Kars-or-upstream | Effort |
|---|---|---|---|---|
| **R-101** Single-command install on AKS / EKS / GKE with sensible defaults (Foundry-shape on AKS, OpenAI-shape on EKS/GKE) | "Time to first running governed agent" drops below 30 min. Removes pilot-onboarding friction. | E1 (pilot failure), E5 (current baseline is install-from-source) | Kars-specific (CLI + chart) | 2-3 weeks |
| **R-102** Public docs site (`microsoft.github.io/kars`) with copy-pastable quickstart + architecture diagrams + complete CRD reference, code-accuracy-audited | Cuts evaluator's first-hour confusion. The Forrester finding that "platform fragmentation slows scaling" is partially a docs-quality problem. | E1 (platform fragmentation), E5 (mdBook unpublished today) | Kars-specific | Job 1 of the 2026-06-15 plan; ~5-7 days |
| **R-103** v1 API stability commitment + CNCF Sandbox application | Procurement teams treat pre-v1 as risky. v1 + CNCF Sandbox unlocks enterprise procurement gates. | E1 (CIO concern: vendor / project stability), E5 (alpha CRDs today) | Kars-specific | Q4 2026; ~6-8 weeks of governance work |

#### OUTCOME ② — "Each agent gets the right level of oversight for what it's doing"

Directly targets the Gartner finding (E1: 40% will retire agents due to *uniform-governance failure*) and the NIST AI RMF Agentic Profile GOVERN extension. The single highest-leverage architectural change kars needs.

| Item | What unblocks | Evidence | Kars-or-upstream | Effort |
|---|---|---|---|---|
| **R-201** Autonomy Tier 1..5 schema on `KarsSandbox` + per-level default policy bundles (read-only / HITL / shared / conditional / supervised) | Compliance teams can sign off on Tier 1-2 agents (low-risk) without blocking Tier 4-5 review. Removes the "everything goes through the same review queue" bottleneck. | E1 Gartner (proportional governance), E2 NIST GOVERN extension | Kars-specific (the CRD field + per-level defaults); leverages AGT's policy runtime to enforce the tier-defaulted policy | 2 weeks |
| **R-202** Per-tier default HITL gates (every tool call / EgressApproval gated / approval per checkpoint / kill-only) | Tier-aware HITL replaces the binary "locked-down vs trusted" CIO concern with a graded model. | E1 CIO concerns + Gartner | Kars wires K8s-native approval surfaces; AGT runtime evaluates the per-action policy | 2-3 weeks; depends on R-201 |
| **R-203** General human-in-the-loop framework (operator approval surface in CLI + web UI, not just KarsSREAction) | Today only KarsSREAction has HITL. Coding / research / data-analysis tasks have no general HITL surface — every team builds their own escape valve. | E1 (every successful production deployment has clear HITL escalation), E3 (all 7 use-case categories need it) | Kars-specific (CRD + UI); AGT provides the underlying policy decision | 2-3 weeks; depends on R-201 |
| **R-204** Principled decommissioning lifecycle: revoke fed creds, deprovision Entra Agent ID, archive audit, freeze state, lock namespace | Closes the "agent owner left the team, agent still running" gap. Required by NIST MANAGE extension. | E2 NIST MANAGE extension, E1 (Gartner "shadow AI" concern) | Kars-specific K8s-side; calls AGT to revoke mesh identity | 2 weeks |

#### OUTCOME ③ — "I can run agents from multiple teams against shared models with provable cost + governance separation"

The original multi-tenant enterprise problem kars exists to solve. Targets the 49% CISO concern (E1) and the cost-attribution + ownership requirements (E1 Anthropic).

| Item | What unblocks | Evidence | Kars-or-upstream | Effort |
|---|---|---|---|---|
| **R-301** Native LLM providers in router: Anthropic, AWS Bedrock, Google Gemini / Vertex AI, Ollama, vLLM | Removes the "kars is Azure-only" perception. Multi-cloud enterprises and sovereign deployments require this. | E1 (platform fragmentation), agentgateway parity gap | Kars-specific router work; no AGT dependency | 4-6 weeks across 5 providers |
| **R-302** Native guardrails: AWS Bedrock Guardrails, Google Model Armor, OpenAI Moderation; multi-layer chain in `InferencePolicy.contentSafety` | Bedrock / GCP customers expect their guardrail stack to work. Multi-layer chain is what AAGATE LPCI prescribes. | E2 AAGATE LPCI, E3 (regulated industries require provider-native guardrails) | Kars router modules; AGT policy runtime is orthogonal | 4-5 weeks |
| **R-303** Per-API-key virtual keys with budget + cost tracking (per-team, per-task, per-tier) | Cost attribution is the #1 finance / FinOps blocker for multi-team agent platforms. | E1 (ROI requirement); E3 all 7 categories | Kars-specific (CRD + router accounting); composes with agentgateway's virtual-key model for cluster-edge gateway scenarios | 2 weeks |
| **R-304** Unified per-agent action-cost ledger spanning model + tool + MCP + mesh + spawn | Agentgateway tracks model spend only. Real agent cost includes tool calls (e.g. expensive APIs), MCP backends, sub-agent spawn cycles. One ledger = one finance report. | E1 (ROI requirement, "measurable cost cuts within 12 months"); pure agent-runtime concern agentgateway cannot solve | Kars-specific (router + controller) | 2 weeks |
| **R-305** Cost-tracking dashboard in Grafana with per-team / per-tier / per-task breakdown | Operationalises R-303 + R-304 in the surface FinOps actually looks at. | E1 (need formal ROI evaluation framework) | Kars-specific (dashboard); reads from existing metrics | 3 days |
| **R-306** SIG-aligned hardened `SandboxTemplate` upstream → `SandboxClaim` from kars-template gives full hardening (router + egress-guard + governance) | Customers already on the SIG primitive get kars trust boundary "for free" via a `SandboxClaim`. Removes the rip-and-replace fear. | E4 (compose with SIG), §5.c insertion path | Joint with SIG; we ship the template, contribute upstream | 2-3 weeks + community review |

#### OUTCOME ④ — "Agents can collaborate securely across runtimes, teams, organisations"

The AgentMesh story. Targets E1 Forrester finding that platform / vendor fragmentation slows enterprise agent adoption, and the E3 multi-agent pattern that 30-50% of production deployments now use.

| Item | What unblocks | Evidence | Kars-or-upstream | Effort |
|---|---|---|---|---|
| **R-401** Mesh-aware QoS — per-peer rate limit + fair-share scheduling + KNOCK-result-aware budget | Multi-agent fleets need per-peer fairness to prevent one chatty sub-agent starving another. Mesh is unique to kars; no one else has this surface. | E3 multi-agent pattern; E5 only-kars-has-this | Kars-specific surface on top of AGT mesh primitives | 2 weeks |
| **R-402** Cross-runtime mesh interop documented + a worked second-runtime-pair (currently OpenClaw ↔ Hermes; add MAF ↔ LangGraph) | Removes the "we don't believe cross-runtime is real" objection. Demos in customer convos. | E5 (cross-runtime is kars-unique); E3 multi-agent pattern | Kars-specific (interop verification + adapters); leverages AGT wire format | 2-3 weeks per new pair |
| **R-403** Cross-org A2A federation blueprint (worked example with two clusters, two orgs, mesh-bridged via AGT A2A gateway) | Cross-org agent federation (e.g. supply-chain, customer-provider) is one of the 5 documented blueprints but has no end-to-end example today. | E3 customer ops + compliance; blueprint at `docs/blueprints/05-cross-org-federation.md` | Kars composes A2A + AGT mesh; both upstream | 4 weeks |
| **R-404** Tool poisoning detection — attest MCP tool descriptions on registration; detect mid-flight drift | The CSA "Antigravity Sandbox Escape" pattern (real-world prompt-injection → tool poisoning → sandbox escape) is now the canonical agent attack. MCPSHIELD formal framework requires this layer. | E2 ASI-02 + MCPSHIELD; E1 49% CISO concern | Kars router + MCP integration; AGT policy runtime evaluates the attestation outcome | 3-4 weeks |
| **R-405** Sub-agent spawn governance hardening — validate target, inherit credentials with audit-context propagation, enforce TrustGraph at spawn time, cap delegation chain depth | The "cascading failures" (ASI-08) blast radius depends on spawn governance. Today an agent can spawn unbounded depth. | E2 ASI-08 + NIST MEASURE delegation chain monitoring; E3 multi-agent pattern | Kars-specific (spawn handler in router + controller); AGT identity provides the per-sub-agent DID | 3 weeks |

#### OUTCOME ⑤ — "Operators trust the agent fleet: behaviour stays in-bounds, incidents are diagnosed and remediated"

The "agentic observability" gap (E1 Anthropic / Forrester) and the "rogue agents / behavioural drift" risk (ASI-10).

| Item | What unblocks | Evidence | Kars-or-upstream | Effort |
|---|---|---|---|---|
| **R-501** `KarsKillSwitch` CRD: label-selects sandboxes + invokes AGT kill API for fleet-wide emergency termination | One-CRD K8s-native emergency-pause surface. AGT does the actual termination; kars provides the operator surface. | E2 AAGATE GOA + AGT capability surface | Kars wires K8s-native surface; **AGT does the kill** | 1 week |
| **R-502** Behavioural baseline + drift detection per sandbox — anomaly score on action patterns from OTel; flag drift | The ASI-10 / AAGATE Behavioral Analytics gap. AGT's SRE overlay focuses on SLO breaches; kars adds K8s-Pod-level behavioural observability. | E2 ASI-10 + AAGATE Behavioral Analytics; E1 Anthropic (observability gap) | Kars-specific (OTel post-processing pipeline + UI surface); AGT runtime emits the per-action audit events kars analyses | 4-6 weeks |
| **R-503** Delegation chain depth limit + per-chain action-cost ceiling + tree visualisation | ASI-08 cascading failure mitigation. Also operationally useful — operators see "agent fleet shape" for the first time. | E2 ASI-08 + NIST MEASURE; E3 multi-agent pattern | Kars-specific (controller + UI); AGT provides per-action audit input | 3-4 weeks |
| **R-504** SLO-breach → AGT-quarantine integration (kars SRE agent watches `KarsSandbox` workload health, invokes AGT quarantine API on breach) | Closes the loop between K8s workload observability and AGT runtime quarantine — neither side has full visibility alone. | E2 AAGATE SRE overlay + AGT capability surface | Kars SRE watcher invokes AGT API; **AGT does the quarantine** | 1-2 weeks |
| **R-505** Stream AGT compliance proofs into K8s Events + per-sandbox audit volume | EU AI Act + Colorado AI Act audit requirements satisfied by AGT cryptographic proofs; kars makes them visible at the K8s operator surface. | E2 + AGT runtime evidence capability | Kars wires the surface; **AGT generates the proofs** | 1 week |

#### OUTCOME ⑥ — "Developers can spin up the right agent for the right task without becoming kars experts"

The DX gap from [dev-experience-design-note.md](dev-experience-design-note.md). Targets the E1 finding that **88% of pilots fail** partly because the gap between "I want an agent that does X" and "I have a working `KarsSandbox` CR with the right model, tools, autonomy tier, governance refs" is too large.

| Item | What unblocks | Evidence | Kars-or-upstream | Effort |
|---|---|---|---|---|
| **R-601** `KarsRecipe` CRD + reconciler + standard catalog (research-brief, coding-task, threat-model, data-analysis, repo-scan, sre-action, chat-research) | One-line task spawning with team-approved, governance-attached, audit-bound templates. Replaces the per-agent CRD authorship cliff. | E3 (7 use-case categories ⇒ 7 starter recipes); E1 Anthropic (clear ownership / agentic-ops leads need recipe catalogs) | Kars-specific; recipes reference AGT policy profiles | 3 weeks |
| **R-602** `KarsTask` CRD + reconciler + `kars task new` CLI; per-task-kind handover patterns (PR-Draft First for coding, brief-with-citations for research, etc.) | Typed unit of work + matching UX per task kind. The "PR-Draft First" handover is what Cursor / Claude Code / Devin converged on. | E3 (all 7 categories have a typed handover); 2026 agentic-coding UX literature | Kars-specific | 2 weeks; depends on R-601 |
| **R-603** Kars-native conversation ingress (router extension or per-pod ingress sidecar; cluster-wide path-router for `/agents/<ns>/<sandbox>/chat`) | The "talk to my running agent from my browser" UX — the agent's chat surface becomes browser-reachable with kars-shaped authz. Today requires `kubectl port-forward`. | Originally raised 2026-06-15 user request; E1 Anthropic "agentic-ops leads need to see what agents are doing" | Kars-specific; not agentgateway's category (see `agentgateway-vs-kars-router-analysis.md`) | 2 weeks |
| **R-604** Web UI: task feed + chat thread + recipe browser + sub-agent task tree + HITL inbox (Headlamp plugin extension + standalone `kars-ui` Deployment) | The "agentic observability" gap from E1 + the "live multi-agent visibility" from E3. Required for ops teams to scale from one to ten to a hundred agents. | E1 Anthropic + Forrester observability; E3 multi-agent pattern | Kars-specific UI + Headlamp plugin (already exists; needs the new surfaces) | 3 weeks; depends on R-602 + R-603 |
| **R-605** `KarsArtifact` CRD + typed artifact bundling per recipe + cross-task provenance chain | Closes the loop on "what did the agent actually produce". Provenance chain is what regulated industries need for audit. | E1 ROI evidence; E3 (every category produces artifacts); E2 NIST MEASURE | Kars-specific | 2 weeks |
| **R-606** `KarsProject` CRD + cross-task project memory ("project brain") with mesh-distributed append | Long-lived project context across tasks. The Cursor / Claude Code "project brain" UX pattern + multi-agent mesh distribution. | 2026 agentic-coding UX literature; E3 multi-agent pattern | Kars-specific; uses AGT mesh for distribution | 3 weeks; depends on R-602 |
| **R-607** Intake orchestrator agent (a kars sandbox itself) that selects a recipe from natural-language task description, confirms with user, creates `KarsTask` | The "I don't know which recipe to pick" UX. Eats our own dog food (the intake agent is governed by the same primitives as everyone else). | E1 Anthropic "clear ownership" gap; E3 all categories | Kars-specific; an Orka-style intake done with auditable CRDs instead of opaque orchestrator | 3 weeks; depends on R-601 + R-602 + R-604 |
| **R-608** Regret-free undo: per-action undoability annotation + agent-emitted explanation surface; auto-escalate Tier-4 tasks past 5 irreversible actions | What makes the autonomy tiers meaningful in practice. Without undoability, Tier-4-5 autonomous tasks have no escape valve. | 2026 agentic-coding UX patterns ("regret-free commits"); E1 HITL requirement | Kars-specific; built on top of existing audit + KarsSREAction substrates | 2 weeks; depends on R-201 + R-602 |

#### OUTCOME ⑦ — "Kars is a defensible long-term bet, not a Microsoft-Azure-only science project"

Targets the procurement / community-legitimacy gap. The E1 Forrester finding "platform fragmentation slows commitment" is partially about the *perception of project longevity*.

| Item | What unblocks | Evidence | Kars-or-upstream | Effort |
|---|---|---|---|---|
| **R-701** v1 API stability + deprecation policy + CRD conversion-webhook strategy (overlaps R-103) | Procurement / architecture-review boards need v1 to consider deployment. | E1 + procurement reality | Kars-specific | 6-8 weeks Q4 2026 |
| **R-702** CNCF Sandbox application (post-v1) | Independent ecosystem signal that kars is a community project, not a vendor-pinned tool. | E1 (need multi-vendor governance) | Kars + CNCF process | 4 weeks application + 1-3 months review |
| **R-703** Recruit ≥3 non-Microsoft contributors in 6 months | The single highest-signal community-legitimacy proof. Targets: AGT contributors at other orgs, agent-sandbox SIG contributors, banks / regulated-industry adopters. | E1 multi-vendor governance | Community work | ongoing |
| **R-704** Public benchmark publication (cold-start, router latency, mesh frame throughput, cost-per-call) | Closes the "how do I know it's actually fast / efficient?" evaluation gap. | E1 (ROI requirement); industry benchmarking norm | Kars-specific | 2 weeks |
| **R-705** Production-reference customers named publicly (2-3 logos minimum) | Removes the "no one runs this in production" objection. | E1 procurement reality | Negotiation, not engineering | requires customer development |

### 6.c Tier / quarter sequencing — the same items, scheduled

**Tier 1 — Q3 2026 (6-8 weeks)** — biggest unblocks-per-week ratio. Onboarding + tier-aware governance + ecosystem composition.

- **R-101** Single-command install (2-3 wk) — onboarding cliff is everything
- **R-102** Public docs site (5-7 days) — Job 1 of the 2026-06-15 plan
- **R-201** Autonomy Tier 1..5 schema (2 wk) — single highest-leverage item; closes Gartner-flagged governance failure mode
- **R-301** Native Anthropic + Bedrock providers (~2 wk) — breaks Azure-only perception
- **R-302** Bedrock Guardrails + OpenAI Moderation modules (~2 wk) — easiest two guardrails
- **R-501** `KarsKillSwitch` CRD (1 wk) — leans on AGT for the actual termination; we just provide the K8s surface
- **R-505** Stream AGT compliance proofs into K8s Events (1 wk) — same shape; small effort for compliance story
- **R-306** SIG-aligned hardened `podTemplate` snippet (1-2 wk) — first cut of SIG overlay-hardening; full `SandboxTemplate` lands in Tier 2

**Tier 2 — Q4 2026 (8-10 weeks)** — DX foundation + cost story + behavioural observability.

- **R-202** Per-tier default HITL gates (2-3 wk; depends R-201)
- **R-203** General HITL framework (2-3 wk; depends R-201)
- **R-301** Remaining LLM providers — Gemini / Vertex / Ollama / vLLM (~3 wk)
- **R-303** Per-key virtual keys + budget + cost tracking (2 wk)
- **R-304** Unified action-cost ledger (2 wk)
- **R-305** Grafana cost dashboard (3 days)
- **R-601** `KarsRecipe` CRD + standard catalog (3 wk)
- **R-602** `KarsTask` CRD + CLI + per-task-kind handover patterns (2 wk)
- **R-603** Kars-native conversation ingress (2 wk)
- **R-502** Behavioural baseline + drift detection (4-6 wk)
- **R-504** SLO-breach → AGT-quarantine integration (1-2 wk)
- **R-306** Ship kars-hardened `SandboxTemplate` upstream (2-3 wk)
- **R-204** Principled decommissioning lifecycle (2 wk)

**Tier 3 — Q1 2027** — procurement-grade legitimacy + the rest.

- **R-103 / R-701** v1 API stability + CNCF Sandbox application (6-8 wk)
- **R-604** Web UI: task feed + chat thread + recipe browser + sub-agent task tree (3 wk)
- **R-605** `KarsArtifact` CRD + typed artifact bundling (2 wk)
- **R-606** `KarsProject` cross-task project memory (3 wk)
- **R-607** Intake orchestrator agent (3 wk)
- **R-608** Regret-free undo (2 wk)
- **R-503** Delegation chain depth limit + tree (3-4 wk)
- **R-401** Mesh-aware QoS (2 wk)
- **R-402** Cross-runtime mesh second pair (MAF ↔ LangGraph) (2-3 wk)
- **R-403** Cross-org A2A federation blueprint (4 wk)
- **R-404** Tool poisoning detection (3-4 wk)
- **R-405** Sub-agent spawn governance hardening (3 wk)
- **R-704** Public benchmark publication (2 wk)
- **R-703 / R-705** Community + customer-reference work — ongoing

**Total Tier 1+2+3 engineer-weeks: ~80-100 weeks.** At 2 FTE: ~10-12 months calendar. At 3 FTE: ~7-8 months. Tier 1 alone (6-8 weeks at 2 FTE) is what makes kars credible at the enterprise-procurement stage; everything else compounds on top.

### 6.d What this roadmap is NOT

- It is not exhaustive — items added in response to specific customer requests get inserted on merit, not deferred.
- It is not a vendor wishlist — every item ties to evidence E1-E5 above. If a future feature request can't cite at least one, it doesn't ship (see §7.8 guardrail).
- It is not unique to kars — many items (R-101 install ergonomics, R-301 provider matrix) close gaps that any agent platform must close to be production-ready. Our differentiation is in §2, not in this roadmap. The roadmap closes table-stakes; §2 keeps the moat.

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
8. **We will NOT add features without a "what customer outcome does this unblock, what evidence says it matters" answer.** Every new CRD, controller, router module, or UX surface justifies itself against the seven irreducible advantages (§2), the alignment story (§4), one of the seven outcome buckets in §6.b, and one of the five evidence streams (E1-E5 in §6.a). If a proposal can't cite at least one of each, it doesn't ship.

9. **We will NOT duplicate AGT primitives that we can invoke.** Kill-switch, policy enforcement runtime, cryptographic compliance proofs, SLO-driven circuit breakers, framework adapters — AGT ships these. Kars provides the K8s-CRD-native invocation surface. Building parallel implementations would create maintenance drag and divergence risk.

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
