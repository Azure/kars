# agentgateway vs. kars inference router — detailed architectural analysis

**Date:** 2026-06-15
**Status:** internal analysis. Settles a recurring question: what exactly is the difference between agentgateway and the kars inference router, and where do they overlap, complement, or compete?
**Sources cited inline**, all verified 2026-06-15.

---

## TL;DR

They are **two different things that sound alike** because both use the word "gateway" in the agent / LLM / MCP context.

- **agentgateway** = a **cluster-edge centralized data-plane proxy** built on the Kubernetes Gateway API. It sits *at the edge* of your cluster (one HA deployment per cluster, addressable via a LoadBalancer Service). It is configured via xDS or YAML + watch. It is **caller-agnostic**: anyone holding a credential can call it. It does not know what a kars sandbox is.
- **kars inference router** = a **per-pod egress sidecar** that lives *in every sandbox pod*. It sits in front of the agent's network egress only because iptables egress-guard redirects the agent's traffic to it. It is **caller-specific**: there is exactly one caller per router instance (the sibling agent), and the router holds upstream credentials the agent's UID 1000 cannot read.

They overlap in some surfaces (LLM provider integration, MCP routing, A2A gateway-style traffic) but solve different problems: agentgateway makes one shared proxy *better than callers calling models directly*; the kars router makes the agent's call **the only path out of a pod whose user-space is assumed adversarial**.

The right pattern in many systems is to **run both** — agentgateway as the cluster-edge LLM and MCP gateway, kars router as the per-pod trust boundary inside each agent. Agentgateway is not a substitute for the kars router and vice-versa.

The "ingress for developers to talk to running agents" use case discussed in the [dev-experience design note](dev-experience-design-note.md) is **not something agentgateway does today** (confirmed by source-code grep — no WebSocket upgrade handling, no per-sandbox session routing). It is a kars-shaped capability.

---

## Establishing the facts

### Agentgateway

| Attribute | Value | Source |
|---|---|---|
| Repo | github.com/agentgateway/agentgateway | repo |
| Stars / forks / contributors | 3,283 / 537 / 30 | GitHub API 2026-06-15 |
| Created | 2025-03-18 | GitHub API |
| Last commit | 2026-06-12 | GitHub API |
| Activity | 100+ commits in last 30 days | GitHub API |
| Language | Rust | repo |
| License | Apache-2.0 | repo |
| Governance | LF-hosted; TSC; charter at `CHARTER.md` | CHARTER.md (LF Projects, LLC) |
| Deployment shape | One LB-fronted Gateway deployment per cluster (Gateway API `GatewayClass: agentgateway`) | docs `setup/gateway.md` |
| Configuration | Static + Local (file watch) + xDS (purpose-built, not Envoy) | `architecture/configuration.md` |
| Data-plane container | `agent-gateway` (single container per Pod) | docs `setup/gateway.md` |
| Mission | "create a secure, scalable, and standardized foundation for AI agents to discover, communicate with, and leverage external tools and services" | `CHARTER.md` §1.a |
| Primary protocols | HTTP / gRPC / WebSocket (parse/websocket.rs exists) / TCP / TLS / mTLS / A2A / MCP (stdio + HTTP + SSE + Streamable HTTP) | `crates/agentgateway/src/parse/` + docs |
| LLM providers | OpenAI, Anthropic, Azure (OpenAI + Foundry), Bedrock, Vertex AI, Gemini, Ollama, vLLM, OpenAI-compatible | docs `/llm/providers/` |
| Guardrails | Regex/PII, OpenAI Moderation, AWS Bedrock Guardrails, Google Model Armor, custom webhook, multi-layer chain | docs `/llm/guardrails/` |
| MCP capabilities | Static, dynamic (label selector), virtual (federation), HTTPS, JWT auth, tool-level access RBAC, rate limit, stateful sessions, multi auth providers (Keycloak documented) | docs `/mcp/` |
| Auth | JWT, API keys, OAuth, basic auth, mTLS, OIDC | `crates/agentgateway/src/http/` (apikey.rs, basicauth.rs, auth/) + docs |
| Policy engine | CEL (their own `cel-fork` and `celx` crates) | `crates/cel-fork/` |
| Observability | OpenTelemetry metrics + logs + tracing | README |
| Inference Routing | Kubernetes Inference Gateway extensions (InferencePool, InferenceObjective) | README + docs |
| Backers | Solo.io (origin), Microsoft, Dell, CoreWeave, T-Mobile, UBS, Akamai, Nirmata (Kyverno) | agentgateway.dev landing-page quotes |

