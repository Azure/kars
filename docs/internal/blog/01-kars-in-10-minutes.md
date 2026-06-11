# Announcing kars — a position paper on running agents on Kubernetes

This is the lead post for the [kars blog series](README.md). It announces kars and lays out the reasoning behind the design choices we expect to be challenged on. If you want depth on a specific surface after reading it, the [series index](README.md) points you at the right deep-dive.

---

## What we're announcing

Kars (Agent Reference Stack for Kubernetes) is a hardened, opinionated runtime for AI agents on Kubernetes. Each agent runs in its own namespace. Each agent's network egress is confined by an iptables-based egress-guard and redirected through a per-pod policy enforcer (the *inference router*) the agent cannot bypass — and from which the agent cannot read the upstream credentials. Eleven CRDs compose into a complete governance picture — model budget, tool allow-list, memory binding, mesh trust topology, egress allowlist, eval runs. Inter-agent messaging is end-to-end encrypted using Signal Protocol. Eight agent frameworks are supported via runtime adapters that all sit behind the same trust boundary.

Kars ships as a Helm chart plus a small CLI. Source is at [github.com/Azure/kars](https://github.com/Azure/kars). It runs on stock Kubernetes; install is `helm install`.

This post explains the design choices behind those one-line claims and the alternatives we considered.

---

## The opinion behind the design

These are the four claims kars is built on. If you agree with them, kars fits. If you disagree with one, we'd like to hear which one and why.

### Claim 1 — The agent's code is adversarial

The LLM's output is untrusted input. A tool the LLM writes a payload for may execute that payload. A sub-agent the agent spawned may be hostile. A plugin loaded at runtime may be malicious. Prompt injection works in practice; indirect prompt injection (via tool-response content the agent treats as instruction) works in practice. We have seen both on production agents.

The implication: **don't put credentials in the agent's process**. Don't trust the agent runtime to do its own egress policy enforcement; it can be tricked, patched, or replaced. Don't trust the framework to do governance; frameworks change quarterly while security primitives shouldn't. Put the trust boundary in a process the agent's user-space cannot reach.

*Therefore kars puts an iptables egress-guard around the agent's UID and an out-of-process Rust router (separate UID, separate memory) on the only path out — both before the agent has a chance to act on the LLM's output.*

### Claim 2 — Governance applies uniformly across call types

Token budgets, content safety, tool allow-lists, model-region pinning, sub-agent spawn validation, memory store access, mesh peer admission — these are *semantic* policies. They depend on what the agent is *asking for*, and the right enforcement point is the boundary between the agent's code and the upstream surface, because that's where the policy can hold the upstream credential and observe every external action consistently.

A single enforcement point also gives operators one audit trail to read, one budget to manage, one allowlist to update — across model calls, tool calls, mesh messages, MCP backends, and sub-agent spawns. With per-call-type enforcement spread across multiple components, attribution and consistency suffer.

*Therefore kars routes all six surfaces (model, tool, MCP, memory, mesh, spawn) through the same router with one policy-bundle schema, one OpenTelemetry shape, one budget ledger.*

### Claim 3 — Inter-agent messaging benefits from end-to-end secrecy

Two agents need to talk to each other. They may live in different namespaces, clusters, or organizations. There is a broker in the middle.

The conventional approach — TLS to the broker, broker forwards, TLS to the recipient — leaves the broker in the trust set: it sees every message body. That is fine when the broker is fully trusted, and increasingly hard to defend when the broker is run by a different team, a different organization, or under cluster-admin authority you cannot prove will never be abused.

Signal Protocol (X3DH key agreement + Double Ratchet for forward secrecy) reduces the broker to a ciphertext-routing role. The broker sees DIDs and ciphertext, nothing else. Forward secrecy is per-message — even if the receiver is compromised today, traffic from prior ratchet steps cannot be decrypted. Post-compromise security restores secrecy after the attacker loses live access to the session state and a fresh DH ratchet step occurs.

This is what AgentMesh (a component of Microsoft AGT — see below) provides. [Post 2](02-agentmesh-deep-dive.md) goes into the protocol details.

*Therefore kars uses upstream Microsoft AGT AgentMesh for every inter-agent message and never builds custom cross-agent transports — the broker is fully out of the trust set.*

### Claim 4 — Multi-runtime is the steady state

There is no single winning agent framework, and there will not be one. OpenClaw, Hermes, Microsoft Agent Framework (MAF), LangGraph (Python and TypeScript), Pydantic AI, the Anthropic SDK, the OpenAI Agents SDK — every team has reasons for its choice. Telling teams "you must rewrite in framework X" is a non-starter.

The trust boundary therefore has to be **framework-agnostic**. The router runs identically regardless of what's in the agent container. The governance CRDs apply identically regardless of runtime. A new framework is added by writing an adapter, not by reimplementing governance. Kars ships eight runtime adapters in one chart today; [post 5](05-multi-runtime.md) explains the contract.

*Therefore kars ships eight runtime adapters in one chart, with a documented small contract (six rules) that any future framework can implement to become a first-class kars runtime.*

---

## Where kars fits relative to the major efforts

### Istio agent gateway + Gateway API Inference Extension

Istio has invested heavily in AI in 2025–26. The `agentgateway` proxy is purpose-built for AI agent and MCP traffic, replacing Envoy where appropriate; the Gateway API Inference Extension introduces `InferencePool` and `InferenceObjective` CRDs for model-aware routing, inference-metric-based load balancing, traffic splitting between model versions, and SLO-aware request shaping. Ambient multicluster mode (beta) reduces per-pod sidecar overhead. There is `TrafficExtension` for in-flight customization via Wasm or Lua, and observability tuned for AI patterns (token accounting, queueing latency, GPU utilization).

This is excellent work for what it solves: **the inference-infrastructure layer — routing requests to model serving backends, splitting versions, enforcing SLOs at the gateway, observing inference traffic**. It is the right tool when your problem is "I have N model deployments behind one gateway and I need traffic management and authorization between callers and those deployments".

Kars sits at a different layer: **the per-agent trust boundary**. Concretely:

- Istio agentgateway is a Gateway API `GatewayClass` (not a sidecar or waypoint per its 1.30 docs). Kars's router lives **in every agent pod** as a sidecar — the egress-guard guarantees the agent has no other path out.
- Istio governs traffic *to and from* the agent (or model) at the network layer. Kars governs traffic *originating in* the agent at the call-semantics layer — token budgets, tool argument validation, sub-agent spawn target validation, mesh peer admission, memory store binding — across multiple call types with one audit shape.
- An Istio gateway / ext-auth component could hold upstream credentials in principle. Kars's stronger property is the combination of **egress confinement** (the agent cannot reach upstream services directly) and **semantic mediation per call type before any upstream credential is minted**, with all of it sitting inside the agent's pod rather than at the cluster edge.
- Istio's authorization is request-level (who can hit which model). Kars's enforcement is per-call-type (token budget across a session, tool argument schema, sub-agent spawn target, mesh peer trust score, memory store binding).

The two compose cleanly. Run Istio agentgateway in front of your Foundry / model-serving cluster for inference-side traffic management. Run kars's per-pod router as the agent-side trust boundary. The model call leaves the agent through the kars router (which mints credentials, enforces semantic policy), traverses the network governed by Istio (which enforces mTLS, request-level authorization, and SLO routing), and reaches the model deployment. Each layer does what only it can do.

### Google A2A (Agent-to-Agent protocol)

A2A is a wire protocol for cross-vendor agent discovery and message exchange. It originated at Google and is now a Linux Foundation project. Kars supports A2A on the **ingress** side: the `A2AAgent` CRD declares a public-ingress endpoint that the `a2a-gateway` crate terminates, validates, and forwards to the destination sandbox's router. Bridging incoming A2A payloads onto the internal AgentMesh substrate for an additional E2E hop is on the roadmap but not in this release.

A2A does not itself provide end-to-end secrecy beyond TLS, and it is not designed for per-pair forward-secrecy or KNOCK-style admission control. For traffic between agents inside a kars trust domain, AgentMesh gives properties A2A does not have. For traffic crossing trust domains, A2A is the right interop choice; kars supports it at the gateway and provides its own per-sandbox authz on the consuming side. We expect A2A to continue evolving; the two protocols are complementary, not substitutes.

### The agent-sandbox SIG

Standardizing agent workload shapes on Kubernetes is something the industry needs. The agent-sandbox SIG is the right venue for that conversation. Kars's design intent is to **align with the SIG's emerging shape via overlay + compatible-mode operation**: a kars sandbox should remain a recognizable agent-sandbox workload under any standardized contract, and kars CRDs should overlay rather than replace the SIG's primitives where they overlap.

Concretely, this means:
- Kars's `KarsSandbox` CR should be readable as a SIG-compliant sandbox descriptor with kars-specific extensions in vendor-prefixed fields.
- Where the SIG specifies a workload shape, kars should produce that shape (controller-side translation).
- Where the SIG specifies a tool/runtime interface, kars's runtime adapters should implement it.

We are deliberately shipping ahead of a finalized standard because users need a hardened runtime now. We expect to converge with the SIG as the standard solidifies — and to feed implementation experience back into the SIG conversation.

### Managed agent platforms

Managed offerings are improving fast and many now support private networking, enterprise governance, multiple model backends, and tenant isolation. The right framing is not "managed is simplistic" — it is **where control-plane ownership matters**. Kars is built for shops that need self-hosted control over the K8s control plane (for airgapped, sovereign, or regulated environments), Kubernetes-native extensibility (CRDs, admission controllers, your own operators alongside ours), and on-cluster multi-team / multi-framework composition with one trust boundary. If those constraints don't bind for you, a managed platform may be a better fit. The [blueprints](../../blueprints/00-index.md) cover dev, enterprise-self-hosted, sovereign-airgapped, cross-org-federation, and managed-public scenarios so you can compare deployment shapes head-to-head.

---

## Why the router is the right enforcement point

The router is a Rust sidecar (axum) in every sandbox pod. The agent's iptables rules (installed by an init container called the *egress-guard*) confine UID 1000 to loopback + DNS, then transparently redirect TCP 80/443 from UID 1000 to the router's port. The agent's HTTP clients work unchanged — they think they're calling `api.openai.com:443` — and every byte they emit lands at the router. There is no other path out.

The router holds:
- Upstream model auth (Workload Identity / IMDS-exchanged tokens, or an Entra-Agent-ID auth sidecar — see below), MCP server credentials, channel tokens — none of which the agent ever sees.
- The compiled policy bundle (mounted as a ConfigMap, hot-reloaded on change), with each policy type having its own enforcement module (`InferencePolicy`, `ToolPolicy`, `KarsMemory`, `EgressApproval`, `McpServer`, `TrustGraph` projection).
- The OpenTelemetry exporter emitting GenAI semantic-convention spans.
- The MCP routing table, the Foundry data-plane proxy, the mesh ingress/egress to the AGT relay.

Per call (model, tool, mesh, memory, spawn — same shape):
1. Receive the (transparently-redirected) request from the agent.
2. Apply the route-appropriate policy module.
3. Mint the upstream credential just-in-time.
4. Forward.
5. Apply outbound policy (content safety on the response, token-budget decrement, telemetry emit).
6. Return.

Why this works:

1. **The agent has no upstream cloud credential to exfiltrate.** Even a perfectly prompt-injected agent has no model API key in its env, file system, or process memory — those live in the router's separate process. (Workspace data, task inputs, retrieved documents, and mesh-session state ARE in the agent's memory and remain in scope for endpoint-compromise threats; the trust-boundary claim is specifically about *upstream credentials*.)
2. **Every external action has one audit shape.** Model call, tool call, mesh message, sub-agent spawn — all flow through the same router, get the same OpenTelemetry treatment, generate one audit record per call.
3. **Framework-agnostic.** OpenClaw, Hermes, MAF — the router doesn't care which is upstream. Governance is uniform.
4. **Composes with everything Kubernetes-native.** Istio sits over the router at the network layer; cosign-signed allowlists feed *into* it; CRDs configure it; the Headlamp plugin reads its emitted telemetry.
5. **One binary to review and audit end-to-end.** Concentrating policy enforcement in one Rust process (vs. spread across eight agent frameworks) gives the security team one place to look. A bug spread across N frameworks is N CVE surfaces; a bug in the router is one.

