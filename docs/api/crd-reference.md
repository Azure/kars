# AzureClaw CRD Reference

AzureClaw ships **8 Custom Resource Definitions** under the API group
`azureclaw.azure.com`, version `v1alpha1`. All CRDs are namespaced,
**pre-release** (no conversion webhook; in-place schema edits land via
Helm drift-checked CRD applies), and require Kubernetes 1.30+.

For capabilities overview see [README.md §capabilities](../../README.md#capabilities--phase-2-shipped).
For controller and router architecture see [docs/architecture.md](../architecture.md).
For the BYO runtime contract see [docs/runtime-contract.md](../runtime-contract.md).
For security threat model see [docs/security.md](../security.md).
For CNCF Agent Sandbox compatibility see [docs/sigs-agent-sandbox-compat.md](../sigs-agent-sandbox-compat.md).
For the A2A ingress design rationale see [docs/adr/0001-a2a-ingress-front-edge.md](../adr/0001-a2a-ingress-front-edge.md).

---

## Conventions

### API group and plural/kind table

| Kind | Plural | Short names | Scope |
|---|---|---|---|
| `ClawSandbox` | `clawsandboxes` | `cs`, `claw` | Namespaced |
| `ClawPairing` | `clawpairings` | `cp` | Namespaced |
| `McpServer` | `mcpservers` | `mcp` | Namespaced |
| `ToolPolicy` | `toolpolicies` | `tp` | Namespaced |
| `InferencePolicy` | `inferencepolicies` | `ip` | Namespaced |
| `A2AAgent` | `a2aagents` | `a2a` | Namespaced |
| `ClawMemory` | `clawmemories` | `cmem` | Namespaced |
| `ClawEval` | `clevaluations` | `ceval` | Namespaced |

All reside in group `azureclaw.azure.com`, version `v1alpha1`.

### Field-manager split (Server-Side Apply)

Every controller write uses Server-Side Apply (SSA) with a stable, per-subsystem
`fieldManager` string. This guarantees deterministic field-ownership tracking
across controller restarts, version bumps, and multi-replica HA. The registry is
authoritative in `controller/src/field_managers.rs`.

| Field manager | Owns |
|---|---|
| `azureclaw-controller/clawsandbox` | Namespace, ServiceAccount, Deployment, Service, NetworkPolicy, ConfigMap for sandboxes |
| `azureclaw-controller/pairing` | `ClawPairing.status` (offload-slot lifecycle) |
| `azureclaw-controller/mcp` | `McpServer` JWKS Secret, ConfigMap, status conditions |
| `azureclaw-controller/toolpolicy` | `ToolPolicy` compiled AGT profile, hot-reload bundle |
| `azureclaw-controller/a2aagent` | `A2AAgent` agent-card signing-key Secret, compiled ConfigMap |
| `azureclaw-controller/inferencepolicy` | `InferencePolicy` compiled JSON guardrail profile |
| `azureclaw-controller/clawmemory` | `ClawMemory` binding ConfigMap, status |
| `azureclaw-controller/claweval` | `ClawEval` binding ConfigMap, controller-owned status fields |
| `azureclaw-router` | Runtime-owned status fields (e.g., `ClawEval.status.lastRunAt`, `lastScore`, `lastPass`) |
| `azureclaw-mesh-peer` | `ClawPairing` mesh-peer reconciler (legacy format; preserved for cluster-upgrade compat) |

Cross-namespace references are **never allowed**. A CR may only reference other
CRs in the same namespace. Violations result in `Degraded` with an appropriate
reason rather than a privilege-escalation vector.

### Conditions vocabulary

All controllers emit `status.conditions` following the KEP-1623 convention
(`meta/v1.Condition` shape: `type`, `status`, `reason`, `message`,
`lastTransitionTime`, `observedGeneration`). The table below lists every
condition type emitted across all 8 CRDs.

| Condition type | CRDs | Meaning |
|---|---|---|
| `Ready` | all 8 | `True` when the resource is fully reconciled and healthy. `False` during error states. |
| `Progressing` | `ClawSandbox` | `False` once the controller has completed its reconcile pass (including the `Progressing=False` stamp introduced in S7.B). `True` during active work. |
| `Degraded` | all 8 | `True` when the controller encountered an error it cannot immediately recover from (e.g., missing dependency, write failure). |
| `RuntimeReady` | `ClawSandbox` | `True` once the runtime adapter has confirmed the pod is accepting inference traffic. `False/AdapterMissing` for Tier-2 runtime variants not yet shipped. `False/OverlayMode` in overlay mode. |
| `AllowlistVerified` | `ClawSandbox` | Emitted only when `spec.networkPolicy.allowlistRef` is set. `True/Verified` when the OCI artifact signature checks pass and the policy is applied. `False/<reason>` on fetch or verification failure. |
| `AllowlistDrift` | `ClawSandbox` | Emitted when both `allowlistRef` and inline `allowedEndpoints` are set and they differ. `True/InlineDiffersFromArtifact`. |
| `Suspended` | `ClawSandbox` | `True/OverlayMode` in overlay mode where AzureClaw delegates pod ownership to an upstream reconciler. |
| `EvalsPassed` | `ClawEval` | Written by the **runtime** (field manager `azureclaw-router`) after an eval run. `True` = score met threshold. Never set by the controller. |

#### Condition reasons by CRD

**ClawSandbox** — controller-side reasons for `Ready=False` / `Degraded=True`:
`InferencePolicyNotFound`, `ToolPolicyNotFound`, `DeploymentFailed`,
`NetworkPolicyFailed`, `OverlayMode`, `AdapterMissing`.

**McpServer** — `JwksFetchFailed`, `SigningKeyWriteFailed`, `JwksConfigMapWriteFailed`.

**ToolPolicy / InferencePolicy** — `ProfileWriteFailed`.

**A2AAgent** — `CardWriteFailed`.

**ClawMemory / ClawEval** — `BindingWriteFailed`.

### Validation layers

Three distinct validation layers apply, in order:

1. **CEL `x-kubernetes-validations`** on `spec` — injected by
   `controller/src/crd_validations.rs` after the kube-rs CRD derive. Runs at
   `kubectl apply` time; rejects before etcd commit. See per-CRD tables below.

2. **Cluster admission policies** (`ValidatingAdmissionPolicy` / `MutatingAdmissionPolicy`)
   — see [Cross-cutting admission policies](#cross-cutting-admission-policies). These
   apply to any object in the cluster matching their `matchConstraints`, independently
   of which tool created the CR.

3. **Reconciler-side guard** — a defensive second pass. A CR that bypasses CEL
   (e.g., applied with `--validate=false`) surfaces a `Degraded` condition with
   a descriptive reason rather than silently producing a broken cluster state.

---

## ClawSandbox

**Group/Version/Kind:** `azureclaw.azure.com/v1alpha1`, plural `clawsandboxes`,
kind `ClawSandbox`. Short names: `cs`, `claw`.

**Purpose:** Primary resource. Declares the desired state for a sandboxed AI
agent runtime. The controller reconciles one `ClawSandbox` into an isolated
Kubernetes namespace (`azureclaw-<name>`), a Deployment carrying the
two-container pod (agent runtime + inference-router sidecar), a Service,
NetworkPolicy, seccomp profile binding, and governance ConfigMaps. It is the
unit of isolation, billing, and governance in AzureClaw.

### Spec fields — `spec.runtime`

The `runtime` block selects the agent runtime variant. The `kind` discriminator
selects exactly one variant struct; the others must be absent (enforced by CEL).

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.runtime.kind` | enum | yes | `OpenClaw` | Runtime variant: `OpenClaw`, `OpenAIAgents`, `MicrosoftAgentFramework`, `SemanticKernel`, `LangGraph`, `Anthropic`, `BYO`. Tier-1 adapters (OpenClaw, OpenAIAgents, MicrosoftAgentFramework) are fully wired. Tier-2 variants (SemanticKernel, LangGraph, Anthropic) are schema stubs; the controller stamps `RuntimeReady=False/AdapterMissing` until adapters land. |

**`spec.runtime.openclaw` sub-table** (required when `kind == OpenClaw`):

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.runtime.openclaw.image` | string | no | controller default (`:latest`) | OCI image override. Prefer leaving unset to avoid version drift. |
| `spec.runtime.openclaw.version` | string | no | — | Semantic version hint. Informational; the controller uses `image` for pod spec. |
| `spec.runtime.openclaw.config` | object (JSON) | no | — | Free-form configuration merged into the OpenClaw config file. |
| `spec.runtime.openclaw.extraEnv` | map[string]string | no | — | Extra env vars injected into the openclaw container. Reserved prefixes (`AGT_`, `AZURE_`, `AZURECLAW_`) are stripped by the reconciler. |

**`spec.runtime.openaiAgents` sub-table** (required when `kind == OpenAIAgents`):

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.runtime.openaiAgents.pythonVersion` | string | no | adapter latest | Python interpreter version, e.g. `"3.12"`. |
| `spec.runtime.openaiAgents.agentCode.oci.image` | string | conditional | — | OCI image carrying agent code. |
| `spec.runtime.openaiAgents.agentCode.git.url` | string | conditional | — | Git URL (https/ssh) for dev iteration path. |
| `spec.runtime.openaiAgents.agentCode.git.ref` | string | no | HEAD | Branch/tag/commit SHA. |
| `spec.runtime.openaiAgents.agentCode.git.path` | string | no | repo root | Subdirectory within the cloned repo. |
| `spec.runtime.openaiAgents.entrypoint` | []string | no | adapter default | Container entrypoint override. |
| `spec.runtime.openaiAgents.extraEnv` | map[string]string | no | — | Extra env vars; same reserved-prefix policy as OpenClaw. |

**`spec.runtime.microsoftAgentFramework` sub-table** (required when `kind == MicrosoftAgentFramework`):

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.runtime.microsoftAgentFramework.language` | enum (`python`, `dotnet`) | no | `python` | Language flavour for adapter image selection. |
| `spec.runtime.microsoftAgentFramework.agentCode` | AgentCodeRef | no | — | Same `oci`/`git` shape as `openaiAgents.agentCode`. |
| `spec.runtime.microsoftAgentFramework.entrypoint` | []string | no | adapter default | Container entrypoint override. |
| `spec.runtime.microsoftAgentFramework.extraEnv` | map[string]string | no | — | Extra env vars. |

**`spec.runtime.byo` sub-table** (required when `kind == BYO`):

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.runtime.byo.image` | string | **yes** | — | Container image. Must declare `org.azureclaw.runtime.contract` OCI label matching `contractVersion`. |
| `spec.runtime.byo.contractVersion` | string | **yes** | — | BYO contract version. No silent default — an undeclaring image must not appear contract-compliant. |
| `spec.runtime.byo.command` | []string | no | — | Container command override. |
| `spec.runtime.byo.args` | []string | no | — | Container args override. |
| `spec.runtime.byo.env` | []EnvVar | no | — | Extra env vars (raw K8s `EnvVar` shape; supports `valueFrom`). |

### Spec fields — `spec.sandbox`

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.sandbox.isolation` | string | no | `enhanced` | Isolation level: `standard`, `enhanced`, or `confidential`. |
| `spec.sandbox.seccompProfile` | string | no | `azureclaw-strict` | Named seccomp profile. The cluster auto-stamps `azureclaw-strict.json` via the `seccomp-auto-stamp` MAP when absent. |
| `spec.sandbox.selinuxContext` | string | no | `""` (none) | Custom SELinux type. Empty = no custom type (compatible with `restricted` PodSecurity). Custom types require `baseline` enforcement and a privileged DaemonSet. |
| `spec.sandbox.readOnlyRootFilesystem` | bool | no | `true` | Mount the root filesystem read-only. |
| `spec.sandbox.runAsNonRoot` | bool | no | `true` | Require UID != 0. |
| `spec.sandbox.allowPrivilegeEscalation` | bool | no | `false` | Allow `setuid` / `setgid`. Always `false` in production. |
| `spec.sandbox.writablePaths` | []string | no | `["/sandbox", "/tmp"]` | Paths mounted as `emptyDir` to allow writes despite read-only root FS. |

### Spec fields — `spec.inferenceRef`

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.inferenceRef.name` | string | **yes** | — | Name of an `InferencePolicy` CR in the **same namespace**. Single source of truth for model preference, content-safety floor, and token budgets. Missing → `Degraded/InferencePolicyNotFound`. Cross-namespace refs are not supported. |

### Spec fields — `spec.networkPolicy`

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.networkPolicy.defaultDeny` | bool | no | `true` | Default-deny all egress. Only `allowedEndpoints` are permitted. |
| `spec.networkPolicy.approvalRequired` | bool | no | `true` | Require operator approval before the sandbox can make outbound calls. |
| `spec.networkPolicy.allowedEndpoints` | []EndpointConfig | no | `null` | Explicit egress allowlist. Each entry: `host` (string, required), `port` (uint16), `methods` ([]string), `paths` ([]string). Ignored when `allowlistRef` is set. |
| `spec.networkPolicy.learnEgress` | bool | no | `false` | Observe accessed domains without blocking. Use `azureclaw policy learn` to export. |
| `spec.networkPolicy.allowlistRef.registry` | string | conditional | — | OCI registry hostname for signed egress allowlist artifact. When set, `allowedEndpoints` is ignored; drift surfaces as `AllowlistDrift=True`. |
| `spec.networkPolicy.allowlistRef.repository` | string | conditional | — | Repository path. |
| `spec.networkPolicy.allowlistRef.digest` | string | conditional | — | Content-addressed digest (`sha256:…`). |
| `spec.networkPolicy.allowlistRef.artifactType` | string | conditional | — | OCI artifactType, e.g. `application/vnd.azureclaw.egress-allowlist.v1+yaml`. Consumers reject mismatches. |

### Spec fields — `spec.governance`

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.governance.enabled` | bool | no | `false` | Enable AGT governance (tool policy, trust scoring, audit). |
| `spec.governance.toolPolicyRef.name` | string | conditional | `""` | Name of a `ToolPolicy` CR in the same namespace. Required when `enabled: true`; missing → `Degraded/ToolPolicyNotFound`. The CR's `metadata.name` doubles as the AGT policy profile name (`AGT_POLICY_PROFILE`). |
| `spec.governance.trustThreshold` | int32 | no | `500` | Minimum AGT trust score (0–1000) for inter-agent communication. |
| `spec.governance.trustedPeers` | string | no | — | Pre-seeded trusted peer AMIDs (comma-separated `name:AMID` pairs). Set by the spawner to let a child auto-trust its parent and siblings. |
| `spec.governance.registryMode` | string | no | `"local"` | AGT registry mode: `"local"` or `"global"`. Global enables cross-cluster mesh and handoff tools. |

### Spec fields — `spec.agent`

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.agent.instructions` | string | no | — | System prompt for the Foundry prompt agent. |
| `spec.agent.tools` | []string | no | — | Foundry tools: `file_search`, `web_search`, `code_interpreter`. |
| `spec.agent.fileIds` | []string | no | — | Pre-uploaded Foundry file IDs for knowledge retrieval. |

### Spec fields — `spec.a2a`

Controls inbound A2A 1.0.0 exposure. Default OFF; absent = no inbound A2A path.

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.a2a.enabled` | bool | no | `false` | Master switch. `false` → immediate teardown of Service + CNP + ConfigMap entry. |
| `spec.a2a.allowedCallers` | []AllowedCaller | conditional | — | Required when `enabled: true`. Each entry: `jwsThumbprint` (string, required), `displayName` (string), `issuer` (string). |
| `spec.a2a.expiresAt` | string | conditional | — | RFC 3339 timestamp. Required when `enabled: true`; max 30 days in the future (admission CEL). Reconciler tears down on expiry. |
| `spec.a2a.advertisedSkills` | []AdvertisedSkill | conditional | — | Required when `enabled: true`. Skills surfaced on `/.well-known/agent.json`. Each entry: `name` (required), `description`. |
| `spec.a2a.minimumTrustScore` | uint32 | no | `700` | AGT TrustManager floor. Callers below this score are refused at the gateway before touching the router. |
| `spec.a2a.rateLimit.rpm` | uint32 | no | — | Per-caller requests per minute. |
| `spec.a2a.rateLimit.burst` | uint32 | no | — | Token bucket size. |
| `spec.a2a.bodyCapBytes` | uint32 | no | `1048576` (1 MiB) | Body cap. Hard ceiling 4 MiB enforced by admission CEL. |
| `spec.a2a.sessionMaxSeconds` | uint32 | no | `60` | Session length cap. Hard ceiling 600 seconds. |
| `spec.a2a.allowStreaming` | bool | no | `false` | Allow A2A streaming responses. Fail-closed per ADR-0001 D8. |

### Spec fields — `spec.upstreamCompatibility`

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.upstreamCompatibility.sigsAgentSandbox` | string | no | `"off"` | `sigs.k8s.io/agent-sandbox` translation mode: `"off"` (Native), `"observe"`, `"translate"`, or `"overlay"`. In overlay mode AzureClaw provides only the governance overlay; pod ownership stays with the upstream reconciler. |
| `spec.upstreamCompatibility.upstreamSandboxRef.name` | string | conditional | — | Required when `sigsAgentSandbox == "overlay"`. Name of the upstream `Sandbox` CR in the same namespace. |
| `spec.upstreamCompatibility.aiConformanceReference` | bool | no | `false` | When `true`, controller emits the CNCF conformance status block regardless of other settings. Schema-only. |

### Spec fields — other top-level fields

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.resources.requests` | object | no | — | K8s resource requests (raw JSON). |
| `spec.resources.limits` | object | no | — | K8s resource limits (raw JSON). |
| `spec.azureServices` | []AzureServiceConfig | no | — | Azure services accessible from the sandbox: `service` (one of `storage`, `ai-search`, `cosmos-db`, `ai-foundry`, `keyvault`, `service-bus`, `event-hubs`, `sql`), `account`, `permissions`. **Schema reserved; no role assignments are created yet.** |

### Validation (CEL)

`ClawSandbox` CEL rules are embedded in the Helm CRD manifest (`deploy/helm/azureclaw/templates/crd.yaml`) rather than in `crd_validations.rs` (which covers the other six CRDs). Key invariants enforced at admission:

| Rule | Reason |
|---|---|
| `spec.inferenceRef.name` must be non-empty | `FieldValueRequired` |
| When `spec.a2a.enabled == true`: `allowedCallers` non-empty, `expiresAt` set, `advertisedSkills` non-empty | `FieldValueRequired` |
| `spec.a2a.bodyCapBytes <= 4194304` | `FieldValueInvalid` |
| `spec.a2a.sessionMaxSeconds <= 600` | `FieldValueInvalid` |
| When `spec.upstreamCompatibility.sigsAgentSandbox == "overlay"`: `upstreamSandboxRef` must be set | `FieldValueRequired` |
| Exactly one of `spec.runtime.{openclaw, openaiAgents, microsoftAgentFramework, byo, ...}` is set (matches `kind`) | `FieldValueInvalid` |

### Status fields

| Path | Owner | Description |
|---|---|---|
| `status.phase` | controller | `Pending`, `Creating`, `Running`, `Failed`, `Terminating` |
| `status.sandboxPod` | controller | Name of the created pod. |
| `status.namespace` | controller | Namespace created for this sandbox. |
| `status.inferenceEndpoint` | controller | Internal inference endpoint URL. |
| `status.tokensUsed.input` | runtime | Aggregate input tokens consumed. |
| `status.tokensUsed.output` | runtime | Aggregate output tokens consumed. |
| `status.pendingApprovals` | runtime | Count of tool calls awaiting human approval. |
| `status.foundryAgentId` | controller | Foundry Agent ID created on reconcile. |
| `status.runtimeKind` | controller | Runtime kind observed for `observedGeneration`. Compare with `metadata.generation` to detect stale observations. |
| `status.observedGeneration` | controller | `metadata.generation` last reconciled. |
| `status.conditions` | controller + runtime | KEP-1623 condition list. |

### Conditions emitted

| Type | Reasons | Description |
|---|---|---|
| `Ready` | `AsExpected`, `InferencePolicyNotFound`, `ToolPolicyNotFound`, `DeploymentFailed`, `NetworkPolicyFailed`, `OverlayMode`, `AdapterMissing` | Overall readiness. |
| `Progressing` | `Reconciling`, `OverlayMode`, `AsExpected` | `False` when the controller has completed its reconcile pass. |
| `Degraded` | `InferencePolicyNotFound`, `ToolPolicyNotFound`, `DeploymentFailed`, `NetworkPolicyFailed`, `AdapterMissing` | `True` on unrecoverable error. |
| `RuntimeReady` | `AsExpected`, `AdapterMissing`, `OverlayMode` | Pod-level runtime health. `False/AdapterMissing` for Tier-2 variants not yet shipped. |
| `AllowlistVerified` | `Verified`, `SignerPolicyMissing`, `FetchFailed`, `DigestMismatch` | Emitted only when `allowlistRef` is set. |
| `AllowlistDrift` | `InlineDiffersFromArtifact`, `InlineCleared` | Emitted when ref + inline both set and they disagree. |
| `Suspended` | `OverlayMode` | `True` in overlay mode. |

### Lifecycle

```
Pending → Creating → Running
                   ↓         ↑ (reconcile retry)
                 Degraded ───┘
                   ↓
               Terminating (on deletion)
```

In overlay mode: `Pending → Running (OverlayMode)` with `Suspended=True/OverlayMode`.
For Tier-2 runtimes: `Pending → Degraded/AdapterMissing` until the adapter ships.

### Example

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: my-agent
  namespace: default
spec:
  runtime:
    kind: OpenClaw
    openclaw:
      image: myacr.azurecr.io/my-openclaw-agent:latest
  inferenceRef:
    name: my-inference-policy
  sandbox:
    isolation: enhanced
  networkPolicy:
    defaultDeny: true
    allowedEndpoints:
      - host: api.example.com
        port: 443
  governance:
    enabled: true
    toolPolicyRef:
      name: my-tool-policy
  a2a:
    enabled: true
    allowedCallers:
      - jwsThumbprint: "abc123def456..."
        displayName: parent-agent
    expiresAt: "2026-05-30T00:00:00Z"
    advertisedSkills:
      - name: summarize
        description: Summarise a document
```

**See also:** [architecture](../architecture.md), [runtime-contract](../runtime-contract.md),
[ADR-0001](../adr/0001-a2a-ingress-front-edge.md), [security](../security.md).

---

## ClawPairing

**Group/Version/Kind:** `azureclaw.azure.com/v1alpha1`, plural `clawpairings`,
kind `ClawPairing`. Short name: `cp`.

**Purpose:** Establishes a trust relationship between an external OpenClaw agent
and this AzureClaw cluster. An admin generates a one-time pairing token; the
external agent presents it to bind its mesh identity (AMID) and Ed25519 signing
key. Two offload modes are supported: **task** (ephemeral sandbox for one task,
self-destructs on completion) and **handoff** (full agent state migrates to
cloud, runs long-term, returns on recall). ClawPairing is the gatekeeper for
cross-cluster mesh communication.

### Spec fields

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.tokenHash` | string | **yes** | — | SHA-256 hex hash of the one-time pairing token. Plaintext is never stored. |
| `spec.expiresAt` | string | **yes** | — | ISO 8601 expiry timestamp. |
| `spec.slotsMax` | int32 | no | `1` | Maximum concurrent offload sandboxes. |
| `spec.tokenBudget` | int64 | no | `500000` | Maximum total tokens across all offloads for this pairing. |
| `spec.capabilities` | []string | no | `["offload", "handoff"]` | Granted capabilities. Values: `"offload"` and/or `"handoff"`. |
| `spec.displayName` | string | no | — | Human-readable label for admin reference. |
| `spec.model` | string | no | cluster default | Inference model for offload sandboxes. |
| `spec.isolation` | string | no | cluster default | Isolation level for offload sandboxes: `standard`, `enhanced`, or `confidential`. |

### Validation (CEL)

ClawPairing does not carry CRD-level CEL validation rules in the current schema.
Shape invariants (non-empty `tokenHash`, valid ISO 8601 `expiresAt`) are
enforced reconciler-side, surfacing as `Degraded` conditions.

### Status fields

| Path | Owner | Description |
|---|---|---|
| `status.phase` | controller | `PendingPairing`, `Active`, `Expired`, `Revoked` |
| `status.boundAmid` | controller | External agent AMID, set when pairing token is consumed. |
| `status.boundPubkeyEd25519` | controller | External agent Ed25519 signing public key (base64). |
| `status.pairedAt` | controller | ISO 8601 timestamp when the external agent paired. |
| `status.slotsUsed` | controller | Current concurrent offload sandbox count. |
| `status.tokensUsed` | controller | Total tokens consumed across all offloads. |
| `status.lastOffloadAt` | controller | ISO 8601 timestamp of most recent offload. |
| `status.offloadsCompleted` | controller | Count of completed offloads. |
| `status.offloadsFailed` | controller | Count of failed offloads. |
| `status.activeSandbox` | controller | Name of the currently active offload sandbox (if any). |
| `status.conditions` | controller | KEP-1623 condition list. Skipped on wire when empty. |

### Conditions emitted

| Type | Reasons | Description |
|---|---|---|
| `Ready` | `Active`, `PendingPairing`, `Expired`, `Revoked` | Pairing health. |
| `Progressing` | `Reconciling` | Active reconcile in progress. |
| `Degraded` | `TokenExpired`, `SlotExhausted`, `InvalidShape` | Error state. |

### Lifecycle

```
PendingPairing → Active (on token consumption)
Active → Expired (on expiresAt passing)
Active → Revoked (admin manual revocation)
```

### Example

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawPairing
metadata:
  name: dev-laptop-pairing
  namespace: azureclaw-identity
spec:
  tokenHash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
  expiresAt: "2026-06-01T00:00:00Z"
  slotsMax: 2
  tokenBudget: 1000000
  capabilities:
    - offload
    - handoff
  displayName: "Developer laptop agent"
```

---

## McpServer

**Group/Version/Kind:** `azureclaw.azure.com/v1alpha1`, plural `mcpservers`,
kind `McpServer`. Short name: `mcp`.

**Purpose:** Declarative publication of an MCP 2026 (Streamable HTTP) tool server
reachable from sandboxes in the same namespace. The controller emits an Ed25519
signing-key Secret (`azureclaw.azure.com/mcp-signing-key`) and, when
`productionMode: true`, caches the issuer's JWKS into a ConfigMap that the
inference-router mounts to gate `/mcp` with OAuth 2.1.

Spec tracks [MCP 2026-01-15](https://modelcontextprotocol.io/specification/2026-01-15)
field names exactly for mechanical future migration.

### Spec fields

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.url` | string | **yes** | — | Server endpoint URL. Must be `https://` when `productionMode: true` (CEL). |
| `spec.productionMode` | bool | no | `false` | When `true`, router rejects unauthenticated calls; requires `oauth.issuer`. |
| `spec.oauth.issuer` | string | conditional | — | OAuth 2.1 issuer URL. Required when `productionMode: true`. Must serve a discovery document at `/.well-known/oauth-authorization-server` or `/.well-known/openid-configuration`. |
| `spec.oauth.audience` | string | no | — | Required audience claim on bearer tokens. |
| `spec.oauth.resource` | string | no | — | Required `resource` indicator for token exchange. |
| `spec.oauth.pkce` | string | no | `"S256"` | PKCE method. Only `S256` (RFC 7636 §4.2) is honoured; admission CEL rejects other values. |
| `spec.scopes` | []string | no | `[]` | OAuth scopes the router requests when fronting calls from sandboxes. Per-tool gating is expressed in `ToolPolicy`, not here. |
| `spec.allowedTools` | []string | no | `[]` | Allow-list of tool names. Empty list = deny all tools (fail-closed). Use `["*"]` to allow all and lean on `ToolPolicy` for per-tool governance. |
| `spec.allowedSandboxes.matchLabels` | map[string]string | no | — | Label selector restricting which sandboxes can reach this server. Empty = same-namespace only. |
| `spec.displayName` | string | no | — | Human-readable label for operator TUI. |

### Validation (CEL)

| Rule | Message | Reason |
|---|---|---|
| `spec.productionMode == false \|\| (has(spec.oauth) && size(spec.oauth.issuer) > 0)` | `productionMode requires spec.oauth.issuer to be set` | `FieldValueInvalid` |
| `spec.productionMode == false \|\| spec.url.startsWith('https://')` | `productionMode requires spec.url to begin with https://` | `FieldValueInvalid` |
| `!has(spec.oauth) \|\| !has(spec.oauth.pkce) \|\| spec.oauth.pkce == 'S256'` | `spec.oauth.pkce, when set, must be 'S256' (RFC 7636 §4.2)` | `FieldValueInvalid` |

### Status fields

| Path | Owner | Description |
|---|---|---|
| `status.phase` | controller | `Pending`, `Ready`, `Degraded`, `Unknown` |
| `status.observedGeneration` | controller | `metadata.generation` last reconciled (KEP-1623). |
| `status.conditions` | controller | KEP-1623 condition list. |
| `status.lastProbedAt` | controller | Last health-check timestamp (RFC 3339). |
| `status.signingKeyRef.name` | controller | Name of the Secret holding the Ed25519 signing keypair. The Secret has type `azureclaw.azure.com/mcp-signing-key`; keys: `signing-key.private` (32-byte seed) and `signing-key.public` (32-byte verifying key). |
| `status.jwksConfigMapRef.name` | controller | Name of ConfigMap caching the issuer JWKS. Present only when `productionMode: true`. Key `jwks.json` holds raw RFC 7517 JWKSet bytes. |

### Conditions emitted

| Type | Reasons | Description |
|---|---|---|
| `Ready` | `AsExpected`, `JwksFetchFailed`, `SigningKeyWriteFailed` | Overall readiness. |
| `Progressing` | `Reconciling` | Active reconcile pass. |
| `Degraded` | `JwksFetchFailed`, `SigningKeyWriteFailed`, `JwksConfigMapWriteFailed` | Error state. |

### Lifecycle

```
Pending → Ready (signing key created, JWKS cached if productionMode)
Ready → Degraded (issuer unreachable, Secret write failed)
Degraded → Ready (retry)
```

### Example

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: McpServer
metadata:
  name: my-tools
  namespace: default
spec:
  url: https://tools.example.com/mcp
  productionMode: true
  oauth:
    issuer: https://login.microsoftonline.com/my-tenant/v2.0
    audience: api://my-tools
    pkce: S256
  allowedTools:
    - web_search
    - file_read
  scopes:
    - tools.call
```

---

## ToolPolicy

**Group/Version/Kind:** `azureclaw.azure.com/v1alpha1`, plural `toolpolicies`,
kind `ToolPolicy`. Short name: `tp`.

**Purpose:** Per-tool policy gating: AP2 commerce spend caps, rate limits, and
human-in-the-loop approval. Compiled by the controller to an AGT policy profile
and hot-reloaded into sandboxes that reference it. Resolution is bottom-up:
most-specific `appliesTo` selector wins. Precedence rules are documented in
`docs/crd-precedence.md`.

### Spec fields

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.appliesTo.tool` | string | no | — | Tool name as advertised by the MCP server. `"*"` matches all. |
| `spec.appliesTo.mcpServer` | string | no | — | `McpServer.metadata.name` the tool must come from. Empty = any. |
| `spec.appliesTo.sandboxMatchLabels` | map[string]string | no | `{}` | Sandbox label selector (AND with other fields). Must be non-empty (CEL). |
| `spec.commerce.dailyCap` | string | no | — | Daily spend cap as ISO-4217 currency string with 2-decimal-place integer minor units, e.g. `"USD 100.00"`. |
| `spec.commerce.monthlyCap` | string | no | — | Monthly spend cap. Must be >= `dailyCap` (admission CEL). |
| `spec.commerce.counterpartyAllowlist` | []string | no | `[]` | AP2 counterparty allowlist (DID or domain). Empty = deny all (fail-closed). |
| `spec.commerce.perTransferCap` | string | no | — | Per-transfer hard cap. A single transfer above this is refused regardless of daily/monthly balance. |
| `spec.rateLimit.rps` | uint32 | no | — | Requests per second across all matching invocations. |
| `spec.rateLimit.burst` | uint32 | no | — | Token bucket burst size. |
| `spec.rateLimit.window` | string | no | — | Counter window, e.g. `"1m"`, `"1h"`, `"24h"`. |
| `spec.approval.mode` | string | no | `"aboveThreshold"` (when commerce set) | Approval mode: `"never"`, `"always"`, or `"aboveThreshold"`. |
| `spec.approval.threshold` | string | no | — | Currency threshold above which approval is required. Meaningful only when `mode == "aboveThreshold"`. |
| `spec.approval.channel` | string | no | — | Approval channel reference (e.g., Telegram bot, email address). |
| `spec.displayName` | string | no | — | Human-readable label. |

### Validation (CEL)

| Rule | Message | Reason |
|---|---|---|
| `!has(spec.commerce) \|\| spec.commerce.dailyCap <= spec.commerce.monthlyCap` | `spec.commerce.dailyCap must be <= spec.commerce.monthlyCap` | `FieldValueInvalid` |
| `!has(spec.commerce) \|\| (spec.commerce.dailyCap >= 0 && spec.commerce.monthlyCap >= 0)` | `spec.commerce.{dailyCap,monthlyCap} must be non-negative` | `FieldValueInvalid` |
| `has(spec.appliesTo.matchLabels) && size(spec.appliesTo.matchLabels) > 0` | `spec.appliesTo.matchLabels must contain at least one label` | `FieldValueInvalid` |

### Status fields

| Path | Owner | Description |
|---|---|---|
| `status.phase` | controller | `Pending`, `Ready`, `Degraded`, `Unknown` |
| `status.observedGeneration` | controller | `metadata.generation` last reconciled. |
| `status.conditions` | controller | KEP-1623 condition list. |
| `status.lastCompiledAt` | controller | RFC 3339 timestamp of last successful AGT profile compile. |

### Conditions emitted

| Type | Reasons | Description |
|---|---|---|
| `Ready` | `AsExpected`, `ProfileWriteFailed` | Compilation and write health. |
| `Progressing` | `Reconciling` | Active reconcile. |
| `Degraded` | `ProfileWriteFailed` | AGT profile write failed. |

### Lifecycle

```
Pending → Ready (AGT profile compiled and written)
Ready → Degraded (profile write failed)
Degraded → Ready (retry)
```

### Example

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: ToolPolicy
metadata:
  name: commerce-policy
  namespace: default
spec:
  appliesTo:
    tool: purchase
    mcpServer: my-tools
    sandboxMatchLabels:
      env: production
  commerce:
    dailyCap: "USD 50.00"
    monthlyCap: "USD 1000.00"
    counterpartyAllowlist:
      - did:web:shop.example.com
  approval:
    mode: aboveThreshold
    threshold: "USD 25.00"
    channel: telegram-bot
  rateLimit:
    rps: 5
    burst: 10
    window: 1m
```

---

## InferencePolicy

**Group/Version/Kind:** `azureclaw.azure.com/v1alpha1`, plural `inferencepolicies`,
kind `InferencePolicy`. Short name: `ip`.

**Purpose:** Sandbox-side token-budget, content-safety-floor, and model-preference
policy. Not a model router — it declares *which Foundry route to prefer* and
fallback order, plus guardrail floors that the inference-router enforces at every
inference call site. Referenced by `ClawSandbox.spec.inferenceRef`. Compiled to a
JSON profile ConfigMap that the router hot-reloads (S7). Resolution is bottom-up
by `appliesTo` specificity.

### Spec fields — `spec.appliesTo`

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.appliesTo.sandboxName` | string | no | — | Exact sandbox name (`ClawSandbox.metadata.name`). Empty = any sandbox in namespace. |
| `spec.appliesTo.sandboxMatchLabels` | map[string]string | no | `{}` | Label selector on sandboxes (AND with `sandboxName`). |
| `spec.appliesTo.action` | string | no | — | Inference action filter: one of `chat`, `responses`, `image`, `embeddings`, `*`. Maps to call sites in `inference-router/src/routes/inference.rs`. |

### Spec fields — `spec.tokenBudget`

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.tokenBudget.perRequestTokens` | uint64 | no | — | Per-request hard cap. Exceeding this causes the call to be refused before upstream forwarding. |
| `spec.tokenBudget.dailyTokens` | uint64 | no | — | Daily aggregate cap (input + output tokens). |
| `spec.tokenBudget.monthlyTokens` | uint64 | no | — | Monthly aggregate cap. Must be >= `dailyTokens` (admission CEL). |

### Spec fields — `spec.contentSafety`

Each field declares the **maximum tolerated** severity for its category.
A response with a finding above the floor is blocked. Absent field = category
not policed by this CR.

Severity values (Microsoft Content Safety `Microsoft.DefaultV2`):
`Safe` (strictest) < `Low` < `Medium` < `High` (most permissive).

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.contentSafety.hate` | enum | no | — | Hate speech severity floor. |
| `spec.contentSafety.selfHarm` | enum | no | — | Self-harm severity floor. |
| `spec.contentSafety.sexual` | enum | no | — | Sexual content severity floor. |
| `spec.contentSafety.violence` | enum | no | — | Violence severity floor. |
| `spec.contentSafety.requirePromptShields` | bool | no | — | When `true`, router fails-closed if Prompt Shields annotations are missing from the response. |

### Spec fields — `spec.modelPreference`

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.modelPreference.primary.provider` | string | conditional | — | Provider tag: `azure-openai`, `anthropic`, `gemini`, `bedrock`, or `ollama`. Required when `modelPreference` is set (admission CEL). |
| `spec.modelPreference.primary.deployment` | string | conditional | — | Deployment name as advertised by the provider. Required when `modelPreference` is set. |
| `spec.modelPreference.fallback` | []ModelRef | no | `[]` | Ordered fallback routes. Each entry: `provider` + `deployment`. Tried in order on primary 5xx/429; first healthy wins. |
| `spec.displayName` | string | no | — | Human-readable label. |

### Validation (CEL)

| Rule | Message | Reason |
|---|---|---|
| `monthlyTokens >= dailyTokens` (when both set) | `spec.tokenBudget.monthlyTokens must be >= spec.tokenBudget.dailyTokens` | `FieldValueInvalid` |
| `monthlyTokens >= perRequestTokens` (when both set) | `spec.tokenBudget.monthlyTokens must be >= spec.tokenBudget.perRequestTokens` | `FieldValueInvalid` |
| All contentSafety severities in `['Safe','Low','Medium','High']` | `spec.contentSafety severities must be one of: Safe, Low, Medium, High` | `FieldValueInvalid` |
| `modelPreference.primary.provider` non-empty when `modelPreference` set | `spec.modelPreference.primary requires non-empty provider and deployment` | `FieldValueInvalid` |
| Each `modelPreference.fallback[*]` has non-empty `provider` and `deployment` | `spec.modelPreference.fallback[*] requires non-empty provider and deployment` | `FieldValueInvalid` |
| `appliesTo.action` in `['chat','responses','image','embeddings','*']` | `spec.appliesTo.action must be one of: chat, responses, image, embeddings, *` | `FieldValueInvalid` |

### Status fields

| Path | Owner | Description |
|---|---|---|
| `status.phase` | controller | `Pending`, `Ready`, `Degraded`, `Unknown` |
| `status.observedGeneration` | controller | `metadata.generation` last reconciled. |
| `status.conditions` | controller | KEP-1623 condition list. |
| `status.profileConfigMapRef.name` | controller | Name of the compiled-profile ConfigMap. The router-side informer (S7) watches by label selector. |
| `status.versionHash` | controller | Hex-encoded sha256 prefix (32 chars) of the compiled profile JSON. Same input → same hash; immune to map reordering. |
| `status.lastCompiledAt` | controller | RFC 3339 timestamp of last successful compile. |

### Conditions emitted

| Type | Reasons | Description |
|---|---|---|
| `Ready` | `AsExpected`, `ProfileWriteFailed` | Compile and write health. |
| `Progressing` | `Reconciling` | Active reconcile. |
| `Degraded` | `ProfileWriteFailed` | Profile ConfigMap write failed. |

### Lifecycle

```
Pending → Ready (guardrail profile compiled and written to ConfigMap)
Ready → Degraded (ConfigMap write failed)
Degraded → Ready (retry)
```

### Example

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: InferencePolicy
metadata:
  name: production-guardrails
  namespace: default
spec:
  appliesTo:
    sandboxName: my-agent
    action: chat
  tokenBudget:
    perRequestTokens: 8000
    dailyTokens: 500000
    monthlyTokens: 10000000
  contentSafety:
    hate: Low
    selfHarm: Safe
    sexual: Low
    violence: Low
    requirePromptShields: true
  modelPreference:
    primary:
      provider: azure-openai
      deployment: gpt-4o
    fallback:
      - provider: azure-openai
        deployment: gpt-4o-mini
```

**See also:** [Foundry Memory Store auth caveat](../architecture.md) — Memory Store
operations that internally call models require the project's managed identity to
hold `Azure AI User` on the resource group with token audience `https://ai.azure.com/`.

---

## A2AAgent

**Group/Version/Kind:** `azureclaw.azure.com/v1alpha1`, plural `a2aagents`,
kind `A2AAgent`. Short name: `a2a`.

**Purpose:** Publication of an A2A 1.2 agent card. Each CR yields one
cluster-resident AgentCard ConfigMap (`a2aagent-{name}-card`, key `agent.json`).
The router-side projection (`inference-router::a2a::snapshot_rebuild`) unions all
`A2AAgent` CRs into its trust store (S7). Agent-card signing and the
`/.well-known/agent.json` mount are wired in the S7 pass.

Spec tracks [A2A 1.2 AgentCard](https://a2a-protocol.org/spec/v1.2/agent-card)
field names exactly.

### Spec fields

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.endpointUrl` | string | **yes** | — | HTTPS URL where the agent serves `/.well-known/agent.json` and JSON-RPC endpoints. Must be `https://` when `productionMode: true` (CEL). |
| `spec.signingKeys` | []A2aSigningKey | **yes** | — | One or more Ed25519 signing keys. Must contain at least one entry (CEL). |
| `spec.productionMode` | bool | no | `false` | When `true`, router rejects unauthenticated traffic. |
| `spec.capabilities` | []string | no | `[]` | A2A protocol capabilities advertised in the AgentCard (`tasks`, `streaming`, `cancel`, `mandates`, etc.). |
| `spec.displayName` | string | no | — | Human-readable name; embedded in AgentCard `name` field. |
| `spec.description` | string | no | — | Human-readable description; embedded in AgentCard `description` field. |

**`spec.signingKeys[*]` sub-table:**

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.signingKeys[*].kid` | string | **yes** | — | Stable key identifier exposed to verifying peers. |
| `spec.signingKeys[*].alg` | string | **yes** | — | Algorithm pin. Must be `"EdDSA"` (CEL). The router-side projection rejects any other value. |
| `spec.signingKeys[*].publicKeyB64u` | string | **yes** | — | Ed25519 public key, base64url-encoded without padding (RFC 7515 §2). Decoded length must be 32 bytes. |
| `spec.signingKeys[*].notAfter` | int64 | no | — | Unix-seconds expiry. Absent = never expires. |

**`spec.trust` sub-table:**

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.trust.requireSignedRequests` | bool | no | `false` | Reject inbound A2A requests lacking a valid JWS detached signature from a known peer key. |
| `spec.trust.minSignaturesRequired` | uint32 | no | `1` | Minimum independent valid signatures required (A2A 1.2 §6.4 multi-sig). Values > 1 relevant for AP2 cart mandates. |
| `spec.trust.maxClockSkewSeconds` | int64 | no | `60` | Maximum tolerated clock skew between signing peer and verifier. Values below 5s risk NTP brittleness. |

**`spec.federation[*]` sub-table:**

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.federation[*].label` | string | **yes** | — | Short label for audit logs and AgentCard `federation.peers[*].label`. |
| `spec.federation[*].kind` | string | **yes** | — | `"in-cluster"` or `"external"`. |
| `spec.federation[*].agentRef` | string | conditional | — | Required when `kind == "in-cluster"`. Name of the `A2AAgent` CR in the same namespace. Cross-namespace federation is not supported. |
| `spec.federation[*].endpointUrl` | string | conditional | — | Required when `kind == "external"`. `https://` URL of the peer's `/.well-known/agent.json`. |
| `spec.federation[*].pinnedKid` | string | conditional | — | Required when `kind == "external"`. Pinned `kid` for the peer's outbound signatures. Defends against silent key rotation. |

**`spec.policyRefs`:**

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.policyRefs.toolPolicy` | string | no | — | Name of a `ToolPolicy` CR whose `commerce`/`approval`/`rateLimit` blocks apply to A2A `message/send` requests on this agent. Joined at request time by the router (S7). |

### Validation (CEL)

| Rule | Message | Reason |
|---|---|---|
| `size(spec.signingKeys) > 0` | `spec.signingKeys must contain at least one entry` | `FieldValueInvalid` |
| `spec.signingKeys.all(k, k.alg == 'EdDSA')` | `spec.signingKeys[*].alg must be 'EdDSA'` | `FieldValueInvalid` |
| `spec.productionMode == false \|\| spec.endpointUrl.startsWith('https://')` | `productionMode requires spec.endpointUrl to begin with https://` | `FieldValueInvalid` |
| `federation peers: in-cluster → agentRef only; external → endpointUrl + pinnedKid` | See CEL source for full rule | `FieldValueInvalid` |

### Status fields

| Path | Owner | Description |
|---|---|---|
| `status.phase` | controller | `Pending`, `Ready`, `Degraded`, `Unknown` |
| `status.observedGeneration` | controller | `metadata.generation` last reconciled. |
| `status.conditions` | controller | KEP-1623 condition list. |
| `status.agentCardConfigMapRef.name` | controller | Name of the published AgentCard ConfigMap (`a2aagent-{name}-card`, key `agent.json`). |
| `status.versionHash` | controller | SHA-256 prefix (32 hex chars) of the canonicalised compiled AgentCard. |
| `status.lastCompiledAt` | controller | RFC 3339 timestamp of last successful compile. |

### Conditions emitted

| Type | Reasons | Description |
|---|---|---|
| `Ready` | `AsExpected`, `CardWriteFailed` | AgentCard publication health. |
| `Progressing` | `Reconciling` | Active reconcile. |
| `Degraded` | `CardWriteFailed` | AgentCard ConfigMap write failed. |

### Lifecycle

```
Pending → Ready (AgentCard ConfigMap published, signing-key Secret emitted)
Ready → Degraded (write failure)
Degraded → Ready (retry)
```

### Example

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: A2AAgent
metadata:
  name: my-a2a-agent
  namespace: default
spec:
  endpointUrl: https://agent.example.com
  productionMode: true
  displayName: My A2A Agent
  description: Handles document summarisation tasks
  capabilities:
    - tasks
    - streaming
  signingKeys:
    - kid: primary-2026
      alg: EdDSA
      publicKeyB64u: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
  trust:
    requireSignedRequests: true
    maxClockSkewSeconds: 30
  federation:
    - label: parent-agent
      kind: in-cluster
      agentRef: orchestrator-agent
  policyRefs:
    toolPolicy: commerce-policy
```

**See also:** [ADR-0001](../adr/0001-a2a-ingress-front-edge.md),
[sigs-agent-sandbox-compat](../sigs-agent-sandbox-compat.md).

---

## ClawMemory

**Group/Version/Kind:** `azureclaw.azure.com/v1alpha1`, plural `clawmemories`,
kind `ClawMemory`. Short name: `cmem`.

**Purpose:** Binding/provisioning resource over **Azure AI Foundry Memory Store**.
This CRD configures Foundry Memory Store for a sandbox; it does **not** create an
in-cluster memory backend (per `docs/implementation-plan.md` §3 non-compete).
The controller compiles the spec to a binding JSON ConfigMap
(`clawmemory-{name}-binding`). The runtime path (`cli/src/plugin.ts::ensureMemoryStore`)
reads the ConfigMap and calls Foundry on first use. The controller never holds a
Foundry credential.

A sandbox may have multiple `ClawMemory` CRs provided they each declare a distinct
`scope` key. Conflict detection is router-side (S7).

> **Auth caveat:** Memory Store operations that internally call models
> (`update_memories`, `search_memories` with items) require the **project's**
> managed identity to hold `Azure AI User` on the **resource group** (not the AI
> Services resource) with token audience `https://ai.azure.com/`. CRUD and empty
> searches work with the standard workload identity grant. See
> [architecture.md](../architecture.md) and [Foundry Memory Store auth](../architecture.md).

### Spec fields

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.storeName` | string | **yes** | — | Foundry Memory Store name. CEL: 1–63 chars, DNS-label (lowercase alphanumeric + dashes). The runtime creates the store on first use if absent. |
| `spec.sandboxRef.name` | string | **yes** | — | Sandbox name (`ClawSandbox.metadata.name`). CEL: 1–253 chars. |
| `spec.scope` | string | **yes** | — | Scope key under which this sandbox writes/reads memories. Foundry partitions data per scope. CEL: 1–256 chars. Default convention (set by runtime if absent): `agent:{sandboxName}`. |
| `spec.retentionDays` | uint32 | no | — | Retention floor in days. Runtime applies `delete_scope` sweep on TTL. CEL: > 0 when set. |
| `spec.deleteOnSandboxDelete` | bool | no | `true` | When `true`, runtime calls `delete_scope` on this scope when the sandbox or this CR is deleted. Controller finalizer cleans up the binding ConfigMap; Foundry-side delete is wired in S7+. |
| `spec.displayName` | string | no | — | Human-readable label. |

### Validation (CEL)

| Rule | Message | Reason |
|---|---|---|
| `size(spec.storeName) > 0 && size(spec.storeName) <= 63 && spec.storeName.matches('^[a-z0-9]([-a-z0-9]*[a-z0-9])?$')` | `spec.storeName must be a DNS-label (1-63 chars, lowercase alphanumeric + dashes)` | `FieldValueInvalid` |
| `size(spec.sandboxRef.name) > 0 && size(spec.sandboxRef.name) <= 253` | `spec.sandboxRef.name must be 1-253 characters` | `FieldValueInvalid` |
| `size(spec.scope) > 0 && size(spec.scope) <= 256` | `spec.scope must be 1-256 characters` | `FieldValueInvalid` |
| `!has(spec.retentionDays) \|\| spec.retentionDays > 0` | `spec.retentionDays must be > 0 when set` | `FieldValueInvalid` |

### Status fields

| Path | Owner | Description |
|---|---|---|
| `status.phase` | controller | `Pending`, `Ready`, `Degraded`, `Unknown` |
| `status.observedGeneration` | controller | `metadata.generation` last reconciled. |
| `status.conditions` | controller | KEP-1623 condition list. |
| `status.bindingConfigMapRef.name` | controller | Name of the binding ConfigMap. The router-side informer (S7) watches by label selector. |
| `status.versionHash` | controller | Hex-encoded sha256 prefix of the compiled binding JSON. |
| `status.lastReconciledAt` | controller | RFC 3339 timestamp of last reconcile. |

### Conditions emitted

| Type | Reasons | Description |
|---|---|---|
| `Ready` | `AsExpected`, `BindingWriteFailed` | Binding ConfigMap publication health. |
| `Progressing` | `Reconciling` | Active reconcile. |
| `Degraded` | `BindingWriteFailed` | Binding ConfigMap write failed. |

### Lifecycle

```
Pending → Ready (binding ConfigMap published)
Ready → Degraded (ConfigMap write failure)
Degraded → Ready (retry)
[ClawMemory deleted] → finalizer cleans up binding ConfigMap
```

### Example

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawMemory
metadata:
  name: agent-memory
  namespace: default
spec:
  storeName: my-foundry-store
  sandboxRef:
    name: my-agent
  scope: agent:my-agent
  retentionDays: 90
  deleteOnSandboxDelete: true
  displayName: "Agent long-term memory"
```

---

## ClawEval

**Group/Version/Kind:** `azureclaw.azure.com/v1alpha1`, plural `clevaluations`,
kind `ClawEval`. Short name: `ceval`.

**Purpose:** Eval workflow declaration over a sandbox, binding to **Azure AI
Foundry Evals** or future suite adapters (`promptfoo`, `inspect-ai`). The
controller compiles the spec to a binding JSON ConfigMap
(`claweval-{name}-binding`). The runtime path reads the binding and triggers
Foundry Evals via the router proxy. The controller never calls Foundry directly
and never runs evals. Scheduled evals (via `spec.schedule`) are triggered by the
runtime's timer; one-shot evals are triggered by `azureclaw eval <name>`.

A sandbox may have multiple `ClawEval` CRs (one per suite or schedule).
The runtime indexes them by name.

**Field-manager split:** the controller owns `phase`, `observedGeneration`,
`conditions`, `bindingConfigMapRef`, `versionHash`, `lastReconciledAt`. The
runtime (field manager `azureclaw-router`) owns `lastRunAt`, `lastScore`,
`lastPass`, and the `EvalsPassed` condition.

### Spec fields

| Path | Type | Required | Default | Description |
|---|---|---|---|---|
| `spec.sandboxRef.name` | string | **yes** | — | Sandbox name. CEL: 1–253 chars. |
| `spec.suite` | enum | **yes** | `foundry-evals` | Eval suite. Values: `foundry-evals` (runtime path wired), `promptfoo` (reserved), `inspect-ai` (reserved). |
| `spec.evaluators` | []string | conditional | `[]` | Foundry evaluator IDs (e.g., `relevance`, `coherence`, `fluency`). Required when `suite == "foundry-evals"` (CEL). Each entry 1–256 chars. |
| `spec.model` | string | no | sandbox primary | Model identifier the runtime evaluates against. |
| `spec.schedule` | string | no | — | Cron schedule (5 or 6 space-separated tokens). Absent = manual-trigger only. CEL validates token count. |
| `spec.threshold.score` | float64 | no | — | Pass/fail threshold in `[0.0, 1.0]` (CEL). |
| `spec.threshold.op` | enum | no | `Gte` | Comparison operator: `Gte` (`score >= threshold`) or `Gt` (`score > threshold`). |
| `spec.regressionAction` | enum | no | `Suspend` | Action when eval fails threshold. Values: `Suspend` (sets `ClawSandbox.spec.suspend=true` via S7), `None` (record only). |
| `spec.dataset.configMapRef.name` | string | conditional | — | ConfigMap containing JSONL eval cases under key `dataset.jsonl`. Mutually exclusive with `inline` (CEL). |
| `spec.dataset.inline` | []object | conditional | `[]` | Inline eval cases (free-form JSON objects). Capped at 64 entries by CEL. |
| `spec.displayName` | string | no | — | Human-readable label. CEL: 1–256 chars when set. |

### Validation (CEL)

| Rule | Message | Reason |
|---|---|---|
| `size(spec.sandboxRef.name) > 0 && size(spec.sandboxRef.name) <= 253` | `spec.sandboxRef.name must be 1-253 characters` | `FieldValueInvalid` |
| `spec.suite != 'foundry-evals' \|\| (has(spec.evaluators) && size(spec.evaluators) >= 1)` | `spec.evaluators must contain at least one entry when spec.suite is 'foundry-evals'` | `FieldValueInvalid` |
| Each evaluator entry 1–256 chars | `each spec.evaluators entry must be 1-256 characters` | `FieldValueInvalid` |
| Schedule is 5 or 6 cron tokens (when set) | `spec.schedule, when set, must be a 5-or-6-field cron expression (1-256 chars)` | `FieldValueInvalid` |
| `spec.threshold.score` in `[0.0, 1.0]` (when set) | `spec.threshold.score must be in [0.0, 1.0] when set` | `FieldValueInvalid` |
| `spec.dataset.configMapRef` and `spec.dataset.inline` are mutually exclusive | `spec.dataset.configMapRef and spec.dataset.inline are mutually exclusive` | `FieldValueInvalid` |
| `size(spec.dataset.inline) <= 64` | `spec.dataset.inline is capped at 64 entries; use a ConfigMap for larger datasets` | `FieldValueInvalid` |
| `displayName` 1–256 chars (when set) | `spec.displayName, when set, must be 1-256 characters` | `FieldValueInvalid` |

### Status fields

| Path | Owner | Description |
|---|---|---|
| `status.phase` | controller | `Pending`, `Ready`, `Degraded`, `Unknown` |
| `status.observedGeneration` | controller | `metadata.generation` last reconciled. |
| `status.conditions` | controller | KEP-1623 condition list. |
| `status.bindingConfigMapRef.name` | controller | Binding ConfigMap name. |
| `status.versionHash` | controller | Hex-encoded sha256 prefix of compiled binding JSON. |
| `status.lastReconciledAt` | controller | RFC 3339 timestamp of last controller reconcile. |
| `status.lastRunAt` | **runtime** | RFC 3339 timestamp of last completed eval run. Written by `azureclaw-router`. |
| `status.lastScore` | **runtime** | Score of the last completed run; range `[0.0, 1.0]`. Written by `azureclaw-router`. |
| `status.lastPass` | **runtime** | Whether the last run passed `spec.threshold`. Written by `azureclaw-router`. |

### Conditions emitted

| Type | Owner | Reasons | Description |
|---|---|---|---|
| `Ready` | controller | `AsExpected`, `BindingWriteFailed` | Binding ConfigMap health. |
| `Progressing` | controller | `Reconciling` | Active controller reconcile. |
| `Degraded` | controller | `BindingWriteFailed` | Write failure. |
| `EvalsPassed` | **runtime** | `ThresholdMet`, `ThresholdNotMet`, `NoThreshold` | Written by `azureclaw-router` after each run. |

### Lifecycle

```
Pending → Ready (binding ConfigMap published)
Ready → Degraded (ConfigMap write failure)
Degraded → Ready (retry)

After each eval run (runtime-side):
  status.lastRunAt, status.lastScore, status.lastPass updated
  EvalsPassed condition patched via SSA by azureclaw-router
  If regressionAction=Suspend and lastPass=false: ClawSandbox.spec.suspend=true (S7)
```

### Example

```yaml
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawEval
metadata:
  name: nightly-quality-check
  namespace: default
spec:
  sandboxRef:
    name: my-agent
  suite: foundry-evals
  evaluators:
    - relevance
    - coherence
    - fluency
  model: gpt-4o
  schedule: "0 2 * * *"
  threshold:
    score: 0.8
    op: Gte
  regressionAction: Suspend
  dataset:
    inline:
      - { input: "Summarise this document.", expected: "A concise summary." }
  displayName: "Nightly quality regression check"
```

---

## Cross-cutting admission policies

AzureClaw ships seven `ValidatingAdmissionPolicy` (VAP) and one
`MutatingAdmissionPolicy` (MAP) resources that apply cluster-wide
independently of the CRD reconcilers. All require Kubernetes 1.30+
(VAP GA). The MAP (`seccomp-auto-stamp`) additionally requires K8s 1.34+
and is disabled by default.

### `azureclaw-content-safety-floor`

**File:** `deploy/helm/azureclaw/templates/admission-content-safety-floor.yaml`

**Enabled by:** `admission.contentSafetyFloor.enabled: true` (Helm value).

**Applies to:** `InferencePolicy` CREATE/UPDATE.

**What it rejects:** Any `InferencePolicy` that sets a
`spec.contentSafety.{hate,selfHarm,sexual,violence}` severity **more permissive**
than the configured cluster minimum
(`admission.contentSafetyFloor.minimum`, default `Medium`). Severity
ordinal: `Safe (0) < Low (1) < Medium (2) < High (3)`. Lower ordinal = stricter
floor. "High" means only the worst abuse is blocked; setting it above the cluster
minimum loosens the cluster's posture and is denied.

**Bypass:** Label the `InferencePolicy`
`azureclaw.azure.com/dev-only: "true"` to opt out.

### `azureclaw-dev-only-label-immutable`

**File:** `deploy/helm/azureclaw/templates/admission-dev-only-label-immutable.yaml`

**Always enabled** (no Helm guard).

**Applies to:** `ClawSandbox`, `McpServer`, `ToolPolicy` UPDATE.

**What it rejects:** Any update that removes
`azureclaw.azure.com/dev-only: "true"` from an object that previously
carried it, **unless** the new object carries the annotation
`azureclaw.azure.com/dev-only-removal-reason` with a non-empty value. This
provides an auditable break-glass: label removal is logged at the API server
audit layer.

### `azureclaw-no-public-router-exposure`

**File:** `deploy/helm/azureclaw/templates/admission-no-public-router-exposure.yaml`

**Always enabled** (no Helm guard). **No break-glass** — this is the hardest
invariant in the platform.

**Applies to:** Services, Ingresses, NetworkPolicies, HTTPRoutes, TLSRoutes,
TCPRoutes in namespaces labelled `azureclaw.azure.com/isolated: strict`.

**What it rejects:**
- `Service` of type `LoadBalancer` or `NodePort`.
- Any `Ingress`, `HTTPRoute`, `TLSRoute`, or `TCPRoute` object.
- `NetworkPolicy` with ingress `ipBlock.cidr: 0.0.0.0/0` or `::/0`.

Public A2A traffic must go through the dedicated `azureclaw-a2a-gateway`
component in its own namespace. See [ADR-0001](../adr/0001-a2a-ingress-front-edge.md).

### `azureclaw-null-provider-block`

**File:** `deploy/helm/azureclaw/templates/admission-null-provider.yaml`

**Enabled by:** `admission.nullProviderBlock.enabled: true` (Helm value).

**Applies to:** `ClawSandbox`, `McpServer`, `ToolPolicy` CREATE/UPDATE.

**What it rejects:** Any manifest whose `spec.*.provider` or
`spec.agt.providers.*` is one of `null`, `noop`, `disabled`, `none`.
These are the values the controller historically accepts as "no provider
configured".

**Bypass:** Label the object `azureclaw.azure.com/dev-only: "true"`.

### `azureclaw-sandbox-exec-ban`

**File:** `deploy/helm/azureclaw/templates/admission-pod-exec-ban.yaml`

**Enabled by:** `admission.podExecBan.enabled: true` (Helm value).

**Applies to:** Pod CONNECT (exec/attach) subresource requests in namespaces
labelled `azureclaw.azure.com/isolated: strict`.

**What it rejects:** `kubectl exec` and `kubectl attach` targeting the
`openclaw` agent runtime container. Operator exec bypasses every in-pod
hardening layer (seccomp, egress-guard, Landlock, Entra token scope).
No break-glass.

### `azureclaw-sandbox-posture-lock`

**File:** `deploy/helm/azureclaw/templates/admission-sandbox-posture-lock.yaml`

**Enabled by:** `admission.sandboxPostureLock.enabled: true` (Helm value).

**Applies to:** Pod UPDATE in sandbox namespaces (`azureclaw.azure.com/isolated: strict`).

**What it rejects:** Any pod UPDATE that:
- Sets `privileged: true` on any container.
- Sets `allowPrivilegeEscalation: true`.
- Flips `readOnlyRootFilesystem` from `true` to `false`.
- Drops `runAsNonRoot` from `true` to `false`.
- Removes `seccompProfile` or sets `type: Unconfined`.
- Adds `ephemeralContainers` (the canonical pod-exec escape hatch).

Enforces defense-in-depth against a compromised workload identity or
controller bug that attempts to relax sandbox posture.

### `azureclaw-seccomp-auto-stamp` (MAP)

**File:** `deploy/helm/azureclaw/templates/admission-seccomp-auto-stamp.yaml`

**Enabled by:** `admission.seccompAutoStamp.enabled: true`. Defaults to `false`
because it requires the `MutatingAdmissionPolicy` feature gate (K8s 1.34+, beta,
not enabled by default in AKS yet).

**Applies to:** Pod CREATE in sandbox namespaces (excludes `kata*` runtimeClassName).

**What it does:** Auto-stamps the `azureclaw-strict.json` seccomp profile
(`type: Localhost, localhostProfile: azureclaw-strict.json`) onto pods that are
missing it. Defense-in-depth: guarantees the strict seccomp profile lands even if
the controller or a future adapter forgets to set it.

---

*This document covers all 8 AzureClaw CRDs as of Phase 2.5. For conditions
vocabulary see also [docs/api/conditions.md](conditions.md). For CLI commands that
create and manage these resources see [docs/cli-reference.md](../cli-reference.md).*
