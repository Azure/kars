# Conditions Taxonomy — AzureClaw CRDs

Every AzureClaw CRD exposes a `status.conditions[]` array following the
[Kubernetes condition convention](https://kubernetes.io/docs/reference/using-api/api-concepts/#resource-versions).
Each condition carries `type`, `status` (`True`/`False`/`Unknown`),
`lastTransitionTime`, `reason`, `message`, and `observedGeneration`.

This document lists the reasons the controller actually emits today.
CNCF conformance criterion **C3** (`tests/cncf-conformance`) asserts
that every CRD declares a `conditions[]` array. Reasons are part of
each CRD's public surface; we do not rename without a deprecation
alias.

## Standard condition types

These types are reused across CRDs.

| Type          | Meaning                                                          |
|---------------|------------------------------------------------------------------|
| `Ready`       | Object reached its desired state and is serving its purpose.    |
| `Progressing` | Reconciler is making forward progress toward the spec.          |
| `Degraded`    | Object is partially functional but a sub-resource is failing.   |
| `Suspended`   | Operator has paused the object via `spec.suspended: true`.      |

`status: True` means the type predicate holds. For `Degraded`, that
means the object **is** degraded; for `Ready`, that it **is** ready.

## Shared `reason` values (`controller/src/status/conditions.rs`)

| Reason | Used on | Meaning |
|---|---|---|
| `Reconciling` / `Reconciled` | most kinds | Generic progress markers. |
| `Creating` / `Created` | most kinds | Reconciler is creating sub-resources / has just created them. |
| `Failed` | most kinds | Reconcile failed; see `message`. |
| `SpecInvalid` | most kinds | Spec failed semantic validation past the CRD schema. |
| `DependencyMissing` | most kinds | A referenced sibling CR is absent. |
| `TimedOut` | most kinds | A wait loop hit its budget. |
| `SuspendedBySpec` | `ClawSandbox` | `spec.suspended: true`; Deployment scaled to 0. |
| `Active` | `ClawSandbox` | Pairs with `Suspended=False` to clear a prior `SuspendedBySpec`. |

## ClawSandbox

The sandbox carries a richer condition set because it owns the
end-to-end runtime.

| Type | `status` | Reasons emitted |
|---|---|---|
| `Ready` | True/False | `Created`, `Reconciled`, `SuspendedBySpec`, `Failed`, `AdapterMissing`, `OverlayMode`, `InferencePolicyNotFound`, `ToolPolicyNotFound`, `AwaitingFoundryProvisioning`, `AwaitingRouterEnforcement`, `RouterEnforcing` |
| `Progressing` | True/False | `Reconciling`, `Creating` |
| `Degraded` | True/False | `AuthMisconfigured`, `MemoryStoreMissing`, `FailedClosed` |
| `Suspended` | True/False | `SuspendedBySpec`, `Active` |
| `RuntimeReady` (S10) | True/False | `AdapterMissing` (Falsey when the runtime adapter isn't wired) |
| `AllowlistVerified` (S12.b) | True/False | `Verified`, `Unsigned`, `FailedClosed` |
| `AllowlistAuthoritative` (S12.e) | True/False | `Inline`, `Verified`, `StaleLKG`, `FailedClosed`, `InlineDiffersFromArtifact` |
| `AllowlistDrift` (S12.e) | True/False | `InlineDiffersFromArtifact`, `InlineCleared` |

Plus Warning events (not conditions) for soft deprecations:
`McpSingularDeprecated` — emitted on a CR that still uses
`spec.governance.mcpServerRef` instead of `mcpServerRefs[]`.

## A2AAgent

| Type | Reasons |
|---|---|
| `Ready` | `Reconciled`, `SpecInvalid`, `Failed` |
| `Progressing` | `Reconciling`, `Creating` |

`status.versionHash` + `status.lastCompiledAt` reflect the last
successful AgentCard compile. `status.agentCardConfigMapRef` points
at the ConfigMap mounted into the inference-router for outbound
calls.

## McpServer

| Type | Reasons |
|---|---|
| `Ready` | `Reconciled`, `SpecInvalid`, `DependencyMissing` (JWKS fetch failure when `productionMode: true`), `Failed` |
| `Progressing` | `Reconciling` |

`Ready=True` means the controller successfully mirrored OAuth metadata
and JWKS into the per-sandbox `mcp/<name>/` directories. **It does not
prove the upstream MCP host is reachable** — health probing is a v1.1
item.

## ToolPolicy

| Type | Reasons |
|---|---|
| `Ready` | `RouterEnforcing` (data-plane echoed back the compiled AGT-profile digest), `NoSandboxesReferencing` (compiled but no consumer), `Reconciled`, `SpecInvalid` |
| `Progressing` | `AwaitingRouterEnforcement` (the router has not echoed the digest yet), `Reconciling` |
| `Degraded` | `Failed` |

The `Ready ⇔ router echo` invariant is fully wired
on this CRD. A `ToolPolicy` is `Ready=True / RouterEnforcing` only when
at least one referencing sandbox's router has confirmed the loaded
profile digest matches what the controller compiled.

## InferencePolicy

| Type | Reasons |
|---|---|
| `Ready` | `RouterEnforcing`, `NoSandboxesReferencing`, `Reconciled`, `SpecInvalid` |
| `Progressing` | `AwaitingRouterEnforcement`, `Reconciling` |
| `Degraded` | `Failed` |

Same wire contract as `ToolPolicy`. Per-request token budgets are
enforced today; aggregate (`dailyTokens` / `monthlyTokens`) is
accepted and surfaced but not yet aggregated — see
[`docs/roadmap.md`](../roadmap.md#v11--topology--signed-crd-upgrades).

## ClawMemory

| Type | Reasons |
|---|---|
| `Ready` | `RouterEnforcing` (binding loaded by router), `AwaitingFoundryProvisioning` (Memory Store not yet present upstream — common until first runtime sync), `Reconciled`, `SpecInvalid` |
| `Progressing` | `AwaitingRouterEnforcement`, `Reconciling` |
| `Degraded` | `AuthMisconfigured` — router observed an upstream 403 from Foundry; check the project-MI role assignment (see `azureclaw-deployment` skill). `MemoryStoreMissing` — router observed an upstream 404; the store has not been provisioned yet. Precedence: `AuthMisconfigured` outranks `MemoryStoreMissing`. |

## ClawEval

`ClawEval` has its own taxonomy in `controller/src/claw_eval.rs`.

| Type | Reasons |
|---|---|
| `Ready` | `Reconciled`, `CorpusResolved`, `Scheduled`, `RunTriggered`, `RunReportRead`, `AllPassed` |
| `Progressing` | `Reconciling`, `Scheduled` |
| `Degraded` | `SpecInvalid`, `TargetSandboxMissing`, `TargetSandboxNotReady`, `CorpusFetchFailed`, `CorpusParseFailed`, `CorpusBuiltinMissing`, `RunReportParseFailed` |
| `ConformanceDrift` | `DriftDetected` — at least one corpus case failed on the last run. When `spec.failSandboxOnDrift: true`, the target `ClawSandbox` is also patched `Degraded=True / EvalDrift`. |

## TrustGraph

| Type | Reasons |
|---|---|
| `Ready` | `Reconciled` — graph parsed, edge signatures verified, projection ConfigMap written. |
| `Progressing` | `Reconciling` |
| `Degraded` | `SpecInvalid`, `Failed` |

`status.validEdges` / `invalidEdges` count what passed and what was
dropped; failing edge IDs are surfaced in controller logs (the count
is also surfaced in events). v1alpha1 is reconciler-only — Ready does
**not** today imply the router consumes the projection (KNOCK still
uses an in-router score map). See the [`TrustGraph` CRD entry](crd-reference.md#trustgraph--mesh-trust-topology).

## EgressApproval

| Type | Reasons |
|---|---|
| `Ready` | `RouterConfirmed` — overlay merged into the sandbox's allowlist and the router echoed back the merged digest. |
| `Progressing` | `AwaitingRouterEcho` — controller compiled the overlay but router has not yet POSTed back the loaded digest. |
| `Degraded` | `BlockedOnSandbox` — the referenced `ClawSandbox` is not Ready; `TtlExceedsCeiling`, `TtlInvalid`, `ReasonInvalid` — admission-time validation failures retained for visibility; `Expired` — TTL exceeded, file removed (terminal). |

Phase transitions: `Pending → Active → Expired`. See
[Network egress & proxy](../egress-proxy.md) for the full lifecycle.

## Adding a new reason

1. Pick the smallest noun phrase in `PascalCase` that names the failure.
2. Add the constant to `controller/src/status/conditions.rs` (or the CRD-specific module) and add it to the table above in the same PR.
3. Reasons are part of the CRD's public surface — treat them like API symbols. Do not rename without a deprecation alias.
