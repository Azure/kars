# Conditions Taxonomy — AzureClaw CRDs

Every AzureClaw CRD exposes a `status.conditions[]` array following the
[Kubernetes condition convention](https://kubernetes.io/docs/reference/using-api/api-concepts/#resource-versions).
Each condition carries `type`, `status` (`True`/`False`/`Unknown`),
`lastTransitionTime`, `reason`, `message`, and `observedGeneration`.

This document is the canonical reason taxonomy per CRD. CNCF
conformance criterion **C3** (`tests/cncf-conformance`) asserts that
every CRD declares a `conditions[]` array.

## Standard condition types

These types are used across CRDs. CRD-specific types are listed in
each CRD section.

| Type          | Meaning                                                       |
|---------------|---------------------------------------------------------------|
| `Ready`       | Object reached its desired state and is serving its purpose. |
| `Progressing` | Reconciler is making forward progress toward the spec.       |
| `Degraded`    | Object is partially functional but a sub-resource is failing.|

`status: True` means the type predicate is true. For `Degraded`,
that means the object **is** degraded; for `Ready`, that means it
**is** ready.

## ClawSandbox

| Type          | Reason values                                                  |
|---------------|----------------------------------------------------------------|
| `Ready`       | `SandboxRunning`, `WaitingForRuntime`, `RuntimeFailed`         |
| `Progressing` | `Reconciling`, `BuildingDeployment`, `WaitingForRouter`        |
| `Degraded`    | `RouterCrashLooping`, `EgressGuardFailed`, `NetpolDriftDetected`|

## ClawPairing

| Type          | Reason values                                                  |
|---------------|----------------------------------------------------------------|
| `Ready`       | `PairingActive`, `PairingPending`, `PairingExpired`, `PairingRevoked` |
| `Progressing` | `WaitingForPeer`, `RotatingTokenHash`                          |
| `Degraded`    | `BudgetExceeded`, `SlotsExhausted`                             |

## A2AAgent

| Type          | Reason values                                                  |
|---------------|----------------------------------------------------------------|
| `Ready`       | `AgentRegistered`, `AgentUnreachable`                          |
| `Progressing` | `Discovering`, `WaitingForCard`                                |
| `Degraded`    | `CardSchemaInvalid`, `EndpointUnauthorized`                    |

## McpServer

| Type          | Reason values                                                  |
|---------------|----------------------------------------------------------------|
| `Ready`       | `ServerHealthy`, `ServerUnreachable`                           |
| `Progressing` | `Resolving`, `WaitingForOAuth`                                 |
| `Degraded`    | `ToolListEmpty`, `OAuthTokenExpired`                           |

## ToolPolicy

| Type          | Reason values                                                  |
|---------------|----------------------------------------------------------------|
| `Ready`       | `PolicyActive`                                                  |
| `Progressing` | `Compiling`                                                     |
| `Degraded`    | `RuleConflict`, `UnknownTool`                                   |

## InferencePolicy

| Type          | Reason values                                                  |
|---------------|----------------------------------------------------------------|
| `Ready`       | `PolicyActive`                                                  |
| `Progressing` | `Resolving`                                                     |
| `Degraded`    | `BudgetMisconfigured`, `ContentSafetyDisabled`                  |

## ClawEval

| Type          | Reason values                                                  |
|---------------|----------------------------------------------------------------|
| `Ready`       | `EvalCompleted`, `EvalRunning`                                  |
| `Progressing` | `Scheduled`, `Sampling`                                         |
| `Degraded`    | `JudgeUnreachable`, `DatasetMissing`                            |

## ClawMemory

| Type          | Reason values                                                  |
|---------------|----------------------------------------------------------------|
| `Ready`       | `MemoryReady`                                                   |
| `Progressing` | `Indexing`, `Backfilling`                                       |
| `Degraded`    | `StoreUnreachable`, `EmbeddingFailed`                           |

## TrustGraph

| Type          | Reason values                                                  |
|---------------|----------------------------------------------------------------|
| `Ready`       | `GraphActive`, `GraphEmpty`                                     |
| `Progressing` | `Resolving`, `RollupComputing`                                  |
| `Degraded`    | `PeerUnverified`, `RegistryUnreachable`                         |

## Adding a new reason

1. Pick the smallest noun phrase in PascalCase that names the failure.
2. Add it to the table above in the same PR that emits it.
3. Reasons are part of the CRD's public surface — treat them like
   API symbols. Do not rename without a deprecation alias.
