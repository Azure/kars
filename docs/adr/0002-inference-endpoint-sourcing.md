# ADR 0002: Inference endpoint sourcing — cluster-wide via env vars; no per-sandbox CR override

**Status:** Accepted
**Date:** 2026-05-05
**Deciders:** Pal Lakatos-Toth, Copilot (drafter)
**Context PRs:** #226 (`fix(refs,migrate): ToolPolicy.appliesTo uses sandboxMatchLabels`), #227 (`fix(refs): drop spurious InferencePolicy.modelPreference.primary.endpoint`)
**Supersedes:** —
**Superseded by:** —

## Context

The inference router is a per-sandbox proxy that fronts every model call
made by the OpenClaw agent. It needs to know **where** to send the
request — the Azure OpenAI account URL, the Foundry Models API URL, and
(for the project-scoped APIs like Memory / Agents / Indexes) the Foundry
project endpoint URL.

Historically the CLI had a code path in `cli/src/refs.ts:buildInferencePolicy`
that injected an `endpoint` field under
`InferencePolicy.spec.modelPreference.primary.endpoint`, sourcing the
URL from the `--openai-endpoint` flag of `azureclaw up`. The field was
**not** present in the InferencePolicy CRD schema and **was not read**
by either the controller or the router — it was anticipatory scaffolding
for a per-sandbox endpoint override that was never wired through.

When the InferencePolicy CRD was put into strict-decoding mode, this
spurious field became a hard `kubectl apply` reject, breaking
`azureclaw up` at sandbox bring-up:

```
strict decoding error: unknown field "spec.modelPreference.primary.endpoint"
```

We had to decide: re-introduce the override path properly, or strip
the field and codify endpoint sourcing as a single cluster-wide
mechanism.

## Decision

**The inference router gets all endpoint URLs from environment variables
injected by the controller from helm values. There is no per-sandbox
endpoint override.**

The data path:

```
helm values
  inferenceRouter.azure.openai.endpoint → env AZURE_OPENAI_ENDPOINT
  foundry.endpoint                       → env FOUNDRY_ENDPOINT
  foundry.projectEndpoint                → env FOUNDRY_PROJECT_ENDPOINT
                ↓
  controller-deployment.yaml passes them through
  reconciler reads them at boot (controller/src/reconciler/mod.rs)
                ↓
  every spawned router pod inherits the same env (via controller spawn)
                ↓
  router reads at startup: inference-router/src/config.rs:86,92
                ↓
  every inference route uses:
    azure_openai_endpoint   .or(foundry_endpoint)
  (inference-router/src/routes/inference.rs:441,501,572,573)
```

The single per-sandbox routing knob that **does** exist is
`InferencePolicy.spec.modelPreference.primary.deployment` — the
deployment name (e.g. `gpt-5.4`) within the cluster-wide AOAI / Foundry
account. That dimension is fully wired end-to-end (CRD schema →
controller compile → router lookup).

## Forward direction

- `foundry_endpoint` is the canonical/forward concept — it routes through
  the Foundry Models API gateway and is required for project-scoped APIs
  (Memory, Agents, Indexes).
- `azure_openai_endpoint` is **explicitly labeled "legacy fallback"** in
  `inference-router/src/config.rs:44`. It exists so customers with
  AOAI-only deployments (no Foundry project) can still point the router
  somewhere. The route handlers chain `azure_openai_endpoint` first, then
  fall back to `foundry_endpoint` — this is for backward compatibility
  with the pre-S13 deployment shape, not architectural preference.

When AOAI-only deployments are no longer in scope, the chain should
flip (or the legacy field should be removed). That's a future ADR.

## Consequences

### Positive

- **Single source of truth.** Every router pod in a cluster talks to the
  same AOAI / Foundry account. Operators don't have to look in N
  different CRs to know "what model account is this cluster pointed at".
- **No drift between sandbox CR and reality.** Strictly env-driven means
  there is no field a user can edit in a CR that silently doesn't take
  effect.
- **Simple controller logic.** Reconciler propagates a fixed env block
  to every spawned router pod from a single ConfigMap-equivalent. No
  per-sandbox env materialization.
- **Strict-decoding compatible.** No CR fields that lack a corresponding
  reader. `kubectl apply` stays trustworthy.

### Negative / accepted trade-offs

- **No per-sandbox endpoint override** out of the box. Use cases that
  need this — multi-region routing, BYO AOAI per tenant, dev-vs-prod
  routing on a shared cluster — are not addressable today without a
  proper feature (CRD field + controller wire-up + router reader).
- **Helm value changes require a cluster-wide rollout**, not a per-CR
  patch. Acceptable: endpoint changes are rare and operator-scoped.

### Mitigations for the over-ride use case

If a customer demand for per-sandbox override surfaces, **do not**
re-introduce the `endpoint` field on `modelPreference.primary` without
also building the controller propagation and router reader. The
correct vertical slice is:

1. Add `endpointOverride: { type: string, format: uri }` to the
   InferencePolicy CRD schema (`crd-inferencepolicy.yaml`).
2. Have the controller's reconciler read the override from the
   sandbox's `inferenceRef` target and inject it as a sandbox-scoped
   env var (e.g. `AZURE_OPENAI_ENDPOINT_OVERRIDE`) on that specific
   router pod's PodSpec.
3. Have the router's config layer prefer the override env var over the
   cluster default (`config.rs`).
4. Add unit tests on each side AND a CRD-schema validation test that
   round-trips a CR with the override through the schema (so this
   regression cannot happen again).

Half-built versions of this — schema-only, or builder-only, or any one
side without the others — are forbidden by this ADR.

## Implementation notes

This ADR codifies what the code already does after #227. No code
changes are required; the ADR exists to document the contract so future
contributors don't re-introduce the dead field.

The deeper meta-fix — adding CRD-OpenAPI schema validation to
`npm test` so spurious-field bugs are caught at PR time, not on a live
cluster — is tracked separately as a post-launch CI hardening task.