The alternatives we considered seriously were (a) enforcing at the model provider's API, which loses per-agent identity attribution and per-team policy; (b) enforcing in the agent framework, which requires per-framework reimplementation and trusts the framework not to bypass; (c) enforcing at an out-of-pod gateway, which adds a network hop and does not solve the "agent holds the key" problem on its own. The per-pod router approach avoids all three.

---

## Identity for agents

A kars sandbox can take its upstream identity from one of two router-side modes (today they are exclusive; the router selects on startup based on the presence of `KarsAuthConfig` + the Entra-auth sidecar):

- **Workload Identity (default)** — the sandbox pod's ServiceAccount is federated to a per-sandbox Entra application registration. The router exchanges the IMDS token for a resource token and calls upstream. This is the default for `kars up` on AKS and is the simplest mode for service-style agents.
- **Microsoft Entra Agent ID** — Microsoft's identity system purpose-built for AI agents (GA April 2026). Each agent is a first-class identity in Entra with its own lifecycle, owner, conditional access policies, and audit trail. When the `KarsAuthConfig` CR + the Entra auth sidecar are configured, the router routes all upstream calls through that sidecar; downstream services see the per-sandbox Agent ID as the calling identity. The router fails closed — no fallback to Workload Identity in this mode — which is the property an Agent-ID deployment depends on for clean attribution.

