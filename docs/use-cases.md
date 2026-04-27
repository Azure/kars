# AzureClaw Use Cases

Three canonical scenarios shipping today, plus a roadmap track for future agentic runtimes. Each shipping scenario is implemented end-to-end on `main` and exercised by the compat / conformance / e2e harness before any merge.

> **Looking for "how do I run AzureClaw for *audience X*?"** see [`docs/blueprints/`](blueprints/00-index.md) for the five end-to-end deployment shapes (developer inner-loop, enterprise self-hosted, managed public offload, cross-org federation, sovereign / air-gapped) — each with topology + trust-boundary + flow Mermaid diagrams.

| # | Scenario | Where the user runs | Network shape | Status | Reference |
|---|---|---|---|---|---|
| 1 | **AzureClaw-native agent** | AKS (operator owns the cluster) | Cluster-internal | ✅ Shipping | [`docs/architecture.md`](architecture.md) |
| 2 | **Any OpenClaw → AzureClaw cloud offload** | Laptop / NemoClaw / any OpenClaw host (no AzureClaw CLI required) | Host ↔ AKS via AgentMesh relay (E2E) | ✅ Shipping | [`docs/any-openclaw-cloud-offload.md`](any-openclaw-cloud-offload.md) |
| 3 | **AzureClaw ↔ AzureClaw mesh** | Two AKS-hosted agents, possibly across tenants/clusters | Cluster ↔ cluster via AgentMesh relay (E2E) | ✅ Shipping | [`docs/e2e-encryption-proof.md`](e2e-encryption-proof.md) |
| 4 | **Any agentic runtime → AzureClaw** (LangChain, AutoGen, Semantic Kernel, custom) | Anywhere the runtime runs | Host ↔ AKS via MCP / A2A / AP2 | 🚧 On roadmap | [`docs/phase-0-1-capabilities.md`](phase-0-1-capabilities.md) |

All shipping scenarios share the same trust boundary:

