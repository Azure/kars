# Competitive positioning + leadership plan — June 2026

**Status:** internal strategy doc. Not for public publication. Drives the next 2–3 quarters of kars priorities.
**Authors:** Pal Lakatos, Copilot
**Date:** 2026-06-14
**Repo state at time of writing:** `Azure/kars` 6 stars, 8 contributors, 277 MB repo, ~98K LOC, 11 CRDs, 8 runtime adapters. Branch: `kars-sre/demo-and-agent`, commit `1dcc791`.

---

## TL;DR

There are three projects in adjacent territory to kars that the user we showcase to will compare us against:

1. **Orka** (`sozercan/orka`) — single-author, experimental, 4 months old, 7 stars. Wraps OpenAI/Anthropic with a task-orchestration model on top of K8s Jobs. Notable for **repository security scanning** as a flagship use case and the **OpenAI/Anthropic API-compatible front door** (lets `Continue`, `Cursor`, `Claude Code` "just work" against the cluster).
2. **Agentgateway** (`agentgateway.dev`, LF-hosted) — donated by Solo.io, **multi-vendor backed** (Microsoft, Dell, CoreWeave, T-Mobile, UBS, Akamai, Nirmata). Mature gateway data plane for HTTP/gRPC + LLM + MCP + A2A. 10+ LLM providers, 6+ guardrail integrations, virtual keys with per-key budgets, CEL-RBAC, MCP federation. **Production deployments cited at T-Mobile and UBS.**
3. **Kubernetes agent-sandbox SIG** (`kubernetes-sigs/agent-sandbox`) — Google-led with Anthropic + community. 52 merged + 41 open PRs in last 3 months. Owns the `Sandbox` workload-shape primitive. Roadmap includes portable backend, 1st-class router, multi-sandbox-per-pod, dynamic identity association, network-policy at claim time, framework integrations (LangChain/CrewAI/Ray/kAgent).

**Where kars sits uniquely today:**
- Only project in this set with **per-pod egress trust boundary** (iptables egress-guard + inference router; agent has no API keys).
- Only project with **E2E encrypted inter-agent messaging** (AgentMesh / Signal Protocol).
- Only project with **multi-runtime adapter framework** for 8 agent frameworks behind one trust boundary.
- Only project that **composes governance through 11 CRDs** with deterministic policy compilation and cosign-attested allowlists.

**Where kars is behind:**
- Provider coverage (we are Azure-heavy; agentgateway has 10+ providers).
- Guardrail integrations (we have Prompt Shields; agentgateway has Bedrock + Model Armor + OpenAI Moderation + regex + webhooks).
- OpenAI/Anthropic API-compatible shim (Orka has it; we don't — `Continue`/`Cursor`/`Claude Code` don't "just work" yet).
- Built-in UI (Orka embeds React in the controller binary; we require Headlamp install).
- Community: small star count, single-org backing (Microsoft Azure), no LF home, no v1 cadence yet.

**Leadership plan summary:** Don't try to out-feature agentgateway on gateway features (different deployment shape; we'd lose). Don't try to out-feature SIG on workload primitive (we're not the workload primitive; we should compose on top). Don't worry about Orka as a competitive threat (experimental, narrow scope) — but **steal the two genuinely good ideas (API-compatible shim, embedded UI) and the security-scanning use case as a kars-native agent**.

Instead: **double down on the four properties no one else has** (egress trust boundary, E2E inter-agent encryption, multi-runtime adapters, governance compose model) **AND close the credibility gaps** (provider matrix, guardrail integrations, community standing) so a serious enterprise evaluator can't dismiss us on the surface.

---

## Detailed comparison

### Methodology

Facts in this matrix are dated 2026-06-14 and cite their source. Where a project has multiple deployment shapes, the matrix records the *primary* shape. "✗" means the project does not have the capability today; "(plan)" means it's on the public roadmap; "✓" means shipped.

### Comparison matrix

