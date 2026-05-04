# CRD reference

AzureClaw exposes its API through eight CustomResourceDefinitions in the `azureclaw.azure.com` group, all at version `v1alpha1`. This page is the canonical schema reference. For the prose explanation of how these fit together, see **[Architecture — CRDs as the API](../architecture.md#crds-as-the-api)**.

> **Stability.** v1alpha1 is the v1.0 contract. We will not remove fields. Optional fields may be added. Behaviour may be tightened (denying things that previously slipped through) but never loosened. The full contract is in **[Backwards compatibility](backwards-compatibility.md)** and **[CRD versioning](../architecture/crd-versioning.md)**.

## At a glance

| CRD | Kind | Short names | Scope | What it represents |
|---|---|---|---|---|
| `clawsandboxes.azureclaw.azure.com` | `ClawSandbox` | `cs`, `claw` | Namespaced | One agent. The unit of work. |
| `a2aagents.azureclaw.azure.com` | `A2AAgent` | `a2a` | Namespaced | A public-ingress endpoint a peer can call. |
| `mcpservers.azureclaw.azure.com` | `McpServer` | `mcp` | Namespaced | An external MCP server the sandbox may call. |
| `toolpolicies.azureclaw.azure.com` | `ToolPolicy` | `tp` | Namespaced | Allow / deny / approval rules for tool calls. |
| `inferencepolicies.azureclaw.azure.com` | `InferencePolicy` | `ip` | Namespaced | Model routing, token budgets, region pinning. |
| `clawmemories.azureclaw.azure.com` | `ClawMemory` | `cmem` | Namespaced | Memory-store binding (Foundry Memory Store). |
| `clawevals.azureclaw.azure.com` | `ClawEval` | `ceval` | Namespaced | Reproducible evaluation run. |
| `trustgraphs.azureclaw.azure.com` | `TrustGraph` | `tg` | Cluster | Cross-namespace / cross-cluster mesh trust topology. |

A ninth CRD, `clawpairings.azureclaw.azure.com` (`ClawPairing`, `cp`), is a controller-internal record used to bind sandboxes to AgentMesh registry IDs. It is created by the controller; you generally do not write it directly.

The full Kubernetes schema lives in `deploy/helm/azureclaw/templates/crd*.yaml`. Below we summarise what each CRD does, the spec fields you write, and the status fields the controller reports back.

---

## `ClawSandbox` — the agent

The unit of work. One `ClawSandbox` per agent.

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: my-agent
  namespace: azureclaw-my-agent
spec:
  runtime:
    kind: OpenClaw                  # or OpenAIAgents | MicrosoftAgentFramework |
                                    #    LangGraph | Anthropic | PydanticAi | BYO
    openclaw:                       # block name matches `kind` (CEL-validated)
      image: azureclawacr.azurecr.io/azureclaw-runtime-openclaw:latest
      tools:
        deny: ["bash.unrestricted"]
  inferenceRef:
    name: shared-inference
  policyRef:
    name: shared-tool-policy
  mesh:
    enabled: true
    trustRef:
      name: my-trust-graph
  governance:
    profile: standard               # one of policy-engine/profiles/*
status:
  phase: Ready
  conditions: [...]
  routerEndpoint: http://127.0.0.1:8443
  podName: my-agent-7d9...
```

**Required**

| Field | Type | Notes |
|---|---|---|
| `spec.runtime.kind` | enum | `OpenClaw`, `OpenAIAgents`, `MicrosoftAgentFramework`, `SemanticKernel` *(deferred)*, `LangGraph`, `Anthropic`, `PydanticAi`, `BYO`. |
| `spec.runtime.<kind>` | object | The kind-specific configuration block. CEL validation enforces that exactly one of these is set and that it matches `kind`. See [Runtime catalog](../runtimes.md). |

**Common optional**

| Field | Type | Purpose |
|---|---|---|
| `spec.inferenceRef.name` | LocalObjectReference | Bind to an `InferencePolicy` (model, tokens, region). If unset, the cluster default applies. |
| `spec.policyRef.name` | LocalObjectReference | Bind to a `ToolPolicy`. If unset, the runtime ships with a deny-all default. |
| `spec.memoryRef.name` | LocalObjectReference | Bind to a `ClawMemory`. |
| `spec.mcpRefs` | `[]LocalObjectReference` | List of `McpServer` resources the agent may call. |
| `spec.mesh.enabled` | bool | Register with AgentMesh. Defaults `false`. |
| `spec.mesh.trustRef.name` | LocalObjectReference | Bind to a `TrustGraph` (cluster-scoped). |
| `spec.governance.profile` | string | Name of a profile under `policy-engine/profiles/`. Defaults to `standard`. |

**Status**

| Field | Notes |
|---|---|
| `status.phase` | `Pending` → `Provisioning` → `Ready`, or `Degraded` / `Failed`. |
| `status.conditions[]` | The full condition chain. Every type documented in [`docs/api/conditions.md`](conditions.md). |
| `status.routerEndpoint` | The in-pod router URL (always `http://127.0.0.1:8443`). |
| `status.podName` | Resolved pod name. |

---

## `A2AAgent` — public-ingress peer endpoint

Binds a public name to a `ClawSandbox` so that A2A 1.2 peers can reach it through the gateway.

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: A2AAgent
metadata:
  name: weather-agent
spec:
  sandboxRef: { name: my-agent }
  publicName: weather-agent.example.com
  agentCard:
    trustAnchorRef: { name: partner-org-anchor }
  rateLimit:
    perMinute: 60
status:
  conditions: [...]
  ingressUrl: https://gateway.example.com/weather-agent
```

The `agentCard.trustAnchorRef` configures which signers' AgentCards the gateway will accept on inbound requests. See **[A2A gateway](../architecture/a2a-gateway.md)**.

---

## `McpServer` — declared MCP backend

Declares an MCP server the sandbox may call. The router enforces — calls to an MCP host that is not declared are denied.

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: McpServer
metadata:
  name: github-mcp
spec:
  url: https://api.githubcopilot.com/mcp
  auth:
    kind: OAuth                    # or ApiKey | ServiceAccount | None
    secretRef: { name: github-mcp-creds }
  scopes: ["read:repo"]
  contentSafety: required          # or optional | off
```

---

## `ToolPolicy` — allow / deny / approval

Decision input for the policy engine. Applied per tool-call class (shell, http, file, sub-agent spawn, etc.).

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: ToolPolicy
metadata:
  name: standard
spec:
  shell:
    allow: ["ls", "cat", "grep"]
    deny: ["rm", "curl"]
  http:
    allowHosts: ["api.github.com", "*.openai.azure.com"]
  fileWrite:
    paths: ["/workspace"]
  subAgentSpawn:
    requireApproval: true
  defaults:
    onUnknown: deny
```

A profile from `policy-engine/profiles/` is the recommended starting point; a `ToolPolicy` overrides specific bits per tenant.

---

## `InferencePolicy` — model routing and budgets

Per-tenant control of *which* model, *how much* of it, and *where* it runs.

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: InferencePolicy
metadata:
  name: shared
spec:
  models:
    primary:
      foundryDeployment: gpt-4.1
      region: swedencentral
    fallback:
      foundryDeployment: gpt-4.1-mini
  budget:
    tokensPerHour: 200000
    rejectOnExceed: true
  contentSafety: required
```

`InferencePolicy` is a separate CRD (rather than fields on `ClawSandbox`) so that one policy can govern many sandboxes — typical for multi-tenant fleets.

---

## `ClawMemory` — memory store binding

Binds a sandbox to a Foundry Memory Store with the correct project-managed-identity wiring (the gotcha is documented in `docs/internal/foundry-memory-store-auth.md` and the inline `azure-prepare` skill — Memory Store uses the project MI for internal model calls, not the AI Services account MI).

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawMemory
metadata:
  name: my-agent-memory
spec:
  foundry:
    project: my-project
    store: episodic
  retention:
    days: 30
```

---

## `ClawEval` — reproducible evaluation run

Pin a sandbox spec to a fixed image+config and run a benchmark against it. The result is recorded in status.

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawEval
metadata:
  name: nightly-regression
spec:
  sandboxRef: { name: my-agent }
  suite: agentbench
  pin:
    runtimeImage: azureclawacr.azurecr.io/azureclaw-runtime-openclaw@sha256:...
status:
  conditions: [...]
  results:
    score: 0.78
    reportUrl: https://...
```

---

## `TrustGraph` — mesh trust topology

Cluster-scoped. Declares which mesh peers are trusted at which trust score, what the threshold is for KNOCK accept, and how trust scores roll up across namespaces / clusters.

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: TrustGraph
metadata:
  name: my-org
spec:
  threshold: 500
  peers:
    - id: ext-partner-1
      score: 600
      via: registry-anchor-A
  rollup:
    sameNamespace: 700
    sameCluster: 600
    sameOrg: 500
```

See **[AGT boundary](../architecture/agt-boundary.md)** for how trust scores are evaluated at KNOCK time.

---

## Lifecycle of a `ClawSandbox`

1. You `kubectl apply` (or `azureclaw add`).
2. Controller creates: namespace → RBAC → ServiceAccount → federated credential → ConfigMap (governance profile) → NetworkPolicy → Deployment → Service.
3. Pod schedules, init `egress-guard` runs iptables rules, agent + router start.
4. Router registers with AgentMesh (if `mesh.enabled`).
5. Controller updates `status.phase = Ready` and writes the condition chain.

If anything fails, the failing phase is reflected as a condition with a `Reason` documented in **[Conditions reference](conditions.md)**. The controller is idempotent — re-running `kubectl apply` after fixing the cause re-converges.

---

## See also

- **[Architecture](../architecture.md)** — the prose explanation.
- **[Runtimes](../runtimes.md)** — the runtime catalog and BYO contract.
- **[Conditions reference](conditions.md)** — every status condition.
- **[Backwards compatibility](backwards-compatibility.md)** — what we promise to keep.