- The agent process (UID 1000) **never** sees Azure credentials.
- All external traffic flows through the per-sandbox **inference router** (UID 1001).
- All inter-agent traffic flows through the **AgentMesh relay** (Signal Protocol — X3DH + Double Ratchet); the relay sees only ciphertext.
- Every tool call, every inference, every mesh message, every handoff, is policy-evaluated by **AGT** (`PolicyDecisionProvider`) and persisted to the **audit chain** (`AuditSink`). See [§Provider seams](architecture.md#four-seam-provider-architecture).

---

## 1. AzureClaw-native agent

> "I run AKS. Give me a hardened, governed AI agent that I can talk to from
> Telegram or a TUI, with all access mediated by Azure AI Foundry."

### What you do

```bash
azureclaw up                                   # provisions AKS + ACR + Foundry + first sandbox
azureclaw add research-bot --model gpt-4.1 \
  --governance --learn-egress
azureclaw credentials update research-bot \
  --telegram-token "<bot-token>"
azureclaw connect research-bot
```

### What you get

- A `ClawSandbox` CR per agent in its own namespace (`azureclaw-<name>`).
- One pod with **2 main containers** + **1 init container**:
  - `egress-guard` (init) → installs UID-1000 iptables egress block.
  - `openclaw` (UID 1000) → agent process; can only reach `localhost` + DNS + reply packets.
  - `inference-router` (UID 1001) → sole external path; calls Azure AI Foundry via Workload Identity.
- Defense-in-depth security (`enhanced` isolation by default):
  - Read-only rootfs, drop ALL caps, non-root, no privilege escalation.
  - Custom strict seccomp profile (219 allowed / 28 blocked syscalls).
  - NetworkPolicy default-deny + 51k-domain blocklist auto-refreshed every 6h.
  - Foundry `Microsoft.DefaultV2` Content Safety + Prompt Shields on every inference.
  - AGT governance: `PolicyEngine`, `TrustManager`, `AuditLogger`, `RateLimiter`, `BehaviorMonitor` (native Rust, in-process, <1µs eval latency).
- **Optional `confidential` isolation** — Kata VM on AMD SEV-SNP hardware; per-pod dedicated kernel. Container escape attempts hit a hardware boundary. Sub-agents inherit the parent's isolation level and cannot downgrade.

### Operator surface

```bash
azureclaw operator                # live TUI dashboard for the whole cluster
azureclaw egress research-bot --learned     # review what the agent reached for
azureclaw policy allow research-bot api.example.com
azureclaw model set research-bot gpt-5-mini # hot-swap the model
azureclaw trace research-bot --network      # eBPF trace
```

### Code references
- Controller reconcile path: `controller/src/reconciler/mod.rs`
- Sandbox pod composition: `controller/src/reconciler/mod.rs::deployment_template`
- Egress-guard init container: `sandbox-images/openclaw/entrypoint.sh`
- Foundry route handlers: `inference-router/src/routes/inference.rs`, `routes/governance.rs`

---

## 2. Any OpenClaw → AzureClaw cloud offload

> "I run OpenClaw on my laptop (or NemoClaw, or any other OpenClaw host).
> A heavy task came in; I want to offload it to a hardened AKS sandbox without
> sharing local credentials or installing anything cloud-specific."

The defining property of this scenario is that **the laptop user does not install AzureClaw**. They install an OpenClaw plugin. AzureClaw is somebody else's problem — a managed AzureClaw provider runs the AKS cluster, mints a one-time pairing token for the user, and the LLM in the user's host registers itself the first time it sees the token.

### Roles

- **AzureClaw operator** — runs the AKS cluster. Has the `azureclaw` CLI. Mints pairing tokens. Today this is typically a self-hosted instance per organisation; the same pattern works for managed AzureClaw providers (cloud or in-house) who hand out tokens via their own portal, SMS, or any secure-share channel.
- **Plugin user** — runs OpenClaw / NemoClaw / any OpenClaw-compatible host on a laptop. Has only the `azureclaw-mesh` plugin installed (we aim to upstream this to OpenClaw). Has **no** `azureclaw` CLI, no kubeconfig, no Azure credentials.

### What the operator does (once per user)

```bash
# On the AKS-side operator workstation:
azureclaw mesh promote --port-forward       # (one-time) expose registry + relay so the laptop can reach them
azureclaw pair generate --name alice-laptop --slots 3 --expires 90d \
  --capabilities offload,handoff
```

`mesh promote` is what makes the cluster's relay/registry reachable from off-cluster — see [§How the laptop reaches the AKS mesh edge](#how-the-laptop-reaches-the-aks-mesh-edge) below for the available modes. `pair generate` then prints (and copies to clipboard) a one-time, opaque, expiring token of the form `azc_pair_v1_<base64url-payload>` containing the controller AMID, the relay/registry URLs that `mesh promote` just established, and a sealed pairing secret. The token is sent to the user via any secure channel — secret-share link, SMS, encrypted email, in-person, etc. Today's self-hosted operator typically uses a one-tap secret-share UI; a managed AzureClaw provider would issue tokens through its own portal.

### What the plugin user does

1. Install the `azureclaw-mesh` plugin in their OpenClaw host (today: bundled into NemoClaw images; goal: upstreamed into OpenClaw plugin marketplace).
2. Either:
   - Set the token as an OpenClaw system environment variable (`AZURECLAW_PAIRING_TOKEN=azc_pair_v1_…`), **or**
   - Paste it once into chat: *"Pair this OpenClaw to AzureClaw cloud with token `azc_pair_v1_…`."*
3. The LLM calls the plugin's `azureclaw_pair` tool with the token. The plugin handshakes with the AzureClaw registry, claims a pairing slot, exchanges X3DH prekeys, and the host is now a registered mesh peer. The token is single-use and auto-zeroized.
4. From that point on the user can simply say: *"Offload `analyze_repo("/big-codebase")` to AzureClaw cloud."*

### What happens under the hood

1. Plugin opens a CONNECT tunnel through the egress proxy (Node.js 22 + undici quirks documented in `mesh-plugin/src/connection.ts`).
2. Plugin registers in the AgentMesh registry with an Ed25519-derived AMID, scoped to the pairing slot.
3. KNOCK handshake → trust evaluation → X3DH key agreement → Double Ratchet session.
4. Plugin sends an offload request frame to the AzureClaw `controller`.
5. Controller spawns an offload sub-agent sandbox (`offload-<id>`) with the requested model + the parent's task descriptor, capped to the pairing's `tokenBudget` and `slots`.
6. Sub-agent runs the task inside the AzureClaw security perimeter — Foundry inference, Content Safety, audit chain, blocklist all in effect.
7. Result + any produced files flow back over the same E2E channel as a `file_transfer` payload.
8. Sub-agent self-destructs.

### How the laptop reaches the AKS mesh edge

The relay and registry live inside the cluster on private ClusterIP services. Something has to expose them so the laptop's plugin can reach them. We ship two mechanisms today and have two more on the roadmap:

| Mode | How | Status | When to use |
|---|---|---|---|
| **Port-forward** (default for testing) | `azureclaw mesh promote --port-forward` opens `kubectl port-forward` tunnels for the registry (`:18080`) and relay (`:18765`) on the operator's workstation; the operator publishes those reachable endpoints in the pairing token. | ✅ Shipping | Single operator laptop, demos, CI, e2e harness. |
| **LoadBalancer + IP allowlist** | `azureclaw mesh promote --allow-ip <cidr>` provisions a public LoadBalancer in front of the registry/relay services, restricted to the supplied CIDR. | ✅ Shipping | Trusted internal network or known external user IPs. |
| **Public managed mesh edge** | A globally addressable, WAF-fronted relay/registry endpoint provisioned for the operator (Application Gateway Ingress + WAF, or a managed cloud edge), no per-user CIDR pinning. | 🚧 Roadmap | Self-hosted operators handing out tokens to anonymous-IP laptops. |
| **Managed AzureClaw provider edge** | The provider runs the relay/registry as part of their service; the pairing token they mint already points at their public endpoints. The plugin user never sees an operator-side detail. | 🚧 Roadmap | SaaS-style consumption. Same wire protocol; pairing token + plugin path are unchanged. |

Whichever edge is in use, the pairing token carries the `relay_url` and `registry_url` fields verbatim, so the plugin doesn't need any additional configuration to follow the operator's edge choice.

### Why this matters

- The plugin user **never** holds Azure credentials, never installs a cloud CLI, never sees a kubeconfig. Onboarding is "paste a string once."
- The token is single-use, expiring, slot-bounded, budget-bounded, capability-bounded — leaking it gives an attacker at most one pairing slot inside one tenant.
- The relay sees only ciphertext.
- Both sides emit AGT audit chain entries (visible to the AKS operator via `kubectl claw attest` once Phase 2 lands; today via `GET /agt/audit`).
- The host's local data scope is unchanged: only what was sent in the offload task descriptor leaves the laptop.

### Code references
- Plugin (host side): `mesh-plugin/src/` and the bundled drop in `sandbox-images/nemoclaw/scripts/azureclaw-mesh/`
- Pairing CRD + token format: `controller/src/pairing.rs`, `cli/src/commands/pair.ts`
- Mesh edge exposure: `cli/src/commands/mesh.ts` (`promote` subcommand)
- Offload reconcile: `controller/src/mesh_peer/offload.rs`
- Spawn handler: `inference-router/src/routes/spawn_policy.rs`, `routes/inference.rs`
- AgentMesh wire patches: `vendor/agentmesh-{sdk,relay,registry}/`
- Walkthrough: `docs/any-openclaw-cloud-offload.md`

---

## 3. AzureClaw ↔ AzureClaw mesh

> "I have multiple AzureClaw agents (in one cluster, or two). I want them to
> coordinate end-to-end-encrypted, with policy + trust + audit on every hop."

### What you do

```bash
# Option A: single cluster, two agents
azureclaw add planner   --model gpt-5-mini --governance
azureclaw add worker    --model gpt-4.1    --governance

# Option B: cross-cluster — expose the registry (Application Gateway Ingress + WAF)
azureclaw up --expose-registry           # provisions public registry endpoints

# Option C: pair two existing sandboxes (one-shot)
azureclaw pair planner worker
```

In a chat:

> `@worker can you review this commit?`

### What happens

1. The plugin emits a `mesh_send` tool call.
2. The router's `policy_provider.decide(...)` evaluates the call (sender trust, peer trust, AGT policy).
3. The plugin uses the local AgentMesh SDK to look up the peer's prekey bundle from the registry.
4. KNOCK handshake → trust gate (registry tier × spawner-affinity bonus).
5. Encrypted payload over the relay (`/agt/relay`).
6. Receiver's `onMessage` handler decrypts via Double Ratchet.
7. The receiver's plugin invokes the OpenClaw native delegation path (`openclaw agent --message`) so the sub-task has access to the receiver's full toolset (Foundry, exec, web search, …).
8. Reply traverses the same E2E channel.

### Why this matters

- **No plaintext fallback.** If E2E encryption fails (key mismatch, decrypt error), the message is dropped and a `security_event` is raised; messages are never delivered in cleartext.
- **Trust scoring is queryable.** `azureclaw mesh status`, `GET /agt/trust`, operator-TUI panel.
- **Phase 1 A2A 1.0.0** is a parallel ingress path for inter-agent traffic that **doesn't** terminate at the relay — see [`docs/adr/0001-a2a-ingress-front-edge.md`](adr/0001-a2a-ingress-front-edge.md). When `ClawSandbox.spec.a2a.expose: true`, the router serves a signed `/.well-known/agent.json` and accepts JSON-RPC `message/send` / `tasks/get` / `tasks/cancel`. Agent-card signatures use the in-tree `SigningProvider` (Ed25519 detached JWS); inbound cards are verified against a hot-reloading trust-store snapshot.

### Code references
- Mesh handlers (plugin side): `cli/src/plugin.ts` (mesh tools)
- Relay route (router side): `inference-router/src/routes/mesh.rs`
- Pairing CRD: `controller/src/pairing.rs` + `pairing_reconciler.rs`
- A2A 1.0.0 inbound: `inference-router/src/a2a/` (14 modules)
- E2E proof: `docs/e2e-encryption-proof.md`
- ADR-0001: A2A ingress front-edge

---

## 4. Any agentic runtime → AzureClaw  *(roadmap)*

> "I'm not running OpenClaw. I'm running LangChain / AutoGen / Semantic Kernel
> / a hand-rolled agent. Can I still get an AzureClaw sandbox as a tool surface?"

The Phase 1 protocol scaffolding (MCP 2026 Streamable HTTP, A2A 1.0.0, AP2 mandates) is in the tree precisely so the answer becomes "yes" without a second framework integration. The modules are implemented and unit-tested today; **route mounting is deferred to Phase 2** so we never expose a default-keys / no-auth path.

### Target shapes

- **MCP (Model Context Protocol).** Any MCP-aware client (Copilot, Claude Desktop, VS Code, custom agents) connects to a `ClawSandbox`'s `/mcp` endpoint and gets the sandbox's tools + governance + audit as a remote MCP server. AzureClaw becomes a hosted-MCP provider that any agent can plug into. Today: modules implemented + tested in `inference-router/src/mcp/`; deferred to Phase 2 mount once the `McpServer` reconciler ships JWKS + signing keys via K8s Secrets.
- **A2A (Agent-to-Agent 1.0.0).** Any A2A-aware peer fetches a signed `/.well-known/agent.json` from a `ClawSandbox` and invokes JSON-RPC `message/send` / `tasks/get` / `tasks/cancel`. Cross-organisation agent collaboration without each side adopting the other's mesh stack. Today: modules implemented + tested in `inference-router/src/a2a/` (14 modules); deferred to Phase 2 mount once the `A2AAgent` reconciler ships agent-card signing keys via K8s Secrets.
- **AP2 (Agent Payments Protocol).** Agents transact under signed mandates (IntentMandate → CartMandate → PaymentMandate) with full audit. Today: ledger + signing scaffolding present; integrating-payments use cases tracked for a later phase.
- **`sigs/agent-sandbox` translator.** A K8s SIG-style standardised sandbox object translates to / from `ClawSandbox`, so anyone using the upstream sandbox API gets AzureClaw's controls automatically. Design landed in Phase 0; reconciler in a later phase.

### Why phase this

Mounting `/mcp` or `/a2a` before keys are CRD-bound would create a default-keys path that an external agent could speak to without authentication — exactly the foot-gun the Phase 0 `no-stubs` and `no-null-provider-prod` CI gates exist to prevent. So the modules ship behind the seams in Phase 1, the reconcilers ship in Phase 2, and the routes mount on the same merge.

### Code references
- MCP modules: `inference-router/src/mcp/{jsonrpc,streamable_http,error,initialize,tools,pipeline,oauth,oauth_layer}.rs`
- A2A modules: `inference-router/src/a2a/` (14 modules)
- Phase status: [`docs/phase-0-1-capabilities.md`](phase-0-1-capabilities.md)
- ADR-0001 (A2A ingress front-edge): [`docs/adr/0001-a2a-ingress-front-edge.md`](adr/0001-a2a-ingress-front-edge.md)

---

## What's NOT a use case

- AzureClaw is **not** a model router. Model selection sits in Foundry; `InferencePolicy` (Phase 2) is a budget/guardrail CR, not a router.
- AzureClaw is **not** a memory backend. `ClawMemory` (Phase 2) is a Foundry Memory Store binding CR, never an in-cluster store.
- AzureClaw is **not** a managed-MCP host *for the public Microsoft-managed catalog*. `McpServer` (schema-only on Phase 1, full reconciler in Phase 2) is for **AKS-hosted private/custom** tool servers; the publicly managed MCP surface stays with Foundry.
- AzureClaw is **not** a SaaS agent author. Agent authoring lives in Microsoft 365 Agent Framework / Copilot Studio. AzureClaw is the AKS runtime substrate; M365 Copilot Studio agents can invoke AzureClaw-hosted MCP servers as a tool surface.