Two other identity surfaces are orthogonal to upstream auth and coexist with both modes above:

- **Mesh DID** — for inter-agent messaging on AgentMesh, each sandbox has a `did:mesh:sha256(pub)[:32]` identifier derived from its long-term Ed25519 keypair. The DID is the addressable identity on the mesh and survives across pod restarts.
- **A2A endpoint identity** — for cross-org A2A traffic, the `A2AAgent` CR carries a public endpoint URL plus a `TrustGraph` projection that constrains which external A2A peers may send to it.

So a single sandbox can simultaneously: hold a mesh DID for peer addressing, expose an A2A endpoint for cross-org ingress, and authenticate upstream via either Workload Identity or Entra Agent ID depending on the router's configured auth mode.

---

## What decomposing an agent over AgentMesh unlocks

When an agent decomposes its work into sub-agents and the sub-agents talk to each other over AgentMesh (the encrypted mesh substrate), several properties become available that monolithic agents do not have:

- **Per-sub-agent governance.** Each sub-agent has its own `KarsSandbox` CR, which means its own `InferencePolicy` (model + region + token budget), its own `ToolPolicy` (which tools it may call with which arguments), its own `EgressApproval` (which external hosts it may reach). A research sub-agent gets a model with a bigger context window and the web-search tool; a code-execution sub-agent gets a smaller, cheaper model and the sandboxed-exec tool; a summarization sub-agent gets neither. Authority granularity is per task, not per agent.
- **Per-sub-agent model and tool selection.** Operators can pin the right model to the right job. A reasoning step uses gpt-5.4; a tool-formatting step uses a smaller, faster model. A sub-agent that should never write to a memory store has no `KarsMemory` binding; one that should has a write-scoped binding. The framework-agnostic property of the runtime means each sub-agent can also be in a *different framework* if that's what the team has — see below.
- **Task offload and workspace offload.** A parent agent can offload a sub-task to a freshly spawned sub-agent (own pod, own namespace, own policy bundle), wait for the result on the mesh, then GC the sub-agent. For longer-running workspaces — code workspaces, document workspaces, research workspaces — the parent can hand the workspace off entirely to a specialist sub-agent and revoke it when done. The sub-agent's CRD lifecycle handles cleanup automatically.
- **Cross-runtime inter-agent communication.** Because AgentMesh is a wire protocol and not a runtime feature, a Hermes (Python) sub-agent and an OpenClaw (TypeScript) parent can exchange end-to-end encrypted Signal Protocol frames using the same DID format, the same X3DH key agreement, the same Double Ratchet semantics, the same KNOCK gate. We rebuilt the Python implementation against the TypeScript reference until both spoke the exact same wire format; an OpenClaw parent doing `kars_mesh_send` to a Hermes child arrives correctly, decrypts on the receiver, gets a Hermes-side reply that the OpenClaw parent decrypts — verified on AKS. We have not found another Kubernetes agent runtime that combines per-agent sandbox governance with cross-runtime Signal-Protocol inter-agent messaging; this lets a team mix runtimes per sub-task without giving up the secrecy and trust properties of the mesh.