**What the architecture document says** (`architecture/configuration.md`):

> The local configuration uses a file watch to dynamically reload changes. The local configuration will translate into a shared (with XDS) internal representation (IR) that is used by the proxy at runtime. ... XDS configuration allows the proxy to be configured by a remote control plane. We use the XDS Transport Protocol, but do not use the Envoy types (Listener, Cluster, etc), and instead use purpose-built types.

The data plane is an Envoy-shaped proxy with kars-shaped extensions. The control plane is xDS with the convention that resources point up to their parent (route → listener) rather than parent containing list of children, to avoid Envoy's fanout problem.

**Top-level resources**:
- `Bind` (`port/namespace/name`) — port + namespace binding
- `Port` — TCP/UDP port to listen on
- `Listener` — TLS termination, hostname, protocol, accepts traffic
- `Route` — match + filter + backend dispatch
- `Backend` — destination (LLM provider config / static address / virtual MCP / A2A)
- `Target` — backend details (e.g., MCP tools)
- `Policies` — Request/Response Header Modifier, Redirect, URL Rewrite, Mirror, CORS, A2A, Backend Auth, Timeout, Retry

This is the surface of a service-mesh-style proxy with AI-aware backend types.

### Kars inference router

| Attribute | Value | Source |
|---|---|---|
| Lives where | One sidecar container in every `KarsSandbox` pod | `controller/src/reconciler/mod.rs` deployment builder |
| Container name | `inference-router` | controller deployment template |
| Listening on | `0.0.0.0:8443` (router process) | `inference-router/src/main.rs:418-440` |
| How agent reaches it | iptables egress-guard init container redirects UID 1000's egress to the router; agent's HTTP client thinks it's calling `api.openai.com` and lands at the router | `controller/src/reconciler/mod.rs` egress_guard_init_command |
| Trust direction | Agent (UID 1000) is **adversarial**. Router (UID 1001) holds upstream credentials. Agent cannot read router's process memory. | by design, four-layer model |
| Surfaces | Model calls (chat completions, embeddings), MCP backend calls, mesh ingress + egress (AGT relay), memory store ops, sub-agent spawn validation, A2A egress, governance hooks (Prompt Shields, AGT policy), Foundry data-plane proxy | `inference-router/src/routes/` (~20 modules per `ls`) |
| Configuration | Compiled policy bundle mounted at `/etc/kars/` as ConfigMap, hot-reloaded on change. CRDs upstream: `InferencePolicy`, `ToolPolicy`, `KarsMemory`, `EgressApproval`, `McpServer`, `TrustGraph` projection | `controller/src/reconciler/governance_mounts.rs` |
| Auth held | Workload Identity / IMDS-exchanged tokens, MCP server credentials, channel tokens (Telegram, Slack), Foundry SDK creds | `inference-router/src/auth.rs` |
| Identity mode | Workload Identity (default) OR Microsoft Entra Agent ID via per-pod auth sidecar (`KarsAuthConfig` + sidecar; fails closed, no WI fallback) | `inference-router/src/auth.rs:29-35,75-82,101-113,120-140` |
| Observability | OpenTelemetry GenAI semantic-convention spans, Prometheus metrics | `inference-router/src/metrics.rs` |
| Lifecycle | Bound to the agent's lifecycle (pod start/stop); one router per sandbox | controller deployment |
| Direction | Primarily **egress** (agent → upstream) | by design |

**What the router actually does on a call** (verified against route modules):

1. Receive HTTP request from the agent process (lands here because iptables sent it here, not because the agent chose to use a proxy).
2. Apply route-appropriate policy module (InferencePolicy for chat/embeddings, ToolPolicy for tool calls, AGT governance for mesh, etc.).
3. Mint upstream credential just-in-time (IMDS / Workload Identity / Agent ID sidecar).
4. Forward upstream.
5. Apply outbound policy (content safety on the response, token-budget decrement, telemetry).
6. Return to the agent.

