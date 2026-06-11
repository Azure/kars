# Multi-runtime — one trust boundary, eight agent frameworks

Post 5 in the [kars blog series](README.md).

---

## The premise

In 2026 there is no single winning agent framework. Microsoft has Agent Framework (MAF). Nous has Hermes. Anthropic ships its own SDK. OpenAI ships its own Agents SDK. LangGraph is the de-facto standard in many shops, in two flavors (Python + TypeScript). Pydantic AI is the typed-Python pick. OpenClaw — Microsoft's internal evolution of the OpenAI Agents pattern — is the kars-native default.

Each framework has its own opinions about session lifecycle, tool invocation, memory, sub-agent spawn, and observability. The naive answer is "pick one and standardize". That doesn't work because every team already has a reason for their choice: MAF for Azure-shaped DI, LangGraph for graph-shaped workflows, OpenClaw for browser-grade tool surfaces, Anthropic SDK for native Claude.

The kars answer: **let teams pick their framework, but make all of them sit behind the same router and policy plane**. Eight runtimes, one trust boundary.

---

## What "runtime" means here

A "runtime" in kars is the agent framework + the kars-side adapter that wires it into the sandbox. The router, the egress-guard, the mesh plugin, the policy ConfigMaps — those are identical regardless of runtime. What changes between runtimes is:

- **Session boot semantics.** OpenClaw expects a system prompt + a plugin registry. Hermes expects a "default agent" YAML. MAF expects a Python entrypoint with a registered agent class. LangGraph expects a compiled graph.
- **Tool invocation surface.** OpenClaw's tools are JSON-schema-validated; Hermes uses Pydantic models; LangGraph uses LangChain `BaseTool`; Anthropic SDK uses dataclasses.
- **Mesh integration.** OpenClaw has a TypeScript mesh plugin (`@microsoft/agent-governance-sdk`); Hermes has a Python one (`kars_agt_mesh`). Both speak the same Signal Protocol wire format.
- **Channel adapters.** Telegram/Slack/Discord/WhatsApp integration plugs into each runtime's own channel API.

What *doesn't* change:

- All eight runtimes egress through the same router on `127.0.0.1:8443`.
- All eight are governed by the same nine CRDs ([post 3](03-governance-plane.md)).
- All eight run inside the same sandbox pod shape ([post 6](06-sandbox-anatomy.md)) — same iptables egress-guard, same NetworkPolicy, same seccomp profile.
- All eight authenticate to upstream models via Workload Identity / IMDS — no framework needs to know about Azure auth.

---

## The eight

| Runtime | Language | Where it lives | Notable property |
|---|---|---|---|
| OpenClaw | TypeScript / Node 22 | `runtimes/openclaw/` | Kars-native default. 24 governance-aware tools (`kars_spawn`, `kars_mesh_send`, `foundry_*`). Plugin model. |
| Hermes | Python 3.12 | `runtimes/hermes/` | The Nous Research framework. Embedded TUI chat with a PTY. Used for the SRE agent. |
| Anthropic SDK | Python | `sandbox-images/anthropic/` | Native Claude. Tool use via the SDK's `messages` API. |
| MAF (Microsoft Agent Framework) | Python | `sandbox-images/maf-python/` | Azure-shaped DI, Foundry-native, Microsoft-blessed. |
| LangGraph | Python | `sandbox-images/langgraph/` | Graph-shaped agent workflows; the LangChain ecosystem. |
| LangGraph (TS) | TypeScript | `sandbox-images/langgraph-ts/` | Same model, TypeScript flavor. |
| Pydantic AI | Python | `sandbox-images/pydantic-ai/` | Typed Python, Pydantic-validated tools. |
| OpenAI Agents SDK | Python | `sandbox-images/openai-agents/` | The official OpenAI Agents SDK. |

Plus a documented "BYO" path: any runtime that can speak HTTP can be packaged as a kars sandbox. The contract is small and documented at `docs/runtimes/CONTRACT.md`.

---

## The contract a runtime must honor

To be a kars runtime, the framework's container needs to:

