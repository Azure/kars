# `sigs/agent-sandbox` compatibility mode — design

**Status:** Phase 0 design doc (no code, no CI dependency).
**Scope:** Optional opt-in compatibility layer for clusters running the
`kubernetes-sigs/agent-sandbox` upstream CRDs. No upstream dependency, no CI
pin.
**Plan reference:** internal Phase 1 plan §2 + §6 item 14.
**Source of upstream schema pinned in this doc:**
<https://github.com/kubernetes-sigs/agent-sandbox/blob/main/api/v1alpha1/sandbox_types.go>
(fetched 2026-04-24; unpinned — we explicitly *do not* track a specific
upstream commit while AzureClaw is closed-source).

---

## 1. Why this doc exists

`kubernetes-sigs/agent-sandbox` is establishing an opinionated upstream
schema for sandboxed AI-agent pods in K8s. We want operators who already
author `agents.x-k8s.io/v1alpha1 Sandbox` manifests to be able to run
them on AzureClaw *without rewriting YAML* — **but** we do not want a
dependency on the upstream project's release cadence, CRD definition
freshness, or community coordination surface while AzureClaw itself is
closed-source (plan §0.2 #5).

**Goal:** ship the capability to interop, keep the door open for future
upstream alignment, but carry zero build-time or CI-time dependency on
the upstream project today.

**Non-goal:** become a drop-in replacement for the upstream controller,
file KEPs, pin upstream CRD YAML in our CI, or block AzureClaw releases
on upstream merges.

## 2. The upstream schema in one table

(Pinned snapshot; this doc is manually refreshed when we decide to.)

| Field | Shape | Semantics |
|---|---|---|
| `apiVersion` | `agents.x-k8s.io/v1alpha1` | |
| `kind` | `Sandbox` | Namespace-scoped. |
| `spec.podTemplate.spec` | `corev1.PodSpec` | Required. Full K8s PodSpec. |
| `spec.podTemplate.metadata.{labels,annotations}` | `map[string]string` | Labels propagated per `SandboxPropagatedLabelsAnnotation`. |
| `spec.volumeClaimTemplates[]` | `[]PersistentVolumeClaimTemplate` | PVCs the pod references. |
| `spec.shutdownTime` | `metav1.Time` | Expiry wall-clock. |
| `spec.shutdownPolicy` | `Delete \| Retain` (default `Retain`) | Governs CR deletion on expiry; underlying pods always deleted. |
| `spec.replicas` | `0` or `1` (default `1`) | Scalable subresource. |
| `status.serviceFQDN` | `string` | DNS name for the headless Service. |
| `status.service` | `string` | Service name. |
| `status.conditions[]` | `[]metav1.Condition` | `Ready` condition primary. |
| `status.replicas` | `int32` | Actual replicas. |
| `status.selector` | `string` | Scale subresource. |
| `status.podIPs[]` | `[]string` | Dual-stack aware. |

Key annotations the upstream controller uses:

- `agents.x-k8s.io/pod-name` — pod adopted from warm pool.
- `agents.x-k8s.io/sandbox-template-ref` — template reference.
- `agents.x-k8s.io/sandbox-pod-template-hash` — pod template hash.

### 2.1. What the upstream schema does **not** model

The upstream `Sandbox` is deliberately a narrow primitive: "one sandboxed
pod, optionally scaled to 0, with lifecycle". It does **not** describe:

- Inter-agent mesh membership (KNOCK / Signal / AgentMesh).
- Tool governance / policy decisions.
- A2A agent-card publication.
- AP2 commerce caps.
- Model-routing / inference budget policy.
- Confidential runtime attestation surface.
- AzureClaw's router sidecar, egress-guard, Content Safety integration.
- SSA field-manager contract across multiple controllers.

All of the above live in AzureClaw. That's the "overlay" layer.

## 3. Three modes

The `ClawSandbox` resource gains one optional field in Phase 1:

```yaml
spec:
  upstreamCompatibility: Native | Translate | Overlay   # default Native
```

### 3.1. `Native` (default, unchanged)

Exactly today's behaviour. Our controller reconciles `ClawSandbox`
directly to Deployment + Service + NetworkPolicy + ConfigMap in the
tenant namespace. No upstream CR involved. No change to any existing
`azureclaw dev` / `up` / `add` / `handoff` / `offload` flow.

### 3.2. `Translate` (Phase 1, opt-in)

Our controller **emits** an upstream-shaped `Sandbox` CR as an owned
subresource of `ClawSandbox`, and only reconciles the overlay
(router sidecar, identity bindings, policy bindings, mesh membership)
directly.

Two sub-modes depending on cluster inventory:

- **Translate + upstream-owned pod:** the upstream-sigs controller is
  installed and owns the `Sandbox` CR. Our controller owns the overlay
  (a second Deployment or sidecar injected via our Mutating Admission
  Policy) plus the `ClawSandbox` CR itself. Pod itself is owned by
  upstream; we never set an `ownerReference` on the pod.
- **Translate + vendored-upstream reconcile:** upstream CRD is installed
  but no external controller is present. Our controller **also**
  reconciles the upstream `Sandbox` CR (field manager:
  `azureclaw-controller/upstream-sandbox`) by implementing the upstream
  schema semantics in-repo. This is a *vendored reimplementation*, not
  a fork — we copy behaviour semantics from the pinned snapshot; we do
  not pull their Go code.

The Phase 1 security audit doc for `Translate` captures: (a) ownership
diagram, (b) SSA field-manager partition, (c) which mode is which
(upstream-owned vs vendored reconcile) and how we detect it.

### 3.3. `Overlay` (Phase 2, opt-in)

Operator already has a `Sandbox` CR owned by some other controller
(upstream, a third-party, or hand-rolled). `ClawSandbox.spec.sandboxRef`
points at it by name + namespace. Our controller does **not** touch the
referenced `Sandbox`; it only creates overlay resources keyed off the
existing pod's labels/selectors.

### 3.4. Ownership + SSA matrix

| Object | `Native` | `Translate` (upstream-owned) | `Translate` (vendored) | `Overlay` |
|---|---|---|---|---|
| `ClawSandbox` CR | us | us | us | us |
| Upstream `Sandbox` CR | (absent) | us (emit-only) | us (full reconcile) | external |
| Pod | us | external (upstream ctrl) | us (field mgr `.../upstream-sandbox`) | external |
| Router sidecar | us (container in pod) | us (injected via MAP) | us (container in pod) | us (injected via MAP) |
| Headless Service | us | external | us | external |
| NetworkPolicy | us (`.../reconciler`) | us (`.../overlay`) | us (`.../reconciler`) | us (`.../overlay`) |
| ConfigMap (agent cfg) | us | us | us | us |
| Secret (credentials) | us | us | us | us |

CI invariant (Phase 1 e2e): no pod has two competing controllerRef
owners; no resource has two SSA field managers writing the same field.

## 4. Translation rules (`ClawSandbox → Sandbox`)

Normative mapping table — Phase 1 implementation must match this, Phase
0 only documents it. Field diffs in plan §4.1 "target module layout"
(`controller/src/compat/sigs_sandbox.rs`).

| `ClawSandbox` field | Upstream `Sandbox` field | Notes |
|---|---|---|
| `metadata.namespace` | `metadata.namespace` | same |
| `metadata.name` | `metadata.name` | same; reserve the `-sandbox` suffix for a future if collision risk |
| `spec.openclaw.image` | `spec.podTemplate.spec.containers[0].image` | primary container |
| `spec.openclaw.extraEnv` | `spec.podTemplate.spec.containers[0].env` | projected |
| `spec.sandbox.isolation` | translated into `runtimeClassName` + seccomp profile | `strict` → `azureclaw-strict` |
| `spec.inference.endpoint` | N/A | lives on overlay router ConfigMap only |
| `spec.sandbox.resources` | `spec.podTemplate.spec.containers[0].resources` | pass-through |
| `spec.sandbox.volumes` | `spec.volumeClaimTemplates` + `spec.podTemplate.spec.volumes` | split |
| `spec.sandbox.expiry` | `spec.shutdownTime` | same wall-clock |
| `spec.sandbox.onExpiry: delete\|retain` | `spec.shutdownPolicy` | direct map |
| `spec.scale` | `spec.replicas` | clamped to `{0,1}` |
| (no analog) | `metadata.annotations[agents.x-k8s.io/...]` | added by us for compat; operators must not depend on them |

Inverse (`Sandbox → ClawSandbox`) is provided by `kubectl azureclaw
convert` (§5). The inverse is lossy — AzureClaw fields with no upstream
analog (mesh, policy, inference budget) default to `disabled` (dev-only
label applied automatically) and the CLI warns the user to re-configure.

## 5. `kubectl azureclaw convert`

CLI subcommand. Phase 0 ships a skeleton with `--help`; Phase 2 ships
the real translator.

```
azureclaw convert -f sandbox.yaml --to clawsandbox
azureclaw convert -f clawsandbox.yaml --to upstream-sandbox
azureclaw convert -f clawsandbox.yaml --to overlay --sandbox-ref=<ns/name>
```

- No cluster connectivity required.
- Idempotent over repeated invocations.
- Emits a diff preamble comment so reviewers see what was lossy.

## 6. Explicitly-not-doing list

- **No `go.mod` dependency on `kubernetes-sigs/agent-sandbox`.** The
  upstream schema snapshot is a manually curated table in this doc plus
  (Phase 1) a Rust-side type definition mirroring the snapshot. No
  generated types, no `controller-gen` hook.
- **No CI pin of upstream CRD YAML.** The upstream CRD can appear in a
  customer cluster at any version; our translator emits a YAML that
  *passes* upstream-v1alpha1 schema validation as of the pinned
  snapshot. If upstream evolves, our translator lags — that's a feature,
  not a bug.
- **No upstream PRs, issues, or KEPs by us.** Re-evaluate if / when
  AzureClaw is open-sourced and leadership authorises community
  engagement.
- **No default flip.** `Native` remains the default forever unless
  leadership authorises a strategy change.

## 7. Risks + mitigations

See plan §2.4 — copied here for reference:

- **Schema duplication cost:** accepted; smaller than the flexibility
  of upstream-independent release cadence.
- **Upstream schema drift:** our translator is explicitly allowed to
  lag; translator consumers pin a documented compat snapshot.
- **Two controllers fight over pod ownership:** forbidden by §3.4
  ownership matrix + Phase 1 e2e CI invariant.
- **`agents.x-k8s.io` annotations become load-bearing:** documented as
  internal-only; removal in a later upstream version does not break us.

## 8. Phase gates

| Phase | Deliverable |
|---|---|
| 0 (this doc) | Design review; `kubectl azureclaw convert` skeleton (separate branch). |
| 1 | `spec.upstreamCompatibility: Translate` behind feature flag; translator in `controller/src/compat/sigs_sandbox.rs`; compat-suite test `translate_roundtrip.spec.ts`; security audit. |
| 2 | `Overlay` mode; `kubectl azureclaw convert` full implementation; migration command `azureclaw migrate to-translate-mode`. |
| 3+ | Re-evaluate at OSS time. |

## 9. Non-engagement declaration

As of 2026-04-24, AzureClaw maintainers do not file issues, PRs, or
design docs against `kubernetes-sigs/agent-sandbox`. We observe the
upstream schema via public GitHub reads only. This declaration is
recorded here because CI does not enforce it; any future change to this
posture requires leadership sign-off.