---

## Side-by-side architectural comparison

| Dimension | agentgateway | kars inference router |
|---|---|---|
| **Deployment topology** | Cluster-edge (one HA Deployment, LB Service) | Per-pod sidecar (one container in every sandbox pod) |
| **Cardinality** | Few-of-many: 1 proxy serves N callers and N backends | One-of-one: 1 router serves 1 caller (the sibling agent) |
| **Caller relationship** | Caller-agnostic. Anyone with a credential can call. | Caller-specific. The agent has no alternative. Iptables enforces this. |
| **Credential ownership** | Holds backend credentials at the gateway. Callers do NOT have backend credentials. | Holds backend credentials at the router. Agent does NOT have backend credentials, AND agent's UID 1000 cannot read the router's process memory. |
| **Trust assumption about callers** | Callers are trusted clients (or untrusted but authenticated). Standard gateway-style threat model. | Caller (the agent) is **adversarial**. Threat model assumes the agent's user-space is compromised and the trust boundary must hold anyway. |
| **Authorization model** | CEL-based RBAC + JWT + API key + OAuth + Basic + mTLS | Policy CRDs compiled to deterministic bundles + cosign-attested allowlists + AGT governance + per-sandbox compiled policy |
| **Configuration source** | xDS or local YAML + watch | K8s CRDs → controller compiles → per-sandbox ConfigMap → router hot-reload |
| **Configuration grain** | Per-Gateway (cluster) | Per-sandbox |
| **Routing primitive** | HTTPRoute / GRPCRoute / TCPRoute / TLSRoute → Backend | InferencePolicy.upstream + ToolPolicy.tools + McpServer.spec + EgressApproval.hosts + AGT mesh relay endpoint, all per-sandbox |
| **Stateful sessions** | MCP stateful session routing supported. WebSocket: `parse/websocket.rs` parses but no per-sandbox session affinity that we found. | Mesh-side: full Signal Protocol session state per peer-pair (X3DH, Double Ratchet, KNOCK). Model-call side: stateless. |
| **Sub-agent spawn semantics** | None (gateway doesn't know about agent lifecycle) | Validates spawn target, mints federated identity, propagates audit context, KarsSandbox CR creation gated by `spawn_policy` |
| **Mesh-aware features** | None | KNOCK gate, trust scores, per-peer rate limits, cross-runtime Signal interop |
| **MCP federation** | Yes (virtual MCP: one gateway exposes N backend MCPs) | Schema-only today (`McpServer` CRD is singular per binding) |
| **Egress confinement / iptables** | N/A — gateway is upstream of callers | First-class: init container locks UID 1000 to loopback + DNS; only path out is the router |
| **Confidential VM isolation** | N/A | `spec.sandbox.isolation: confidential` — terminates trust boundary at the pod (AMD SEV-SNP / Intel TDX) |
| **Observability** | OTel metrics/logs/tracing, AI-shape dashboards | OTel GenAI semantic-convention spans, Prometheus, per-sandbox audit |
| **Where the policy CRDs live in the cluster** | Cluster-wide (Gateway API + `AgentgatewayPolicy`) | Mostly namespaced (kars-system), some cluster-scoped (`KarsAuthConfig`, `TrustGraph`) |
| **Failure mode if it dies** | All cluster AI traffic stops until it's restarted (HA mitigates) | One agent's egress stops; other agents continue |
| **Update cadence** | Cluster-wide rollout (Helm upgrade / xDS push) | Per-sandbox (rolling restart of one Deployment) |

---

## What overlaps, what doesn't

### Overlaps (where the two genuinely cover the same surface)

1. **LLM provider integration.** Both can call OpenAI / Anthropic / Azure / Bedrock / Gemini / Vertex / Ollama / vLLM. Agentgateway's matrix is broader today (parity work tracked in [agentgateway-parity-plan.md](agentgateway-parity-plan.md)).
2. **MCP routing.** Both proxy to MCP backends and apply auth / rate limit / tool RBAC. Agentgateway has federation today; we have schema scaffolded.
3. **A2A traffic.** Both can carry A2A traffic. Different angle: agentgateway is the A2A *infrastructure* (route the bytes); kars `a2a-gateway` is the A2A *ingress endpoint* that maps an incoming A2A task to a specific destination sandbox + applies kars policy.
4. **Content safety.** Both can call out to guardrail modules (OpenAI Moderation, Bedrock Guardrails, Model Armor, custom webhooks). Agentgateway's coverage is broader.
5. **Observability.** Both emit OTel. Agentgateway emits at the gateway; kars emits per-sandbox.

### Doesn't overlap — agentgateway has, kars doesn't (today)

- Cluster-wide xDS control plane scalable to thousands of routes.
- Virtual MCP federation (one logical MCP exposing N backends).
- LLM virtual keys with per-key budgets + cost tracking (vs our per-sandbox).
- 10+ LLM provider matrix.
- 6 guardrail integrations.
- CEL-based authorization rules.
- OpenAI Realtime API (voice + bidi).
- Inference Gateway extensions (InferencePool / InferenceObjective).

### Doesn't overlap — kars has, agentgateway doesn't (and structurally can't, at their layer)

- **Per-pod egress trust boundary** with iptables-enforced confinement of UID 1000. Agentgateway is a gateway; the caller is upstream of it; iptables-style confinement makes no sense at the gateway layer.
- **Agent has no upstream credential at all.** With agentgateway, the agent (= caller) still has the API key to the gateway. Kars's router design holds the credential entirely outside the agent's process.
- **E2E encrypted inter-agent messaging** (Signal Protocol via AGT). Agentgateway can carry A2A traffic but A2A is TLS-to-the-broker, not E2E.
- **Sub-agent spawn validation + federated identity propagation.** Agentgateway is not an agent runtime; sub-agent spawn is a kars-shaped concept that lives at the router because the router is the only path out of the parent agent.
- **Per-sandbox compiled policy bundles** with cosign attestation. Agentgateway uses xDS; the byte-deterministic, cosign-attested compilation step is kars-specific.
- **Multi-runtime adapter framework.** Agentgateway is a gateway. It doesn't run agents and isn't aware of OpenClaw / Hermes / MAF / LangGraph as runtime targets.
- **Microsoft Entra Agent ID** as a first-class per-sandbox identity mode.

### Doesn't overlap — neither has, both should consider

- **Ingress for "human talks to a running agent" sessions.** This is the use case in your message. Neither has it. The next section covers why kars is the natural home for it.

---

## "Developer talks to a running agent" — the ingress use case

You're right that this is a real gap and the right place for it is **a kars-native ingress surface**, not agentgateway.

### Why not agentgateway

Mechanically agentgateway can proxy WebSocket bytes from an external client to a backend pod (the WebSocket parser exists in `parse/websocket.rs`). What it doesn't have, and what this use case actually needs:

1. **Awareness of `KarsSandbox` resources.** Agentgateway routes to Backends. A Backend is a static host, an LLM provider, an MCP server, or an A2A endpoint. There is no Backend type that means "the chat surface of the sandbox named X in namespace kars-Y, running the Hermes gateway on port 18789". Adding that would mean teaching agentgateway about kars CRDs — a non-starter cross-project.
2. **Session affinity tied to a `KarsTask` or conversation thread.** Routing a WebSocket reconnect to *the same* sandbox the user was chatting with five minutes ago requires the gateway to maintain a sandbox-name keyed session table. Agentgateway has MCP stateful session routing but not "per-sandbox per-user chat session affinity".
3. **Authentication that knows "this is Alice and she owns sandbox X"**. Agentgateway has JWT / API-key / OAuth — what's missing is the cluster-side authorization that maps an authenticated identity to a set of sandboxes they're allowed to talk to (and what permissions: chat? pause? kill?).
4. **Auto-discovery of newly spawned sandboxes.** Sandboxes are created and destroyed continuously. Manually re-configuring agentgateway Backends per spawn is infeasible. The Backend would need to be a label-selector `KarsSandbox` watch — which is, again, kars-CRD-aware logic that doesn't belong upstream in agentgateway.
5. **Multi-runtime chat surface diversity.** Hermes chats on port 18789 with TUI/PTY semantics. OpenClaw exposes its own chat shape. MAF exposes something else. Routing chat traffic correctly requires the gateway to know which sandbox runs which runtime and where its chat surface is — kars CRD knowledge.

All five are "the gateway would have to know about kars to do this", which is the wrong direction. It's much cleaner to **put the ingress capability where the kars knowledge already lives** — in the router itself, or in a kars-shipped sibling component.

### Why the kars router is the natural home

The router today knows:
- Which sandbox it lives in (env vars, mounted CR slice).
- Which runtime is running (`spec.runtime.kind`).
- Where the agent's chat surface listens (Hermes 18789, OpenClaw default port).
- The compiled policy bundle (who is allowed to do what).
- The sandbox's mesh DID + Entra Agent ID + Workload Identity binding.

What it needs to gain for the developer-chat use case:
- Listen on a second port for **ingress** (e.g., 8444) — or expose a sibling sidecar so the egress and ingress halves don't share a fate.
- Map incoming HTTP / WebSocket on that port to the local chat surface (Hermes gateway, OpenClaw chat, etc.).
- Apply ingress-side policy: caller authn (Entra ID / SA token / OIDC), per-user rate-limit, audit-trail entry, optional pause/resume/kill action authz.
- A Service that exposes the ingress port and a higher-level kars-ingress component that does cluster-wide path-routing (`/agents/<ns>/<sandbox>/chat` → that sandbox's Service).

This is materially the same shape as the egress side (route through a policy-bound, credential-holding sidecar) but inverted (external client → router → local agent chat surface).

### Why "extend the router" vs "new ingress component"

Three possible shapes, in order of how much we'd change today:

1. **Extend the router to bidirectional.** Add an ingress listener (port 8444) to the same `inference-router` process. Pro: one binary, shared policy bundle, single audit trail. Con: router now has dual-role complexity; if the ingress path is misconfigured it could affect egress.
2. **Ship a sibling ingress sidecar** (`kars-ingress-router`). Pro: clean separation of fate; ingress crash doesn't affect agent egress. Con: more pods/containers per sandbox.
3. **Make the agent's runtime chat surface itself a backend of the router**, so all chat traffic round-trips through one place. Pro: most architecturally consistent. Con: bigger reshape of the runtimes.

The right choice depends on a separate design discussion. The point this doc makes is: **the kars router (per-pod, kars-aware, policy-bound, credential-holding) is the right *layer*** — agentgateway is the wrong layer because it doesn't know about kars sandboxes and shouldn't have to.

---

## What this means for the parity plan and the dev-experience design note

### Correction: dev-experience design note (commit `[needs fix]`)

The previous draft of [dev-experience-design-note.md](dev-experience-design-note.md) said:

> "agentgateway is exactly the right ingress data plane for the agent-conversation path"

That was wrong on closer analysis. Agentgateway cannot route per-sandbox conversation traffic with the session + discovery + authz semantics this use case needs, without absorbing kars-CRD awareness into agentgateway (cross-project non-starter). The ingress for "developer talks to a running agent" belongs in a kars-native component — most naturally an extension of the inference router, possibly a sibling ingress sidecar.

A follow-up commit corrects the doc.

### Correction: parity plan strategy note

The composition framing in the parity plan ("agentgateway in front + kars inside for agents") still holds for the **outbound** path (agent → model fleet via agentgateway). It does **not** apply to the **inbound** path (developer → running agent) because agentgateway doesn't do that today and structurally shouldn't (it would need kars-CRD awareness). A follow-up commit clarifies.

### Net positioning

The crisp answer to "what's the difference between agentgateway and the kars inference router":

> Agentgateway is a centralized **edge** data plane that authenticated callers can use to reach **backends** (LLMs, MCPs, A2A peers, REST APIs) with consistent policy. The kars router is a per-pod **trust boundary** in front of an agent whose user-space is treated as adversarial; it is the only path between that one agent and any external surface, and it holds upstream credentials the agent cannot read. They sit at different layers. In a mature deployment you run both — agentgateway as the cluster-edge LLM/MCP/A2A gateway, kars router as the per-pod trust boundary inside every agent — and the agent's request travels through both.
