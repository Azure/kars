# CRD v1alpha2 + conversion webhook plan

> Status: **plan**, not yet implemented. Locked-in for v1.1.
> v1.0 ships v1alpha1 only; v1alpha1 is **frozen** for v1.0 (no
> breaking schema edits — only additive optional fields).

## Why v1alpha2 instead of v1beta1 or v1

CRD versioning conventions in Kubernetes:

- `v1alpha1` — early development, expect breaking changes.
- `v1alpha2`, `v1alpha3`, … — incremental refinements, still alpha,
  still expect breaks.
- `v1beta1` — feature-complete, API surface stable enough that
  downstream operators can build on it; minor breaks possible.
- `v1` — stable, no breaking changes without a new version.

AzureClaw v1.0 ships under `v1alpha1` because we are explicitly
holding the right to refactor schemas as the runtime adapter matrix
matures (BYO contract evolution, TrustGraph reload semantics, A2A
gateway tuning). Promoting to `v1beta1` requires a "no breaking
changes for one full release cycle" commitment we are not ready to
make.

## What v1alpha2 will contain

Field-level changes that are deferred from v1alpha1 because they
would have broken in-flight deployments:

1. **Renamed `runtime.byo.contractVersion` → `runtime.byo.contract.version`.**
   Currently a flat string; v1alpha2 promotes to a struct so we can
   add `runtime.byo.contract.healthChecks` and
   `runtime.byo.contract.signaturePolicy` as siblings without breaking
   the field path.

2. **`runtime.langGraph.nodeAdapter` enum.** v1alpha1 has an implicit
   "Python or TypeScript" via `language`; v1alpha2 makes the runtime
   container's adapter explicit so we can surface
   `nodeAdapter: typescript-experimental-edge` for an Edge runtime
   build later.

3. **`spec.governance.trustGraphRef`.** Currently a free-form string
   referencing a TrustGraph CR; v1alpha2 splits to
   `{ name, namespace, generation }` so the reconciler can pin to a
   specific generation rather than always reading `latest`.

4. **`status.conditions` with `observedGeneration` per-condition.**
   v1alpha1 carries a single top-level `observedGeneration`; v1alpha2
   stamps it per-condition so external controllers (e.g. a GitOps
   operator) can detect partial-staleness.

## Conversion strategy

A **declarative-only** conversion (no webhook required) is the goal —
all schema differences must be expressible via:

- Field renames with both names accepted (old kept as deprecated
  alias for one release).
- Default values that fill in missing fields.
- Restructure-without-breaking via `additionalProperties: x-preserve`
  patterns.

If we cannot express a change declaratively, we add a conversion
webhook with the structure below. Given the field list above we
**will** need a webhook for #1 (struct promotion) and #3 (string →
struct).

### Webhook architecture

```
┌─ kube-apiserver ──┐         ┌─ azureclaw-conversion-webhook ──┐
│                   │   POST  │                                  │
│  AdmissionReview  ├────────►│  /convert                        │
│  (kind=Conversion)│  TLS    │                                  │
│                   │◄────────┤  ConvertedObjects: ...           │
│                   │         │                                  │
└───────────────────┘         └──────────────────────────────────┘
                                       ▲
                                       │ shares core conversion logic
                                       │
                              ┌────────┴────────────┐
                              │ controller crate    │
                              │  conversion module  │
                              │  (pure functions)   │
                              └─────────────────────┘
```

**Constraints:**

- Webhook **must** be in-cluster (cert-manager-managed cert), reached
  by kube-apiserver via Service.
- Webhook **must** be a separate binary (not the controller) so it
  can be horizontally scaled and have a tighter SLO than the
  reconciler.
- Conversion must be **bidirectional** and **lossless** for fields
  that exist in both versions; v1alpha2 → v1alpha1 conversion drops
  v1alpha2-only fields (with a Warning condition).

### Implementation outline

Three Rust crates, all under the existing workspace:

1. `azureclaw-crd-types` — currently the home of CRD structs in
   `controller/src/crd.rs`. Splits to a separate crate that exports
   `v1alpha1::ClawSandbox` and `v1alpha2::ClawSandbox`.
2. `azureclaw-crd-conversion` — pure functions
   `v1alpha1_to_v1alpha2` and `v1alpha2_to_v1alpha1`. No I/O, no
   k8s imports — just struct-to-struct mapping. Tested with
   property-based tests.
3. `azureclaw-conversion-webhook` — thin axum binary that
   wraps the conversion functions, accepts `AdmissionReview` POSTs
   from kube-apiserver, and returns `ConversionResponse`.

### Helm wiring

```yaml
# deploy/helm/azureclaw/values.yaml additions
conversionWebhook:
  enabled: false  # opt-in for v1.1; v1.0 ships only v1alpha1
  image:
    repository: REPLACE_WITH_ACR/azureclaw-conversion-webhook
    tag: latest
  certManager:
    issuerRef:
      name: azureclaw-selfsigned-issuer
      kind: Issuer
      group: cert-manager.io
```

### Migration path for existing v1alpha1 users

When v1alpha2 ships:

1. Operator upgrades AzureClaw chart with `conversionWebhook.enabled=true`.
2. Webhook starts; both versions are now `served: true` in the CRD,
   with v1alpha1 marked `deprecated: true`.
3. Existing v1alpha1 CRs continue to work unchanged.
4. Operator runs `kubectl convert -f my-clawsandbox.yaml --output-version
   azureclaw.azure.com/v1alpha2 > new.yaml` to upgrade manifests.
5. Two releases later (v1.3), v1alpha1 is removed
   (`served: false`).

## Tracking

- v1.0: v1alpha1 frozen. Only **additive** optional fields allowed.
- v1.1: ship `azureclaw-crd-types` + `azureclaw-crd-conversion` crates;
  add `served: false` v1alpha2 schema (validation-only).
- v1.2: flip v1alpha2 to `served: true`; ship webhook.
- v1.3: deprecate v1alpha1; emit deprecation warnings on apply.
- v2.0: remove v1alpha1.

This document is the v1.1 milestone tracking page. Update with PR
links as the work lands.