The combined effect: an agent decomposed over AgentMesh is **more secure** (smaller blast radius per sub-agent) and **more capable** (mixed models, mixed tools, mixed runtimes per task) than a monolithic agent.

---

## What AGT is and what we contribute

Microsoft AGT (Agent Governance Toolkit) is a broader Microsoft effort: shared governance primitives for AI agents across the Microsoft ecosystem. Open source on `github.com/microsoft/agent-governance-toolkit`. It ships AgentMesh (the Signal-Protocol mesh kars uses for inter-agent encryption), governance hooks (content safety, profile-based tool allowlists, policy attestation), and authoring surfaces.

Kars uses stock AGT upstream — no kars fork. We contribute fixes back, including the Ed25519-Timestamp registry auth, the proof-of-possession on WebSocket connect, the prekey writer-lock that prevents accidental key clobbering, the modern DID format, and the cross-runtime (Python ↔ TypeScript) wire-format alignment.

The strategic direction: as AGT's governance primitives mature, more of kars's enforcement migrates to them. Kars is the K8s-native runtime that hosts AGT-governed workloads; AGT is the cross-product governance vocabulary. We are deliberately not building a competing governance language.

---

## What kars is not

To set expectations:

- **Not a model.** Kars uses Azure OpenAI / Foundry / OpenAI / Anthropic / OpenAI-compatible endpoints upstream.
- **Not an agent framework.** Kars runs agents written in eight frameworks; the agent's logic stays in the framework the team picked.
- **Not a managed service.** Kars is a Helm chart and a CLI; you install it on your own cluster.
- **Not "Kubernetes for LLMs"** in the model-serving sense (that is KServe / vLLM / Ollama territory). It is "Kubernetes for *agents that call* LLMs".
- **Not a competitor to MCP.** Kars consumes MCP servers as tool surfaces; the `McpServer` CRD declares which backends an agent may use.
- **Not the right answer for one agent and one user.** If your shop is N=1, kars is overkill; use a serverless function.

