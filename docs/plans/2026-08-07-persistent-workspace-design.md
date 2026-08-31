# Kars Persistent Workspace Design

**Status:** Implemented in [PR #494](https://github.com/Azure/kars/pull/494); follow-up validation and observability are listed in Sections 13, 14, and 17.

**Date:** 2026-08-07

**Scope:** Runtime-agnostic per-sandbox persistent `/sandbox` storage, plus declarative OpenClaw workspace bootstrap files.
**Out of scope:** Feishu channel integration, message-triggered wake-up, cross-cluster volume migration, multi-replica RWX, online volume expansion.

## 1. Problem

Before this change, every `KarsSandbox` runtime mounted `/sandbox` from an `emptyDir` volume. Runtime state survived a container restart inside the same Pod but was lost when the Pod was recreated, the Deployment rolled out, or a suspended sandbox scaled from zero back to one.

The persistent volume is a platform-level capability shared by OpenClaw, Hermes, OpenAI Agents, Microsoft Agent Framework, LangGraph, Anthropic, PydanticAI, and BYO runtimes. It preserves only state the runtime writes below `/sandbox`; the controller cannot make framework state persistent when an adapter stores that state elsewhere or only in memory.

The support statement therefore has two levels:

- **Platform support:** all wired Kubernetes runtimes receive the same PVC at
  `/sandbox`, with the same readiness, Retain/Delete, recovery, and suspension
  semantics.
- **Framework-state support:** OpenClaw and Hermes have known runtime homes
  below `/sandbox`; other frameworks require their application/checkpointer to
  place recoverable state below `/sandbox` or in an external store.

Before this change, the Controller had no supported way to initialize runtime-owned files such as `SOUL.md` and `HEARTBEAT.md` from a declarative source, and OpenClaw's entrypoint rewrote Kars-provided `AGENTS.md`, `SOUL.md`, and `TOOLS.md` on every startup. The implementation now uses a typed workspace block and create-if-missing defaults; the unrelated unstructured `runtime.openclaw.config` field remains outside this feature.

## 2. Goals

1. Give every supported `KarsSandbox` runtime an optional, dedicated persistent volume mounted at `/sandbox`.
2. Preserve runtime state written below `/sandbox` across Pod recreation and scale-to-zero.
3. Let an operator initialize selected OpenClaw workspace Markdown files from a same-namespace ConfigMap.
4. Preserve user changes by default after the initial bootstrap.
5. Retain dynamically provisioned storage by default when a `KarsSandbox` is deleted.
6. Keep sensitive credentials in Kubernetes Secrets and outside workspace ConfigMaps.
7. Maintain backward compatibility: sandboxes without the new storage block continue using `emptyDir`.
8. Surface storage readiness and bootstrap failures through explicit status conditions.

## 3. Non-goals

The first version does not:

- add Feishu or other channel configuration;
- receive IM messages while a sandbox is scaled to zero;
- define a wake gateway or durable message queue;
- share one volume among multiple simultaneously running runtime replicas;
- migrate data between clusters, regions or storage classes;
- continuously reconcile file contents after bootstrap;
- use `runtime.openclaw.config` as an arbitrary pass-through to `openclaw.json`;
- provide backup, snapshot or disaster-recovery orchestration;
- persist `/tmp`;
- replace Foundry Memory Store or another external semantic-memory service.

## 4. Design principles

### 4.1 Separate desired configuration from mutable state

- The CR selects storage behavior and references declarative inputs.
- A ConfigMap contains non-sensitive bootstrap files.
- A Secret contains credentials.
- The PVC contains runtime-mutated state.

### 4.2 Fail closed on ambiguous storage

The Controller must not start a sandbox against an unexpected or incompatible claim. Invalid combinations fail admission where possible; missing claims and incompatible claim modes produce a non-running Deployment and an explicit condition.

### 4.3 Preserve data by default

Dynamically created claims default to `Retain`. Deleting the `KarsSandbox` removes workload resources but leaves the PVC. Destructive deletion requires an explicit `Delete` policy.

### 4.4 Bootstrap is initialization, not synchronization

The default `IfMissing` policy copies each managed file only when the destination does not exist. A ConfigMap update does not overwrite files already modified on the PVC.

## 5. API

The storage block is runtime-agnostic. The nested OpenClaw workspace block is intentionally runtime-specific because `SOUL.md`, `HEARTBEAT.md`, and related files are OpenClaw contracts, not portable Kars runtime contracts.

### 5.1 KarsSandbox storage

`KarsSandboxSpec` exposes the following optional block:

```yaml
apiVersion: kars.azure.com/v1alpha1
kind: KarsSandbox
metadata:
  name: teaching-agent
  namespace: kars-teaching-agent
spec:
  storage:
    workspace:
      size: 10Gi
      storageClassName: managed-csi
      accessModes:
        - ReadWriteOnce
      retainPolicy: Retain
  runtime:
    kind: OpenClaw
    openclaw:
      workspace:
        bootstrapConfigMapRef:
          name: teaching-agent-workspace
        overwritePolicy: IfMissing
  inferenceRef:
    name: teaching-agent-inference
```

Rust schema:

```rust
pub struct KarsSandboxSpec {
    // existing fields...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<SandboxStorageSpec>,
}

pub struct SandboxStorageSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceStorageSpec>,
}

pub struct WorkspaceStorageSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_claim: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_class_name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_modes: Vec<PersistentVolumeAccessMode>,

    #[serde(default)]
    pub retain_policy: WorkspaceRetainPolicy,
}

pub enum PersistentVolumeAccessMode {
    ReadWriteOnce,
    ReadWriteOncePod,
}

pub enum WorkspaceRetainPolicy {
    Retain,
    Delete,
}
```

Defaults for dynamically provisioned storage:

| Field | Default |
|---|---|
| `size` | `10Gi` |
| `accessModes` | `[ReadWriteOnce]` |
| `retainPolicy` | `Retain` |
| `storageClassName` | unset; use the cluster default StorageClass |
| generated claim name | `<sandbox-name>-workspace` |

When `spec.storage.workspace` is omitted, the Controller preserves the current `emptyDir` behavior.

### 5.2 Existing claims

Advanced users may provide a same-namespace claim:

```yaml
spec:
  storage:
    workspace:
      existingClaim: teaching-agent-imported-workspace
```

Rules:

- `existingClaim` is mutually exclusive with `size`, `storageClassName`, `accessModes`, and `retainPolicy`.
- The claim must exist in the sandbox namespace.
- The Controller never creates, resizes, mutates, adopts or deletes an existing claim.
- The existing claim must advertise `ReadWriteOnce` or `ReadWriteOncePod`.
- A Bound claim is required before the Deployment scales above zero.

### 5.3 OpenClaw workspace bootstrap

Extend `OpenClawConfig` with a typed workspace block rather than adding more unstructured fields to `config`:

```rust
pub struct OpenClawConfig {
    pub version: Option<String>,
    pub image: Option<String>,
    pub config: Option<serde_json::Value>,
    pub workspace: Option<OpenClawWorkspaceSpec>,
    pub extra_env: Option<BTreeMap<String, String>>,
}

pub struct OpenClawWorkspaceSpec {
    pub bootstrap_config_map_ref: Option<LocalObjectRef>,
    #[serde(default)]
    pub overwrite_policy: WorkspaceOverwritePolicy,
}

pub enum WorkspaceOverwritePolicy {
    IfMissing,
    Always,
}
```

Default `overwritePolicy` is `IfMissing`.

The reference is same-namespace only. Cross-namespace references are not allowed.

### 5.4 Allowed bootstrap files

The bootstrap ConfigMap may contain only these keys:

- `AGENTS.md`
- `SOUL.md`
- `HEARTBEAT.md`
- `TOOLS.md`
- `USER.md`

The following are explicitly forbidden:

- `MEMORY.md`
- `openclaw.json`
- credentials or token files;
- session database files;
- pairing or binding state;
- AgentMesh identity or prekeys;
- arbitrary paths, nested keys, symlinks or executable files.

`MEMORY.md` is excluded because OpenClaw and Kars mutate it during normal operation. Declaratively overwriting it risks erasing inbox state, Foundry discovery context and user memory.

### 5.5 ConfigMap example

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: teaching-agent-workspace
  namespace: kars-system          # same namespace as the KarsSandbox CR
data:
  SOUL.md: |
    # Soul

    You are a teaching assistant for the university's internal learning platform.
    Protect student data and cite sources when making factual claims.

  HEARTBEAT.md: |
    # Heartbeat

    - Check for unfinished tasks.
    - Check the AgentMesh inbox.
    - If there is no work, return HEARTBEAT_OK.

  AGENTS.md: |
    # Agent Instructions

    Follow the institution's teaching and privacy policies.
```

## 6. Admission and validation

The generated CRD and Helm CRD enforce these constraints with schema and CEL where possible:

1. `existingClaim` must not appear with dynamic provisioning fields.
2. `size` must parse as a positive Kubernetes quantity.
3. `accessModes` must contain one value and that value must be `ReadWriteOnce` or `ReadWriteOncePod` in v1.
4. `retainPolicy` must be `Retain` or `Delete`.
5. `runtime.openclaw.workspace` is valid only when `runtime.kind == OpenClaw`.
6. `bootstrapConfigMapRef.name` must be a valid Kubernetes object name.
7. `overwritePolicy` must be `IfMissing` or `Always`.
8. Unknown bootstrap file keys cause reconciliation failure; they are not silently ignored.

Dynamic workspace PVCs do not support `ReadWriteMany` in v1 because each `KarsSandbox` has one active runtime replica and the security model assumes a private per-agent filesystem.

## 7. Reconciliation

### 7.1 Dynamic PVC creation

For a dynamic workspace, the Controller creates a PVC named `<sandbox-name>-workspace` in the sandbox namespace.

Required metadata:

```yaml
metadata:
  labels:
    app.kubernetes.io/managed-by: kars-controller
    kars.azure.com/sandbox: teaching-agent
    kars.azure.com/storage-role: workspace
  annotations:
    kars.azure.com/retain-policy: Retain
    kars.azure.com/sandbox-uid: <KarsSandbox metadata.uid>
```

The source `KarsSandbox` and generated PVC live in different namespaces, so a
namespaced owner reference would be invalid. Both policies use strict management
labels plus a `kars.azure.com/sandbox-uid` provenance annotation.

Any PVC in the generated namespace prevents direct namespace cascading. On
deletion, the controller only deletes a PVC when the current CR still declares
a dynamically generated `retainPolicy: Delete` workspace and the claim name,
management labels, storage-role label, and sandbox UID all match. It then
requeues until the claim is gone. `Retain`, `existingClaim`, and omitted storage
never authorize PVC deletion, even if a claim carries stale annotations.

### 7.2 Immutable fields and drift

The Controller reconciles desired PVC shape without attempting invalid in-place mutations.

- Increasing `size` may be applied only when the StorageClass allows expansion.
- Decreasing `size` is rejected.
- Changing `storageClassName` is rejected after creation.
- Changing `accessModes` is rejected after creation.
- Changing from a generated claim to `existingClaim`, the reverse, or changing existing claim names requires `spec.suspended: true`; active transitions fail closed with `StorageReady=False/ImmutableFieldChanged`.

These errors set `StorageReady=False` and leave the last-known-good Deployment and claim untouched.

### 7.3 Deployment volume

When storage is enabled:

```yaml
volumes:
  - name: sandbox-data
    persistentVolumeClaim:
      claimName: teaching-agent-workspace
```

The runtime container continues mounting:

```yaml
volumeMounts:
  - name: sandbox-data
    mountPath: /sandbox
```

`/tmp` remains a memory-backed `emptyDir`.

PVC-backed or bootstrap-enabled Deployments use the `Recreate` strategy. This
prevents two runtime Pods from concurrently writing a ReadWriteOnce workspace
and avoids rolling-update multi-attach deadlocks.

The inference-router does not receive access to the workspace PVC unless a separately reviewed feature requires it. The least-privilege boundary remains unchanged.

### 7.4 Bootstrap ConfigMap mount

When a bootstrap ConfigMap is configured, mount it read-only at:

```text
/etc/kars/workspace-bootstrap
```

Do not mount the ConfigMap directly over the writable OpenClaw workspace.

### 7.5 Bootstrap init container

The controller adds an init container before the runtime starts. It mounts:

- `sandbox-data` at `/sandbox`;
- the bootstrap ConfigMap at `/etc/kars/workspace-bootstrap`, read-only.

Its responsibilities are:

1. Create `/sandbox/.openclaw/workspace` with UID/GID `1000:1000`.
2. Validate that every source entry is an allowed regular file.
3. Reject symlinks and path traversal.
4. Copy via a temporary file in the destination directory.
5. Set file ownership to `1000:1000` and mode `0640`.
6. Atomically rename the temporary file to the destination.
7. Under `IfMissing`, skip existing destinations.
8. Under `Always`, replace allowed destination files.
9. Write a non-sensitive manifest to `/sandbox/.kars/bootstrap-state.json` containing ConfigMap UID, resourceVersion, policy, filenames and SHA-256 digests.

The init container must not log file contents.

The Pod disables automatic service-account-token mounting. A projected
Kubernetes token is mounted explicitly into the runtime and router containers,
while `egress-guard` and `workspace-bootstrap` receive neither that token nor an
Azure Workload Identity token.

### 7.6 Default Kars templates

If no bootstrap ConfigMap is configured, the existing Kars default `AGENTS.md`, `SOUL.md`, and `TOOLS.md` remain available, but entrypoint behavior changes from unconditional overwrite to create-if-missing.

This preserves current first-run behavior while preventing Pod restarts from replacing user edits on a PVC.

`HEARTBEAT.md` has no Kars default and is not created unless supplied by the operator or OpenClaw itself.

### 7.7 Suspended sandboxes

When `spec.suspended: true`:

- the Deployment remains at zero replicas;
- the PVC and bootstrap ConfigMap reference remain reconciled;
- no init container runs until the sandbox resumes;
- `StorageReady` can still become true based on PVC state;
- resuming mounts the same claim and restores runtime state.

The CRD documentation must stop describing state preservation for sandboxes that still use `emptyDir`. The guarantee applies only when persistent workspace storage is configured.

## 8. Deletion semantics

### 8.1 Retain

For `retainPolicy: Retain`:

1. Delete the Deployment and normal sandbox-owned resources.
2. Leave the PVC and backing PV intact.
3. Preserve the generated namespace because PVCs are namespaced resources.
4. Do not automatically expose the retained claim to another sandbox instance.

A future sandbox may use the retained data only by explicitly setting
`existingClaim`. Because the claim remains in `kars-<sandbox-name>`, recovery
normally recreates the same sandbox name.

### 8.2 Delete

For `retainPolicy: Delete`:

- the current CR spec must still explicitly request a dynamic Delete workspace;
- the controller verifies the generated claim's name, management labels,
  storage-role label, and sandbox UID before deleting it;
- the finalizer waits for the claim to disappear, then deletes the namespace;
- backing-volume deletion follows the StorageClass/PV reclaim policy.

The CLI must show a destructive warning before creating or updating a sandbox to `Delete`.

### 8.3 Namespace deletion

Kubernetes namespace deletion can delete both Retain and Delete claims. `retainPolicy: Retain` protects against deletion of the `KarsSandbox`, not deletion of its namespace. Documentation and CLI output must state this explicitly.

## 9. Status conditions

The controller adds a `StorageReady` condition to `KarsSandbox.status.conditions`.

| Status | Reason | Meaning |
|---|---|---|
| `True` | `EmptyDir` | Persistence not requested; current ephemeral behavior is active. |
| `True` | `ClaimBound` | Workspace PVC exists and is Bound. |
| `False` | `ClaimPending` | PVC exists but is not Bound. |
| `False` | `ClaimNotFound` | Referenced existing claim does not exist. |
| `False` | `ClaimIncompatible` | Access mode or claim shape is unsupported. |
| `False` | `ImmutableFieldChanged` | Requested storage mutation cannot be applied safely. |

`BootstrapReady` is a separate condition:

| Status | Reason | Meaning |
|---|---|---|
| `True` | `Reconciled` | The bootstrap init container completed successfully. |
| `False` | `Creating` | The bootstrap Pod/init container is pending. |
| `False` | `BootstrapConfigNotFound` | Referenced ConfigMap does not exist. |
| `False` | `BootstrapInvalid` | ConfigMap contains unsupported or unsafe entries. |
| `False` | `BootstrapFailed` | The init container terminated or crash-looped. |

`Ready=True` requires both storage and configured bootstrap initialization to be ready. The controller aggregates all active Pods with `Failed > Pending > Ready` precedence so an old successful Pod cannot hide a new failing rollout.

Kubernetes Events for storage/bootstrap transitions are not part of PR #494 and remain follow-up work.

## 10. Security

1. ConfigMap data is non-sensitive. Admission documentation prohibits secrets in workspace bootstrap files.
2. Credentials remain in `<sandbox-name>-credentials` and are injected through `envFrom` or mounted Secret files.
3. The init container runs with only the permissions needed to write the workspace volume; it receives no cloud credentials or service-account token. Existing Pod-level NetworkPolicy still governs the Pod network; the bootstrap script itself performs no network operations.
4. Bootstrap source and destination paths are fixed; user-controlled path fields are not supported.
5. ConfigMap projection symlinks are accepted as read-only sources; destination files and every writable destination/state directory component reject symlinks.
6. Atomic writes prevent partially initialized files.
7. The runtime remains UID 1000 with a read-only root filesystem.
8. PVCs are per sandbox and same namespace. Cross-namespace claim references are impossible.
9. A retained PVC is not automatically adopted based only on labels; adoption requires an explicit `existingClaim` name.
10. Backup encryption, StorageClass encryption and customer-managed keys remain operator responsibilities and must be documented.

## 11. CLI behavior

`kars add` exposes these storage flags:

```text
--workspace-storage <size>          Enable a generated workspace PVC, e.g. 10Gi
--workspace-storage-class <name>    Select a StorageClass
--workspace-existing-claim <name>   Use a pre-created same-namespace PVC
--workspace-retain-policy <policy>  Retain|Delete; default Retain
--workspace-bootstrap <configmap>   Initialize OpenClaw workspace files
--workspace-overwrite <policy>      IfMissing|Always; default IfMissing
```

Examples:

```bash
kars add teaching-agent \
  --workspace-storage 20Gi \
  --workspace-storage-class managed-csi \
  --workspace-bootstrap teaching-agent-workspace
```

```bash
kars add teaching-agent \
  --workspace-existing-claim teaching-agent-workspace
```

CLI output must show:

- claim name;
- storage class and requested size;
- retain policy;
- bootstrap ConfigMap and overwrite policy;
- a warning when persistence is omitted;
- a destructive warning for `retainPolicy=Delete`.

## 12. Backward compatibility and migration

### 12.1 Existing sandboxes

Existing CRs remain valid. When `spec.storage.workspace` is absent, the Controller continues producing `emptyDir`.

No automatic migration occurs because copying a live workspace requires coordination and can produce inconsistent sessions.

### 12.2 Opt-in migration from emptyDir

A safe migration procedure is:

1. Suspend the sandbox.
2. Create the desired PVC.
3. Copy data from a backup or an explicitly captured workspace archive into the claim.
4. Patch the CR to use `existingClaim` or dynamic storage.
5. Resume the sandbox.
6. Verify sessions, workspace files and AgentMesh identity.

Because `emptyDir` disappears when the Pod is scaled to zero, users must capture data before suspension. The CLI should eventually offer an export/import workflow, but it is outside this spec.

### 12.3 Entrypoint migration

Changing default files from always-overwrite to create-if-missing changes restart behavior intentionally. On the first persistent-storage rollout:

- existing `emptyDir` sandboxes still receive defaults on every new Pod because the directory starts empty;
- persistent sandboxes retain prior edits;
- `systemPromptOverride` remains the authoritative Kars security/welcome instruction unless a later spec explicitly exposes it.

## 13. Testing

### 13.1 CRD and schema tests

Verify:

- valid dynamic workspace storage is accepted;
- valid `existingClaim` is accepted;
- mixed existing/dynamic fields are rejected;
- invalid access modes are rejected;
- invalid retain and overwrite policies are rejected;
- OpenClaw workspace config is rejected for non-OpenClaw runtimes;
- old CRs without storage remain valid.

### 13.2 Controller unit tests

Verify generated resources for:

- omitted storage produces `emptyDir`;
- dynamic storage produces the expected PVC and claim mount;
- both policies omit cross-namespace owner references and record sandbox UID provenance;
- Delete removes only a strictly matched generated claim, then its namespace;
- Retain, existingClaim, and omitted storage preserve any namespace containing PVCs;
- existing claims do not produce a PVC object;
- bootstrap produces ConfigMap volume, mount and init container;
- Router does not mount the workspace claim;
- bootstrap init containers do not receive Kubernetes or Azure identity tokens;
- PVC/bootstrap Deployments use `Recreate`;
- valid pending existing claims remain `ClaimPending` with zero replicas;
- valid pending dynamic claims retain one consumer Pod for `WaitForFirstConsumer`;
- suspended sandboxes retain PVC reconciliation with replicas zero;
- immutable changes set the expected condition.

### 13.3 Bootstrap tests

Verify:

- `IfMissing` initializes absent files;
- `IfMissing` preserves modified files;
- `Always` replaces allowed files;
- forbidden filenames fail;
- symlink source and destination attacks fail;
- file contents never appear in logs;
- ownership and mode are correct;
- manifest digests match initialized files;
- an interrupted copy cannot leave a partial destination.

### 13.4 End-to-end tests

**Follow-up, not delivered by PR #494.** The PR includes controller/CLI unit
tests, Rust/Helm schema parity, API-server CRD dry-run, and executable bootstrap
filesystem tests. A CSI-backed lifecycle suite should additionally prove:

A local Kind test with a CSI-capable test provisioner, or a pre-created hostPath-backed PVC, must prove:

1. Create a sandbox with persistent workspace and bootstrap ConfigMap.
2. Wait for `StorageReady=True/ClaimBound` and `Ready=True`.
3. Verify `SOUL.md` and `HEARTBEAT.md` exist.
4. Modify `SOUL.md` and create representative session/workspace state through a legitimate runtime-facing flow.
5. Delete the Pod.
6. Verify the replacement Pod sees the modified file and state.
7. Set `spec.suspended=true`, wait for zero replicas, then resume.
8. Verify state remains.
9. Delete a Retain sandbox and verify the PVC remains.
10. Create a new sandbox with `existingClaim` and verify explicit recovery.
11. Delete a Delete-policy sandbox and verify its claim is removed.
12. Repeat the write/recreate/suspend sentinel flow for Hermes, LangGraph, and
  BYO to verify each adapter places recoverable state below `/sandbox`.

Do not use `kubectl exec` into the agent container in AKS E2E because the validating admission policy correctly blocks that path. Use an approved test runtime, init-container evidence, `kars connect`, or a purpose-built test probe.

## 14. Observability

**Follow-up, not delivered by PR #494.** Proposed metrics:

```text
kars_workspace_storage_reconcile_total{result,mode}
kars_workspace_storage_ready{sandbox,mode}
kars_workspace_storage_requested_bytes{sandbox}
kars_workspace_bootstrap_total{result,policy}
kars_workspace_bootstrap_files_total{result,policy}
```

Do not label metrics with PVC UID, ConfigMap content, user identifiers or filenames if that creates unbounded cardinality.

Log claim names, mode, condition reason and bootstrap resourceVersion. Never log workspace file contents or Secret values.

## 15. Documentation updates

The implementation updates:

- `docs/api/crd-reference.md` with storage and OpenClaw workspace fields;
- `docs/api/lifecycle.md` with PVC creation, suspension and deletion behavior;
- `docs/runtimes/CONTRACT.md` to distinguish ephemeral and persistent `/sandbox`;
- `docs/security.md` with storage trust boundary and encryption responsibilities;
- `docs/cli-reference.md` with new flags;
- `docs/getting-started.md` with persistence opt-in and cost warning;
- existing CRD comments that currently claim suspension restores state without qualifying storage mode.

## 16. Acceptance criteria

The implemented acceptance criteria are:

1. An old `KarsSandbox` without storage still runs with `emptyDir`.
2. A sandbox with dynamic storage gets a unique Bound PVC mounted at `/sandbox`.
3. Pod recreation and scale-to-zero preserve state that any runtime writes below `/sandbox`.
4. A same-namespace existing claim can be explicitly attached without Controller adoption or deletion.
5. Retain is the default and deleting the CR leaves the claim intact.
6. Delete is explicit, requires strict current-spec and PVC provenance checks, and removes the generated claim before namespace deletion.
7. A bootstrap ConfigMap initializes only the allowed files.
8. `IfMissing` preserves runtime/user edits across restart.
9. `Always` deterministically reapplies operator content.
10. `MEMORY.md`, credentials and runtime databases cannot be supplied through bootstrap.
11. Missing/incompatible claims and invalid bootstrap data prevent false `Ready=True` and expose actionable conditions.
12. No Secret or workspace content is emitted to logs or status.
13. Controller, CRD, CLI, and executable bootstrap tests cover the implemented behaviors; CSI-backed multi-runtime E2E remains tracked follow-up work.
14. Documentation no longer claims state survives suspension when `/sandbox` is ephemeral.

## 17. Future extensions

Potential follow-up specs may add:

- Feishu and other per-sandbox channel configuration;
- a wake gateway and durable message queue for scale-from-zero;
- volume snapshots, backup and restore;
- controlled workspace export/import;
- storage quota metrics and alerts;
- RWX and active/passive failover;
- per-file bootstrap policy;
- OCI-based signed workspace bundles;
- explicit migration jobs between claims or storage classes.
- runtime-specific bootstrap contracts for Hermes or other harnesses (rather
  than reusing OpenClaw Markdown semantics);
- a CSI-backed persistence matrix for Hermes, LangGraph, BYO, and the remaining
  shipping adapters;
- workspace controls in the operator TUI spawn dialog (the primary `kars up`
  and `kars add` CLI paths are implemented);
- workspace lifecycle Events and Prometheus metrics listed in Section 14.
