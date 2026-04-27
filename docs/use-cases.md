# AzureClaw Use Cases

Three canonical scenarios. Each is implemented end-to-end on dev and is
exercised by the compat / conformance / e2e harness before any merge to `main`.

| # | Scenario | Primary user | Network shape | Reference |
|---|---|---|---|---|
| 1 | **AzureClaw-native agent** | Operator who owns the AKS cluster | Cluster-internal | [`docs/architecture.md`](architecture.md) |
| 2 | **Any-OpenClaw → AzureClaw cloud offload** | Developer running OpenClaw on a laptop or a non-AzureClaw runtime | Laptop ↔ AKS via AgentMesh relay (E2E) | [`docs/any-openclaw-cloud-offload.md`](any-openclaw-cloud-offload.md) |
| 3 | **AzureClaw ↔ AzureClaw mesh** | Two AKS-hosted agents, possibly across tenants/clusters | Cluster ↔ cluster via AgentMesh relay (E2E) | [`docs/e2e-encryption-proof.md`](e2e-encryption-proof.md) |

All three share the same trust boundary:

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

## 2. Any-OpenClaw → AzureClaw cloud offload

> "I run OpenClaw on my laptop (or in NemoClaw, or in any other OpenClaw host).
> A heavy task came in; I want to offload it to a hardened AKS sandbox without
> sharing local credentials, and stream the result back."

### What you do

On the host (laptop / NemoClaw / etc.), install the standalone offload plugin:

```bash
# the azureclaw-mesh plugin is published at sandbox-images/nemoclaw/scripts/azureclaw-mesh/
# (the exact install path depends on the host runtime — see any-openclaw-cloud-offload.md)
azureclaw mesh auth --registry https://registry.<your-domain> --provider github
```

Then in the agent prompt:

> "Offload `analyze_repo("/big-codebase")` to AzureClaw cloud."

### What happens (no credentials leave the host)

1. The `azureclaw-mesh` plugin opens a **CONNECT tunnel** through the egress proxy (Node.js 22 + undici quirks documented in `mesh-plugin/src/connection.ts`).
2. The host registers in the **AgentMesh registry** with an Ed25519-derived AMID.
3. KNOCK handshake → trust evaluation → X3DH key agreement → Double Ratchet session.
4. The host sends an offload request frame to the AzureClaw `controller`.
5. The controller spawns an offload sub-agent sandbox (`offload-<id>`) with the requested model + the parent's task descriptor.
6. The sub-agent runs the task inside the AzureClaw security perimeter — Foundry inference, Content Safety, audit chain, blocklist all in effect.
7. Result + any produced files flow back over the same E2E channel as a `file_transfer` payload.
8. Sub-agent self-destructs.

### Why this matters

- The host **never** sees Azure credentials. The router on the AzureClaw side authenticates via Workload Identity.
- The relay sees only ciphertext.
- Both sides emit AGT audit chain entries (visible to the AKS operator via `kubectl claw attest` once Phase 2 lands; today via `GET /agt/audit`).
- The host's local-data scope is unchanged: only what was sent in the task descriptor leaves.

### Code references
- Plugin (host side): `sandbox-images/nemoclaw/scripts/azureclaw-mesh/`
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

## What's NOT a use case

- AzureClaw is **not** a model router. Model selection sits in Foundry; `InferencePolicy` (Phase 2) is a budget/guardrail CR, not a router.
- AzureClaw is **not** a memory backend. `ClawMemory` (Phase 2) is a Foundry Memory Store binding CR, never an in-cluster store.
- AzureClaw is **not** a managed-MCP host. `McpServer` (schema-only on Phase 1, full reconciler in Phase 2) is for **AKS-hosted private/custom** tool servers; managed MCP stays with Foundry.
- AzureClaw is **not** a SaaS agent author. Agent authoring lives in Microsoft 365 Agent Framework / Copilot Studio. AzureClaw is the AKS runtime substrate; M365 Copilot Studio agents can invoke AzureClaw-hosted MCP servers as a tool surface.