---

## Use cases we are optimizing for

In rough order of frequency:

1. **Enterprise developer platforms** running multiple agents from multiple teams against shared model deployments; need per-team token budgets, per-team policies, audit per call, isolated namespaces.
2. **Compliance-bound agent fleets** (SOC2, FedRAMP, GDPR); need cosign-signed policy bundles, per-call audit, content-safety enforcement.
3. **Sovereign / airgapped deployments** (defense, regulated industries); need everything to work without managed services and without internet egress.
4. **Cross-org B2B agent federation**; agents in your cluster talking to agents in a partner's cluster, with mesh-level E2E secrecy that the broker / relay operator cannot read in transit (endpoint compromise — at either end — remains a separate concern, addressed by confidential-compute isolation, sandbox posture defaults, and the four-layer defense documented in [post 6](06-sandbox-anatomy.md)).
5. **Autonomous SRE for agent fleets** — a kars-native agent that watches the others, diagnoses incidents, proposes typed fixes that an operator approves. [Post 4](04-autonomous-sre.md) covers this.
6. **Multi-framework shops** that want teams to pick OpenClaw / MAF / LangGraph / Hermes / etc. without giving up unified governance.

If your use case sits in one of these, kars is built for you. If it does not, the highest-signal contribution we can think of is an issue with "use case X is not served" — that's how the roadmap evolves.

---

## Summary

Kars is:

- A Kubernetes operator (Rust, kube-rs).
- 11 CRDs that compose into a governance picture.
- A per-pod inference router (Rust, axum) that the agent's iptables-confined egress is transparently redirected through — the only path out of every agent.
- 8 runtime adapters for major agent frameworks, all behind the same trust boundary.
- AgentMesh (Microsoft AGT) for E2E encrypted inter-agent messaging, with verified cross-runtime interoperability (Python ↔ TypeScript).
- Identity options spanning Workload Identity, Microsoft Entra Agent ID, mesh DIDs, and A2A endpoint identities.
- A Headlamp plugin for the operator UI.
- A small CLI for the gaps.

Install: `git clone https://github.com/Azure/kars && cd kars && make build && kars dev` brings up a working agent inside a kind cluster in ~3 minutes.

---

## Where to go next

Pick a deep-dive based on what you care about:

- **Encrypted inter-agent messaging, KNOCK gate, trust scoring, cross-runtime mesh?** → [AgentMesh deep-dive](02-agentmesh-deep-dive.md)
- **The 11 CRDs and how they compose?** → [Governance plane](03-governance-plane.md)
- **Autonomous remediation of broken agents?** → [Autonomous SRE agent](04-autonomous-sre.md)
- **Adding a new agent framework?** → [Multi-runtime](05-multi-runtime.md)
- **Threat model, the four defense layers, what an attacker has to bypass?** → [Sandbox anatomy](06-sandbox-anatomy.md)
- **Day-2 operations, Headlamp plugin, dashboards?** → [Operator UX](07-operator-ux.md)

Or run `kars dev` and try it.