1. **Run the agent as UID 1000.** This is what the egress-guard's iptables rules pin against. Running as any other UID bypasses the guard.
2. **Route ALL external HTTP calls through `127.0.0.1:8443`.** Model calls, MCP tool calls, sub-agent spawns, mesh messages — everything. The runtime must NOT hold its own model API keys, NOT make direct HTTP calls to `api.openai.com`, etc.
3. **Read the policy ConfigMaps from `/etc/kars/`.** The router publishes the compiled policy bundle there; the runtime must respect the policy decisions the router enforces (e.g. don't retry a token-budget-exhausted call).
4. **Speak the mesh wire format.** If the runtime wants inter-agent messaging, it talks to `127.0.0.1:8443/v1/mesh/*` (which proxies to the AGT relay). The Signal-Protocol session state lives in the runtime's mesh plugin.
5. **Emit OpenTelemetry GenAI semantic-convention spans.** The router does this for the model/tool calls it sees; the runtime should add its own spans for in-process work the router doesn't see.
6. **Provide a `/sandbox/spawn` HTTP entry point.** If the runtime supports sub-agents, it forwards spawn requests through the router (which validates against `spawn_policy` before creating the child CR).

That's it. Six rules. Two are about identity (UID, no direct egress), three are about the policy boundary (route through the router, respect ConfigMaps, emit telemetry), one is about the mesh (speak the protocol).

---

## How an adapter actually looks

Take the Hermes adapter. The image is built from `sandbox-images/hermes/Dockerfile`. The interesting layers:

```dockerfile
# Hermes agent base
RUN pip install --no-cache-dir "hermes-agent==${HERMES_VERSION}"

# kars-side Python adapter (the plugin that wires Hermes into kars)
COPY runtimes/hermes/ /opt/kars-runtime-hermes/
RUN pip install --no-cache-dir /opt/kars-runtime-hermes

# The Python mesh transport that speaks Signal Protocol to AGT
COPY runtimes/agt-mesh-python/ /opt/kars-agt-mesh/
RUN pip install --no-cache-dir /opt/kars-agt-mesh
```

The adapter (`runtimes/hermes/src/kars_runtime_hermes/plugin/`) does three things:

1. **At startup**, registers the Hermes plugin with the Hermes agent runtime. The plugin discovers the policy ConfigMaps at `/etc/kars/` and surfaces them to Hermes's tool registry.
2. **For each tool call**, decorates it with the kars governance hook — if the policy says deny, raise; if it says approval-required, suspend and emit a `KarsApproval` request; if it says rate-limit, enqueue.
3. **For mesh interactions**, owns the `MeshClient` singleton from `kars_agt_mesh`. Manages the Signal Protocol session, the prekey upload, the KNOCK gate on inbound, the trust-score map.

The controller-side wiring is `controller/src/reconciler/runtime.rs`. When a `KarsSandbox` has `spec.runtime.kind: Hermes`, the controller:

- Uses the `HERMES_RUNTIME_IMAGE` from env (`kars-runtime-hermes:latest` by default).
- Sets the entrypoint to `/usr/local/bin/kars-hermes-entrypoint.sh`.
- Injects `HERMES_*` env vars from `spec.runtime.hermes.extraEnv`.
- Adds the gateway port (18789) to the Service so operators can `kubectl port-forward` for the embedded TUI chat.

OpenClaw's wiring is the same shape with TypeScript-specific knobs. Same pattern repeated for the other six.

---

## What this lets you do

A team can adopt kars without abandoning their framework. The migration path is:

1. Wrap the team's existing agent in the framework's `sandbox-images/<runtime>/` Dockerfile.
2. Make sure the agent runs as UID 1000.
3. Replace direct API calls with calls to `http://127.0.0.1:8443/v1/...` (most SDKs accept an `endpoint=` override; this is usually a one-line change).
4. Write a `KarsSandbox` CR referencing the appropriate `InferencePolicy` + `ToolPolicy`.
5. `kubectl apply`. Done.

The team's agent code stays in their framework. The platform team's governance, observability, billing, and mesh are added underneath without touching that code.

Conversely: when a new framework appears (it will), adding it as a kars runtime is a few hundred lines of adapter code + a Dockerfile + a wiring entry in `controller/src/reconciler/runtime.rs`. The router/governance/mesh stack underneath doesn't change.

---

## What this is NOT

- **Not a framework abstraction layer.** Kars doesn't try to make all eight frameworks look the same to the developer. The OpenClaw plugin model and the MAF DI pattern are still different; the developer writes against whichever they picked. Kars only unifies the *operational* surface (governance, network, mesh, telemetry).
- **Not a model abstraction layer.** Each runtime talks to whichever model upstream its `InferencePolicy` points at. We don't multiplex one prompt across multiple models — that's the agent's job if it wants to.
- **Not a sub-agent orchestrator.** Sub-agent spawn is per-runtime; kars only provides the secure spawn mechanism (the `/sandbox/spawn` route on the router, the `KarsSandbox` CR creation, the federated credentials). The orchestration logic — who delegates what to whom — lives in the agent code.

---

## Where to look

- **Contract:** `docs/runtimes/CONTRACT.md`.
- **Per-runtime adapters:** `runtimes/<name>/` for OpenClaw + Hermes; the others have minimal adapters baked into their Dockerfiles.
- **Controller wiring:** `controller/src/reconciler/runtime.rs` — the runtime dispatch table.
- **Adding a new runtime:** there's a worked example at `docs/runtimes/adding-a-runtime.md`.

---

## Up next

- **What the runtime ends up running inside?** → [Sandbox anatomy](06-sandbox-anatomy.md)
- **The mesh that all eight runtimes share?** → [AgentMesh deep-dive](02-agentmesh-deep-dive.md)
- **How operators see and manage them?** → [Operator UX](07-operator-ux.md)