| Capability | kars | Orka | Agentgateway | agent-sandbox SIG |
|---|---|---|---|---|
| **Maturity / community** | | | | |
| Stars (2026-06-14) | 6 | 7 | LF-hosted | SIG-hosted (Google) |
| Backers | Microsoft / Azure | 1 author + 2 contributors | Solo.io + MSFT + Dell + CoreWeave + T-Mobile + UBS + Akamai + Nirmata | Google + Anthropic + community |
| Production deployments cited | Internal MSFT teams | None (self-says experimental) | T-Mobile, UBS, Dell | Anthropic, Google internal |
| Cadence | active, daily | very active, 59 commits / 30d | active, mature releases | very active, 52 merged PRs / 3mo |
| **Deployment shape** | | | | |
| Trust boundary | Per-pod egress sidecar | Hardened pod, no sidecar | Cluster gateway (centralized) | Workload primitive only |
| Agent isolation | Namespace + iptables + NP + seccomp + readonly rootfs | non-root + readonly rootfs + dropped caps + seccomp | N/A (gateway, not workload) | gVisor / Kata RuntimeClass (operator's choice) |
| Multi-tenant safety | Strong (per-pod egress confinement) | Medium (hardened pod, no egress confinement) | Strong (gateway-level tenant isolation) | Depends on operator's PodSpec |
| **LLM providers** | | | | |
| Azure OpenAI / Foundry | ✓ native + IMDS auth | ✓ "AzureOpenAI" provider | ✓ Azure (OpenAI + Foundry) | N/A |
| OpenAI | ✓ | ✓ | ✓ | N/A |
| Anthropic | ✓ via runtime adapter | ✓ | ✓ | N/A |
| AWS Bedrock | ✗ | ✗ | ✓ + Bedrock Guardrails | N/A |
| Google Gemini / Vertex AI | ✗ | ✗ | ✓ | N/A |
| Ollama / vLLM (local) | ✗ | ✗ | ✓ both | N/A |
| **Token / cost controls** | | | | |
| Per-sandbox token budget | ✓ via `InferencePolicy` | ✓ via `RateLimit` | ✓ "budget limits" | N/A |
| Per-API-key virtual keys with budgets | ✗ | ✗ | ✓ | N/A |
| Cost tracking metrics | ✓ token counts via OTel | ✓ Prometheus | ✓ token + cost dashboards | N/A |
| **Guardrails / content safety** | | | | |
| Azure Prompt Shields | ✓ | ✗ | ✗ (via Azure proxy possible) | N/A |
| AWS Bedrock Guardrails | ✗ | ✗ | ✓ | N/A |
| Google Model Armor | ✗ | ✗ | ✓ | N/A |
| OpenAI Moderation | ✗ | ✗ | ✓ | N/A |
| Regex / PII filters | partial | ✗ | ✓ | N/A |
| Custom webhook | ✓ via ToolPolicy | ✗ | ✓ | N/A |
| Multi-layered chained guardrails | ✗ | ✗ | ✓ | N/A |
| **MCP** | | | | |
| MCP backend integration | ✓ `McpServer` CRD | ✓ tools as MCP-shaped | ✓ static + dynamic + virtual federation | (plan) MCP endpoint via router |
| MCP federation (virtual MCP) | ✗ | ✗ | ✓ | N/A |
| MCP auth (JWT, Keycloak, etc.) | basic | ServiceAccount tokens only | ✓ broad | N/A |
| MCP rate limiting | ✓ via ToolPolicy | ✗ | ✓ | N/A |
| **Inter-agent comms** | | | | |
| E2E encrypted (Signal Protocol) | ✓ AgentMesh (AGT) | ✗ | ✗ (A2A over TLS only) | ✗ |
| KNOCK / trust gating | ✓ | ✗ | ✗ | ✗ |
| Trust score progression | ✓ | ✗ | ✗ | ✗ |
| Cross-runtime mesh interop | ✓ Hermes ↔ OpenClaw verified | N/A (one runtime) | N/A (gateway only) | N/A |
| A2A ingress | ✓ via `A2AAgent` CRD | ✗ | ✓ A2A connectivity | ✗ |
| **Identity** | | | | |
| Workload Identity (Azure) | ✓ default | ✓ via secrets | ✓ supported | (plan) dynamic at claim time |
| Microsoft Entra Agent ID | ✓ via `KarsAuthConfig` | ✗ | ✗ | (plan) |
| ServiceAccount tokens | ✓ | ✓ | ✓ | ✓ |
| OIDC | ✓ via auth-sidecar | ✓ | ✓ | ✗ |
| Kontxt TxToken | ✗ | ✓ | ✗ | ✗ |
| Mesh DID (per-agent Ed25519) | ✓ | ✗ | ✗ | ✗ |
| **Agent frameworks** | | | | |
| Number of supported frameworks | 8 (OpenClaw, Hermes, Anthropic SDK, MAF, LangGraph py/ts, Pydantic AI, OpenAI Agents) | 1 (own framework) | N/A | (plan) LangChain, CrewAI, Ray, OpenEnv, kAgent |
| Adapter contract documented | ✓ `docs/runtimes/CONTRACT.md` | N/A | N/A | (plan) |
| CLI runtime delegation (Claude Code, Codex, Copilot CLI) | ✗ | ✓ as "Agent Runtimes" | N/A | N/A |
| **CRD surface** | | | | |
| Number of CRDs | 11 | 10 | 1 (`AgentgatewayPolicy`) + Gateway API | 4 (Sandbox, Template, Claim, WarmPool) + extensions |
| Cosign-attested policy bundles | ✓ | ✗ | ✗ | ✗ |
| Per-CRD reconciler isolation | ✓ kube-rs | ✓ controller-runtime Go | xDS control plane | controller-runtime Go |
| **Day-2 ops** | | | | |
| Operator UI | Headlamp plugin | Built-in React (embedded in controller) | Helm + xDS dashboards (no first-party UI yet) | (plan) lightweight OSS UI |
| Grafana dashboards | ✓ shipped | ✓ Prometheus + structured logs | ✓ shipped | (plan) controller custom metrics |
| Autonomous SRE agent | ✓ KarsSREAction + watcher | ✗ | ✗ | ✗ |
| OpenAI / Anthropic API-compatible front door | ✗ | ✓ `/openai/v1/chat/completions` + `/anthropic/v1/messages` | ✓ | ✗ |
| Repository security scanning agent | ✗ | ✓ flagship use case | ✗ | ✗ |
| Auto suspend / resume (state-preserving) | partial (`spec.suspended` scales to 0) | ✗ | ✗ | ✓ KEP-694 shipped, KEP-968 auto in progress |
| Warm pool of pre-provisioned sandboxes | ✗ | ✗ | N/A | ✓ `SandboxWarmPool` |
| **Standards alignment** | | | | |
| Kubernetes Gateway API | partial (a2a-gateway uses it) | ✗ | ✓ first-class | (plan) ingress |
| KEP-753 sidecar containers | ✓ uses native sidecar pattern | N/A | N/A | ✓ |
| agent-sandbox SIG overlay mode | ✓ `upstreamCompatibility.sigsAgentSandbox=overlay` | ✗ | ✗ | (source of truth) |
| trusted-init-containers VAP annotation (PR #854) | ready to consume when merged | ✗ | ✗ | (proposed PR) |
| **Threat model rigor** | | | | |
| Per-action security audit docs | ✓ `docs/internal/security-audits/` | ✗ visible | ✓ Solo.io maintains | ✓ via SIG security |
| Egress confinement enforcement | ✓ iptables-based | ✗ | N/A (gateway is upstream) | ✗ (operator's responsibility) |
| Confidential VM support | ✓ `spec.sandbox.isolation: confidential` | ✗ | N/A | ✓ via RuntimeClass |
| **OSS legitimacy** | | | | |
| License | MIT | MIT | Apache-2.0 | Apache-2.0 |
| LF-hosted | ✗ | ✗ | ✓ | ✗ (SIG hosted by K8s) |
| Multi-vendor governance | ✗ (Microsoft) | ✗ (1 author) | ✓ | ✓ |

### Headline reading of the matrix

- **Agentgateway dominates the "central gateway / many backends" category.** Their LF backing + 10+ providers + 6+ guardrails + production deployments at T-Mobile/UBS make them the de facto choice for "I have many model deployments and I need a smart gateway in front". Trying to beat them on gateway feature surface is a losing battle and the wrong fight; we'd be reduced to a worse gateway than the LF-hosted one.

- **Agent-sandbox SIG dominates the "workload-shape primitive" category.** Google + Anthropic backing + 52 PRs merged in 3 months + on roadmap to be the substrate for LangChain / CrewAI / Ray / kAgent. We're not the workload primitive and shouldn't try to be — we should compose on top.

- **Orka is not a serious competitor today** (single author, 7 stars, self-says experimental) but is interesting as a **design study**: it solves "Continue/Cursor/Claude Code see my cluster as an OpenAI/Anthropic endpoint" elegantly, and it productionizes repository security scanning as a CRD-driven workflow. Both ideas are worth stealing.

- **Kars's defensible territory is the trust-boundary + multi-runtime + mesh combination**, which none of the others touch. The brief is "if you're running multiple agent frameworks from multiple teams against a shared LLM fleet, with auditable governance + airgap/sovereign requirements + per-agent E2E messaging + per-team isolation, kars is the answer". If the customer doesn't need that combination, they should pick one of the others.

---

## Per-project deeper analysis

### Orka

**What it actually is** (from `github.com/sozercan/orka`, README, code inspection):
- Go, MIT license, created 2026-02-05, 3 contributors (sozercan / Sertaç Özercan is the main author, well-known MSFT K8s engineer).
- 27.5 MB repo, 59 commits in last 30 days, 7 stars, 4 forks, 20 open issues.
- 10 CRDs: `Agent`, `AgentRuntime`, `Execution`, `Provider`, `RepositoryMonitor`, `RepositoryScan`, `Skill`, `SubstrateActorPool`, `Task`, `Tool`.
- Internal packages reveal scope: `admission/task_provenance`, `security` (with stages: threat-model, mapper, review, validation, patch), `llm` (openai + anthropic + cooldown + fallback + retry), `redact`, `contexttoken`, `controller`, `store`, `taskmeta`, `tools`, `tracing`, `uiembed`, `worker`, `workerenv`, `workspace`, `substratepb`.

**What it does that we don't**:
1. **OpenAI-compatible (`/openai/v1/chat/completions`) and Anthropic-compatible (`/anthropic/v1/messages`) front door.** Existing dev tools (`Continue`, `Cursor`, `Claude Code`) point at the cluster and "just work". The keys live in K8s Secrets; the developer never holds them. Eliminates a huge UX gap.
2. **Repository security scanning as a CRD-driven workflow** (`RepositoryScan` + `RepositoryMonitor`). Scheduled + incremental repo scans with threat model, validated findings (`ValidationArtifact`), patch generation, remediation PRs. Genuinely productionized agentic-security niche.
3. **CLI runtime delegation** (`AgentCLIRuntime` with types for Claude Code CLI, Codex CLI, Copilot CLI). Tasks delegate to external CLI tools that already know how to drive a codebase. Smart pattern.
4. **Embedded React UI in the controller binary** (`internal/uiembed`). One Deployment, dashboard included. Zero install friction.

**What it doesn't do**:
- No egress trust boundary (the agent code can call any external endpoint the pod can reach).
- No inter-agent encrypted mesh (REST coordination only).
- No multi-runtime (own framework only; CLI delegation is a different shape).
- Narrow LLM provider matrix (OpenAI, Anthropic — not Foundry, not Bedrock, not Gemini, not local).
- No governance composition CRDs (no equivalent of `InferencePolicy`, `ToolPolicy`, `EgressApproval`, `KarsMemory`, `TrustGraph`).
- No A2A, no MCP federation, no cosign attestation.
- Self-says experimental, "not yet recommended for production".

**Threat assessment:** Low today (small, narrow, experimental). Worth tracking as a design study; not worth competitive countermeasures. **Steal the API-compatible shim and the embedded UI; build a kars-native repository security scanning use case.**

### Agentgateway (Solo.io → Linux Foundation)

**What it actually is** (from `agentgateway.dev/docs`, LF announcement, Solo.io blog):
- LF-hosted as of 2026, donated by Solo.io.
- HTTP + gRPC + LLM + MCP + A2A data plane. Designed as a centralized gateway, not a per-pod sidecar.
- Kubernetes Gateway API based + `AgentgatewayPolicy` CRD for policy targeting/merging/conditional rules.
- 10+ LLM providers: Amazon Bedrock, Anthropic, Azure (OpenAI + Foundry), Gemini, OpenAI, OpenAI-compatible, Vertex AI, Ollama, vLLM, multiple-endpoints, mock httpbun.
- Guardrails: regex/PII, OpenAI Moderation, AWS Bedrock Guardrails, Google Model Armor, multi-layered chain, custom webhook API.
- LLM features: model aliasing, API keys, virtual keys (per-key token budgets + cost tracking), load balancing (P2C), model failover with outlier detection, content-based routing, OpenAI Realtime, function calling, prompt enrichment/templates, request transformations, budget+spend limits, rate limiting, cost tracking, CEL-based RBAC.
- MCP features: static / dynamic / virtual federation, HTTPS, JWT auth, tool access RBAC, rate limiting, stateful sessions, multiple auth providers (Keycloak documented).
- Listeners: HTTP, HTTPS, mTLS (FrontendTLS), TCP, advanced TLS settings.
- Backed by: Microsoft, Dell, CoreWeave, T-Mobile, UBS, Akamai, Nirmata (Kyverno), NYU (TUF).

**What it does that we don't**:
1. **10+ LLM providers** (we are heavily Azure OpenAI / Foundry).
2. **6 guardrail integrations** (we have Prompt Shields only).
3. **Virtual keys with per-key token budgets + cost tracking** (we have per-sandbox budgets only).
4. **MCP federation** (multiple backend MCPs exposed as one virtual MCP).
5. **CEL-based RBAC** for AI route auth (we have rigid ToolPolicy schemas).
6. **OpenAI Realtime API support** (voice + bidirectional streaming).
7. **Gateway API first-class alignment** (our `a2a-gateway` is partial).
8. **xDS control plane** scalable to large data planes.

**What it doesn't do**:
- No per-pod sandbox trust boundary. The agent (= gateway client) holds API keys to call the gateway — the "agent has no upstream credentials" property doesn't hold.
- No E2E encrypted inter-agent messaging. A2A is TLS-only.
- No agent workload management. Doesn't run sandboxes; doesn't compose with Pod-level isolation primitives.
- No multi-runtime framework adapters. The agent is upstream of the gateway; gateway doesn't know about Hermes / OpenClaw / MAF.
- Designed for gateway operators, not sandbox operators.

**Threat assessment:** High — they will dominate the gateway category. But they don't directly compete with kars's positioning. **We should integrate with agentgateway as a backend** (agentgateway in front, kars sandboxes behind, agent traffic flows through both) rather than try to out-feature them. **And steal the broader provider matrix and guardrail integrations into the kars router** so kars is not Azure-locked.

### Kubernetes agent-sandbox SIG

**What it actually is** (from `kubernetes-sigs/agent-sandbox`, roadmap.md, KEPs, recent PRs):
- SIG Apps subproject. Apache-2.0. `apiVersion: agents.x-k8s.io/v1beta1`.
- 4 CRDs: `Sandbox` (core), `SandboxTemplate`, `SandboxClaim`, `SandboxWarmPool`.
- `SandboxSpec` is intentionally narrow: `podTemplate`, `volumeClaimTemplates`, `lifecycle`, `operatingMode`, `service`.
- v1beta1 migration in progress with two-way conversion webhook (PRs #962, #966, #955, #971 merged).
- Active: 52 merged + 41 open PRs in last 3 months. Google-led (justinsb, vicentefb, moficodes), Anthropic + community contributors.

**Roadmap headlines (2026)**:
- **Decouple API from Runtime (Portable Backend)** — KEPs #597, #747 — common proto runtime backend. Status: in progress.
- **1st Class Router** — Go-based, ships with project. Status: planned.
- **Auto Suspend/Resume** — KEP-968 (PRs #970, #972). Status: planned.
- **Multi-Sandbox per Pod** — extend API for N sandboxes per Pod. Status: planned.
- **Sandbox/Pod Identity Association** — dynamic identity at claim time. Status: planned.
- **NetworkPolicy attach at claim time** — Status: planned.
- **Integration with Ray (Rllib)** — Status: in progress.
- **Integration with LangChain, CrewAI, OpenEnv, kAgent** — Status: in progress.
- **MCP server endpoint via router or SDK** — Status: planned.
- **UI in OSS** — lightweight OSS dashboard. Status: planned.

**Relevant open PRs** (kars alignment touchpoints):
- **#854** — `agents.x-k8s.io/trusted-init-containers` annotation on `secure-sandbox-policy` VAP. Author explicitly cites "mesh sidecar iptables init container" — exactly our egress-guard. **Direct enabler** for kars overlay-mode hardening.
- **#967** — Cilium egress example on GKE Dataplane v2 (NetworkPolicy + FQDNNetworkPolicy + Squid + VAP). Alternative to our iptables-based egress-guard for Cilium environments.
- **#850** — Envoy + ext_proc data-plane RFC (Draft) for the SIG sandbox-router. If adopted, kars governance hooks could be ext_proc filters.
- **#838 / #923** — Go sandbox-router (cluster-singleton ingress proxy). **Name collision** with our inference-router; different role.
- **#956, #903** — portable backend gRPC proto (KEP #597, #747 implementation).

**What it does that we don't**:
1. Warm pool of pre-provisioned sandboxes (sub-second claim latency target).
2. PVC-based suspend/resume.
3. TypeScript + Python + Go SDKs (we have a TypeScript CLI but no agent-side SDKs).
4. Gateway API alignment for ingress.
5. Multi-vendor (Google + Anthropic + community).

**What it doesn't do** (per current shipped state):
- No governance plane (operator brings their own).
- No inter-agent communication (each Sandbox is independent).
- No multi-runtime adapter framework.
- No trust boundary inside the Pod (operator's responsibility).
- No mesh secrecy.

**Threat assessment:** Not a direct competitor — they're solving the *workload primitive* problem. **They are a critical alignment target.** If the SIG becomes the de facto K8s sandbox primitive, kars must be cleanly composable on top, ideally with a kars-shipped `SandboxTemplate` and contributions to `trusted-init-containers` so the egress-guard pattern is sanctioned upstream.

---

## What kars must do to be the leader

### Five principles

1. **Don't compete where we lose; compose where we can win.** Don't try to out-gateway agentgateway. Don't try to out-workload-primitive the SIG. Compose on top of both.
2. **Double down on the four properties no one else has** (trust boundary, mesh, multi-runtime, governance compose model). These are the moat.
3. **Close the credibility gaps that block serious evaluators** (provider matrix, guardrails, API-compatible shim, embedded UI, OSS legitimacy).
4. **Steal good ideas from Orka.** API-compatible front door, embedded UI, repo-scanning agent. All three are achievable as slices.
5. **Be loudly Microsoft-native AND broadly multi-cloud.** Foundry / Entra Agent ID are differentiators in MSFT shops; Bedrock / Gemini / Vertex must work for everyone else. Don't pick one.

### Concrete leadership work items, by theme

#### Theme 1 — Expand the router's provider + guardrail matrix
- **R1.** Add native Anthropic provider (no runtime adapter required) to the inference-router.
- **R2.** Add native Google Gemini / Vertex AI provider to the inference-router.
- **R3.** Add native AWS Bedrock provider to the inference-router.
- **R4.** Add Ollama / vLLM local-model provider support.
- **R5.** Wire AWS Bedrock Guardrails as a content-safety module in the router.
- **R6.** Wire Google Model Armor as a content-safety module.
- **R7.** Wire OpenAI Moderation as a content-safety module.
- **R8.** Add multi-layered guardrail chaining in `InferencePolicy.contentSafety` (currently single Prompt Shields).
- **R9.** Add regex / PII detector primitives in `ToolPolicy.argValidation`.

#### Theme 2 — API-compatible front door
- **F1.** Add `/openai/v1/chat/completions` endpoint on a new `kars-api-gateway` or extend `a2a-gateway` so `Continue`, `Cursor`, OpenAI-compatible clients hit the cluster directly. Auth via ServiceAccount tokens.
- **F2.** Add `/anthropic/v1/messages` for `Claude Code`.
- **F3.** Document the dev-tool integration recipe end-to-end (Continue config, Cursor settings, Claude Code config).

#### Theme 3 — Per-key virtual budgets + cost tracking
- **V1.** Extend `InferencePolicy` with per-API-key virtual-key budgets (cap per-key, track per-key cost).
- **V2.** Cost dashboard in Grafana with per-key breakdown.
- **V3.** Per-key rate-limit module in the router.

#### Theme 4 — agent-sandbox SIG alignment
- **S1.** Ship the documented hardened `podTemplate` snippet for overlay mode (`docs/runbooks/overlay-mode.md`).
- **S2.** Ship a kars-hardened `SandboxTemplate` using the SIG's own primitive. Users `SandboxClaim` from it. **Most important integration win.**
- **S3.** Track PR #854 (`trusted-init-containers`); add the annotation to our egress-guard init container as soon as it merges.
- **S4.** Open an issue on `kubernetes-sigs/agent-sandbox` proposing kars as a *governance overlay reference implementation*; offer to contribute an `examples/kars-governance/` directory.
- **S5.** Track PR #850 (Envoy + ext_proc RFC); if adopted, prototype kars governance hooks as ext_proc filters.
- **S6.** Watch the Portable Backend KEPs (#597, #747); evaluate whether kars sandbox shape could be implementable as a backend.

#### Theme 5 — Steal the security-scanning use case
- **SEC1.** Build a kars-native `KarsRepoScan` CRD modeled on our existing `KarsSREAction` pattern. The repo-scan agent uses the SRE pattern (typed actions + human approval + bounded-CRB).
- **SEC2.** Threat-model, validation, patch-generation stages matching Orka's `RepositoryScan` workflow shape, but with kars's audit-trail + AGT governance + mesh-distributed validation across multiple specialist agents.
- **SEC3.** Demo against a public repo (e.g. our own) at next showcase.

#### Theme 6 — Embedded UI / one-deploy friction
- **U1.** Embed the React Headlamp plugin bundle in the controller binary OR ship a `kars-ui` Deployment in the chart that serves the dashboard standalone (so users get a dashboard without installing Headlamp). Use Headlamp plugin path for Headlamp users; standalone path for non-Headlamp users.
- **U2.** "kars up" should print the dashboard URL with a single port-forward command.

#### Theme 7 — MCP federation + advanced policies
- **M1.** Extend `McpServer` CRD with federation: one logical `McpServer` exposing N backend MCP servers (the "Virtual MCP" pattern).
- **M2.** Wire CEL-based RBAC on routes: `ToolPolicy` rules expressible in CEL, evaluated per request.
- **M3.** OpenAI Realtime API support in the router (voice / bidi streaming) — Foundry first.

#### Theme 8 — OSS legitimacy + community
- **C1.** Open a CNCF Sandbox application proposal (post-v1 readiness).
- **C2.** Establish a public design-doc cadence at `docs/design/`; first 3 design docs to publish: AgentMesh wire format, KarsSandbox v1beta1 schema rationale, SRE action lifecycle.
- **C3.** Recruit at least 3 non-Microsoft contributors in next 6 months. Identify likely targets via the AGT and agent-sandbox SIG contributors lists.
- **C4.** v1 release with API stability commitment.
- **C5.** Sample integration demos with Anthropic Managed Agents and Google's Anthropic on GKE (per agent-sandbox SIG PR #950).

#### Theme 9 — Tighten the unique-value blog content
- **B1.** Lead blog post (this one) now positions kars correctly. Keep updating as the landscape moves.
- **B2.** Publish a separate "kars vs agentgateway: when to use which" post.
- **B3.** Publish a separate "kars on top of agent-sandbox SIG: overlay mode walk-through" post.
- **B4.** Publish a separate "running OpenAI Agents SDK + LangGraph + Hermes in one cluster behind one trust boundary" post (the multi-runtime story).

### Sequencing recommendation (next 2 quarters)

**Q3 2026 (priority)**:
- Theme 2 (API-compatible front door) — biggest UX gap, low complexity.
- Theme 4 (SIG alignment S1, S2, S3) — lands as upstream PR #854 lands.
- Theme 1 (router providers R1, R2, R5) — Bedrock + Gemini + Bedrock Guardrails first.
- Theme 6 (embedded UI U1) — one-Deployment friction reduction.

**Q4 2026**:
- Theme 5 (security scanning) — capitalize on demo momentum.
- Theme 3 (virtual keys) — matches agentgateway capability.
- Theme 8 (CNCF Sandbox application, v1 release).
- Theme 7 (MCP federation M1, CEL RBAC M2).

**Through 2027**:
- Theme 4 (S4, S5, S6) — deeper SIG contribution.
- Theme 1 R3, R4, R6, R7 (more providers, more guardrails).
- Theme 8 C3 (non-MSFT contributors).
- Theme 9 (continuing blog cadence).

### Risks

- **agentgateway picks up "per-pod data plane" as an architecture.** Solo.io has the engineering capacity; if they ship a sidecar mode of agentgateway with the same provider + guardrail matrix, our trust-boundary differentiation narrows. **Mitigation:** ship the four-property combination (mesh + multi-runtime + governance compose + trust boundary) faster than they can replicate; deepen mesh and multi-runtime where they have no expertise.
- **SIG sandbox-router becomes "the kars router."** If the upstream Go sandbox-router (PRs #838, #923) gets popular and adds semantic features, our inference-router could look duplicative. **Mitigation:** disambiguate the role explicitly in docs; contribute to the upstream router; offer the kars router as a per-pod *sidecar* (different from upstream's cluster-singleton ingress role).
- **Orka or a similar small project gets acquired / endorsed.** Sertaç is at MSFT; if Orka becomes "MSFT's official agent runtime", the org could push it over kars. **Mitigation:** be the production-ready, security-first option already running in MSFT teams; make the technical case for kars's deeper isolation primitives; collaborate where possible (Orka could be a kars runtime adapter).
- **Foundry-native positioning is too narrow** as the industry standardizes on more vendors. **Mitigation:** Theme 1 (broader providers) is the answer.

---

## Appendix — sources

- `github.com/sozercan/orka` (README, /api/v1alpha1/, /internal/security/, /internal/llm/, repo stats via GitHub API, accessed 2026-06-14).
- `agentgateway.dev/docs/about/`, `agentgateway.dev/docs/llms.txt` (project documentation index, providers, guardrails, MCP features, policies, accessed 2026-06-14).
- LF announcement: `linuxfoundation.org/press/linux-foundation-welcomes-agentgateway-project-to-accelerate-ai-agent-adoption-while-maintaining-security-observability-and-governance`.
- `github.com/kubernetes-sigs/agent-sandbox` (README, /api/v1beta1/sandbox_types.go, /docs/keps/, /roadmap.md, accessed 2026-06-14).
- `github.com/kubernetes-sigs/agent-sandbox/pulls` (100 PRs since 2026-03-01, breakdown: 41 open / 52 merged / 7 closed).
- Specific PRs cited: #850 (Envoy + ext_proc RFC), #854 (trusted-init-containers VAP), #967 (Cilium egress on GKE), #838/#923 (sandbox-router Go + WebSocket), #970/#972 (KEP-968 auto-suspend), #956/#903 (portable backend), #597/#747 (Portable Backend KEPs).
- Kars internal: `controller/src/crd.rs`, `controller/src/reconciler/mod.rs`, `deploy/helm/kars/templates/crd-*.yaml`, `runtimes/`, `sandbox-images/`, `inference-router/src/providers/`. State at `kars-sre/demo-and-agent@1dcc791`.
