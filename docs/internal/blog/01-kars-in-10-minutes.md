# Announcing kars — a position paper on running agents on Kubernetes

**Read first.** This is the lead post for the [kars blog series](README.md). It's part announcement, part position paper. If after reading it you want depth on a specific surface, the [series index](README.md) points you at the right deep-dive.

---

## Why bother announcing yet another Kubernetes thing?

Reasonable question. In June 2026 there are at least a dozen "platform for AI agents" projects, half of them open source, half of them in the OSS-but-actually-driven-by-one-vendor zone. There's the [agent-sandbox SIG](https://github.com/agent-sandbox-sig) figuring out a workload-shape standard. There's [Istio agent gateway](https://istio.io/latest/blog/2025/agent-gateway/) extending the service mesh with LLM-aware policy. There's Google's [A2A protocol](https://github.com/google/a2a) for cross-vendor agent interop. There's [Orka](https://github.com/sozercan/orka), [Dapr-AgentRuntime](https://github.com/dapr/dapr-agents), [LangGraph Platform](https://www.langchain.com/langgraph), [OpenAI's Agents SDK](https://github.com/openai/openai-agents-python), and three or four more we're losing track of.

Our pitch for adding one more thing to the pile is not "ours is better". It's:

> **The thing the industry needs in 2026 isn't another agent framework or another model-routing gateway. It's a hardened, opinionated runtime where the agent's code is treated as adversarial and the policy enforcer is the only network path out — applied uniformly across every agent framework, every model provider, every team. That's the gap kars is closing.**

What follows is the rationale. If you finish it and disagree, that's fine — we'd rather argue the design than have you adopt it on vibes.

---

## What kars is, in two sentences

Kars is a Kubernetes operator that gives every AI agent its own namespace, locks the agent's egress to a per-pod policy enforcer (the *inference router*) that the agent cannot reach, and exposes 11 CRDs that compose into a complete governance picture — model budget, tool allow-list, memory binding, mesh trust topology, egress allowlist, eval runs.

The router is the trust boundary. The agent never holds a model API key. Inter-agent messaging is end-to-end encrypted with Signal Protocol. The whole thing runs on stock Kubernetes; install is `helm install`.

---

## The opinion behind the design

These are the four claims kars is built on. If you agree with all four, kars is for you. If you disagree with any, we'd genuinely like to hear why.

### Claim 1 — The agent's code is adversarial

The LLM's output is untrusted input. A tool the LLM writes a payload for could execute that payload. A sub-agent your agent spawned could be hostile. A plugin loaded at runtime could be malicious.

This is not a hypothetical. Prompt injection works. Indirect prompt injection (via a tool's response content) works. We have seen it on production agents.

The implication: **don't put credentials in the agent's process**. Don't trust the agent runtime to do its own egress policy enforcement (it can be tricked, patched, or replaced). Don't trust the framework to do governance (frameworks change quarterly; security primitives shouldn't). Put the trust boundary in a sidecar that the agent cannot reach.

### Claim 2 — Governance lives at the call surface, not the network surface

Token budgets, content safety, tool allow-lists, model-region pinning, sub-agent spawn validation — these are *semantic* policies. They depend on what the agent is *asking for*, not what bytes it's sending.

A service mesh (Istio, Linkerd, Cilium) governs the network. It can enforce TLS, mTLS between pods, L7 HTTP rules. It cannot enforce "this agent has used 1.8M of its 2M daily token budget so reject the next chat completion". It can't, because it sees encrypted TLS bytes — by design.

The right place to enforce semantic policy is **between the agent code and the upstream API**, in a process that holds the upstream credential. That process is the *inference router*. It sees the request body. It mints the upstream token. It enforces the policy. It writes the audit record.

A service mesh is complementary, not competitive. Run Istio for pod-to-pod network policy. Run kars's router for agent-call semantic policy. They sit at different layers.

### Claim 3 — Inter-agent messaging needs E2E secrecy, not broker secrecy

Two agents need to talk. They run in different namespaces, possibly different clusters, possibly different orgs. There's a broker in the middle that routes messages.

The conventional answer is "TLS to the broker, broker forwards, TLS to the recipient". The broker — by construction — sees every message body. This is fine if the broker is fully trusted. It is **not** fine when:

- The broker is run by a different team than either agent.
- The broker is run by a different *org* than either agent (cross-org agent federation).
- The broker is run by you, but cluster-admin compromise would silently leak every agent-to-agent message.
- You need to convince a regulator that no third party can read agent traffic in flight or at rest.

We had all four. So we use **Signal Protocol** between agents (X3DH key agreement + Double Ratchet for forward secrecy) and reduce the broker to a ciphertext-routing role. The broker sees DIDs and ciphertext. Nothing else.

This is what AgentMesh is. We didn't invent it — it's a Microsoft AGT (Agent Governance Toolkit) component, and we contribute back upstream. [Post 2](02-agentmesh-deep-dive.md) goes into the details.

### Claim 4 — Multi-runtime is the steady state

There is no single winning agent framework, and there won't be one. OpenClaw, Hermes, MAF (Microsoft Agent Framework), LangGraph (Python and TS), Pydantic AI, Anthropic SDK, OpenAI Agents SDK — every team has a reason for their pick. Telling teams "you must rewrite in framework X" is a non-starter.

So the trust boundary has to be **framework-agnostic**. The router runs the same regardless of what's in the agent container. The governance CRDs apply the same regardless of runtime. New frameworks are added by writing a small adapter, not by re-implementing governance.

Kars ships eight runtime adapters in one chart. [Post 5](05-multi-runtime.md) explains the contract.

---

## Why not the alternatives

### Why not just put the agent in an Azure Function / AWS Lambda?

Works for N=1 with one user. Breaks at N=10 from multiple teams.

Specific failures:
- No isolation between agents — they share the function app's process space.
- The function platform was not designed assuming the workload could be malicious. The agent has the same egress surface as your code.
- Credentials are pulled from KeyVault at cold start and live in env vars. A prompt-injected agent reads them out of `os.environ` and exfiltrates them via the function platform's outbound IPs (which you can't restrict because Functions needs to call your own APIs).
- No per-agent token budget. Per-app budgets aggregate across teams.
- No inter-agent messaging surface unless you build one. If you build one, you've reinvented a chunk of kars.

If your shop is one agent, one user, one team — keep using your function. We mean that. Don't adopt kars because the announcement was loud.

### Why not Istio agent gateway?

Istio agent gateway is a great fit for **the network-layer parts** of agent traffic. mTLS between sidecars, L7 HTTP authorization on the model-call path, request-level metrics — Istio does all of that well and it composes cleanly with kars.

What it doesn't do, and we don't think it should:

- See into the encrypted Signal Protocol frames between agents. By design, the broker shouldn't see them — see Claim 3.
- Mint upstream model tokens from per-pod federated credentials and enforce token budgets across model deployments. That requires a process holding the upstream credential — Istio's design is that workloads hold their own credentials.
- Validate sub-agent spawn requests against per-parent governance policy and create the child `KarsSandbox` CR. That's K8s-API-level work, not service-mesh work.
- Compose with cosign-attested egress allowlists published as OCI artifacts. Istio's authorization policies are CRDs, not signed bundles — different supply-chain shape.

So: **run Istio for pod-to-pod, run kars's router for agent-call semantics, run AgentMesh for agent-to-agent secrecy**. Three layers, three different problems.

### Why not Google A2A?

A2A is a wire protocol for cross-vendor agent discovery and message exchange. We **do** speak A2A — there's an `A2AAgent` CRD and an `a2a-gateway` crate in this repo. It's our **ingress** path for external A2A-speaking peers (so an agent in someone else's cluster can talk to one of ours).

A2A doesn't have built-in E2E encryption — it relies on TLS plus whatever the broker does, exactly the shape Claim 3 rejects. For intra-kars and intra-trust-domain messaging, AgentMesh gives us E2E secrecy that A2A doesn't have. For cross-trust-domain messaging via A2A, the kars A2A gateway terminates the A2A connection and re-publishes the message to AgentMesh — so the message gains E2E secrecy on the internal hop even though the external sender doesn't speak Signal.

A2A is a complement, not a substitute. We expect more of the industry to converge on A2A for cross-vendor interop, and we'll keep updating the kars A2A gateway as A2A evolves.

### Why not the agent-sandbox SIG's eventual standard?

We **want** the SIG to standardize agent workload shapes on Kubernetes. The fragmentation today is bad for everyone. Kars's design — agent + policy sidecar + per-agent namespace — is convergent with what the SIG conversation suggests is the likely outcome.

We're an early mover. The SIG hasn't shipped a standard. When it does, we'll either align (most likely — our shape is what we'd propose anyway) or contribute to the standard's design from a position of operating experience.

If you're waiting for the SIG to declare a winner before adopting anything — that's a reasonable position. We're shipping ahead of the standard because our internal users need it now, and we'd rather inform the standard from working code than wait for a committee.

### Why not a managed SaaS agent platform?

If your data residency, governance, sovereignty, and cost-per-token constraints are all satisfied by a managed offering — by all means, use it. We're not trying to compete with managed services for use cases they fit.

Where managed offerings struggle:
- Airgapped clusters (defense, regulated industries).
- Sovereign clouds (EU regulators want everything in EU; some require operator-controlled clusters).
- Multi-vendor model routing (an agent that should call gpt-5 for chat and Claude for coding, on a per-call basis, with audit-trail consistency).
- Cross-org B2B federation with E2E secrecy.
- Custom governance hooks (your security review wants a tool to require human approval; managed offerings rarely expose that hook).

Kars is built for the **self-hosted, multi-team, governance-required, possibly-airgapped** end of the spectrum. The blueprints under `docs/blueprints/` cover dev, enterprise-self-hosted, sovereign-airgapped, cross-org-federation, and managed-public scenarios.

---

## Where the router fits, and why we put governance there

The router is a Rust sidecar (axum) listening on `127.0.0.1:8443` in every sandbox pod. The agent's iptables rules drop all egress from UID 1000 except loopback + DNS, so the **only** way the agent can talk to anything external is through the router.

The router holds:

- The upstream model auth (IMDS / Workload Identity token, exchanged on demand).
- The compiled policy bundle (read from `/etc/kars/` as a ConfigMap, hot-reloaded on change).
- The OpenTelemetry GenAI exporter.
- The MCP backend routing table.
- The Foundry data-plane proxy.
- The mesh ingress/egress to the AGT relay.

For every call:

1. Authenticate the caller (loopback + UID check).
2. Apply the route-appropriate policy module (InferencePolicy for model calls, ToolPolicy for tool calls, ContentSafety for both inbound and outbound, etc.).
3. Mint the upstream credential.
4. Forward.
5. Apply outbound policy.
6. Emit telemetry. Decrement budget. Return.

Why this is the right place for governance:

1. **The agent never has a credential.** A perfectly prompt-injected agent has nothing to exfiltrate. The keys live in a process the agent cannot reach.
2. **Single audit boundary.** Every external action — model call, tool call, mesh message, sub-agent spawn — has the same shape: agent → router → upstream. One place to find the audit trail; one place to enforce per-team budgets; one place to inject content-safety.
3. **Framework-agnostic.** OpenClaw, Hermes, MAF, LangGraph — the router doesn't know which is upstream. Governance applies the same regardless.
4. **Composable with anything Kubernetes-native.** Istio sits *over* the router (TCP+TLS layer); cosign-signed allowlists feed *into* the router (policy supply chain); the K8s API watches the policy CRDs that *configure* the router.
5. **Auditable as one binary.** The router is ~30 KLOC of Rust. It can be (and is) reviewed end-to-end. A bug in the router is one CVE; a bug spread across 8 agent frameworks is 8 CVEs.

The alternative we considered most seriously was *enforcing at the model provider's API*. The provider doesn't know per-agent identity or per-team policy. Hop-by-hop attribution via headers is spoofable by a compromised agent. Cross-vendor consistency is impossible. The provider isn't the right place to enforce policy that's specific to *your* governance model.

---

## What AGT is and what we're doing with it

Microsoft AGT (Agent Governance Toolkit) is a broader Microsoft effort: shared governance primitives for agents across the M365 Copilot ecosystem and beyond. Open-source on github.com/microsoft. It ships:

- **AgentMesh** — the Signal-Protocol mesh we use for inter-agent encryption.
- **Governance hooks** — primitives for content safety, profile-based tool allowlists, policy attestation.
- **Authoring tools** — surfaces for defining and validating governance policies.

Kars uses AgentMesh as its mesh transport (no kars fork; we depend on stock upstream). We use AGT's governance profile primitives in our router. We contribute fixes back upstream — the Ed25519-Timestamp registry auth, the proof-of-possession on WebSocket connect, the prekey writer-lock, the modern DID format — all originated as kars contributions to AGT.

The direction: as AGT's governance primitives mature, more of kars's enforcement moves to them. Kars becomes "the K8s-native runtime for AGT-governed agents", and AGT becomes the shared cross-product governance layer. We are deliberately not building a competing governance vocabulary.

---

## What kars is NOT trying to be

To set expectations:

- **Not a model.** Kars uses Azure OpenAI / Foundry / OpenAI / Anthropic / OpenAI-compatible endpoints upstream. It doesn't train, fine-tune, or serve models.
- **Not an agent framework.** Kars runs agents written in eight frameworks. The agent's logic stays in the framework the team chose.
- **Not a managed service.** Kars is a Helm chart + a CLI. You install it on your own K8s cluster.
- **Not "Kubernetes for LLMs"** (KServe, vLLM territory). It is "Kubernetes for *agents that call* LLMs". The difference matters.
- **Not a competitor to MCP.** Kars consumes MCP servers as tool surfaces; sits *above* MCP.
- **Not the answer for N=1.** If you have one agent, one user, one team — kars is overkill. Use a serverless function.

---

## Use cases we're optimizing for

In rough order of how often we hear them:

1. **Enterprise dev platforms** — N teams running M agents against the same Foundry deployment. Need per-team token budgets, per-team policies, audit per call, isolated namespaces.
2. **Compliance-bound agent fleets** — SOC2 / FedRAMP / GDPR. Need cosign-signed policy bundles, full audit trail, per-call OpenTelemetry, content-safety logs.
3. **Sovereign / airgapped agent deployments** — defense, regulated industries. Need everything to work in a cluster with no internet egress and no managed services.
4. **Cross-org B2B agent federation** — agents in your cluster talking to agents in a partner's cluster, with E2E secrecy that neither cluster admin can break.
5. **Autonomous SRE for agent fleets** — the SRE agent watches the other agents, diagnoses incidents, proposes typed fixes that the operator approves. [Post 4](04-autonomous-sre.md) covers this.
6. **Multi-framework shops** — let teams pick OpenClaw / MAF / LangGraph / etc. without giving up unified governance.

If your use case is one of these, kars is built for you. If it's not — give us feedback. The roadmap is at `docs/internal/roadmap.md`; opening an issue with "use case X is unserved" is the highest-signal contribution we can think of.

---

## The boring summary

Kars is:

- A Kubernetes operator (Rust, kube-rs).
- 11 CRDs that compose into a governance picture.
- A per-pod inference router (Rust, axum) that's the only network path out of every agent.
- 8 runtime adapters for major agent frameworks.
- AgentMesh (Microsoft AGT) for E2E encrypted inter-agent messaging.
- A Headlamp plugin for the operator UI.
- A small CLI for the gaps.

Install: `git clone https://github.com/Azure/kars && cd kars && make build && kars dev` → working agent inside a kind cluster in ~3 minutes.

---

## Where to go next

Pick a deep-dive based on what you care about:

- **Encrypted inter-agent messaging, KNOCK gate, trust scoring?** → [AgentMesh deep-dive](02-agentmesh-deep-dive.md)
- **Policy / governance model, the 9 CRDs?** → [Governance plane](03-governance-plane.md)
- **Autonomous remediation of broken agents?** → [The autonomous SRE agent](04-autonomous-sre.md)
- **Adding a new agent framework?** → [Multi-runtime](05-multi-runtime.md)
- **Threat model, the four defense layers?** → [Sandbox anatomy](06-sandbox-anatomy.md)
- **Day-2 operations, Headlamp plugin, dashboards?** → [Operator UX](07-operator-ux.md)

Or just `kars dev` it.
