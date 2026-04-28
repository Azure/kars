# Security Audit — Phase 2 / S10.A2 multi-runtime dispatch seam

**Date:** 2026-04-28
**Slice:** S10.A2 `phase2-multi-runtime-dispatch`
**Branch:** `phase2-multi-runtime-dispatch`
**Layer (per `docs/internal/phase-2-story.md` §2):** Layer 1 — Runtime
**§14.6 column moved:** Column 11 (multi-runtime hosting) — *no change yet; A2 is the seam, A3+A4 flip the column.*

---

## 1. Scope

This slice is a **structural seam only**. It introduces
`controller/src/reconciler/runtime.rs` — a `RuntimeDeploymentPlan`
producer/dispatcher — and routes the existing reconciler through it
**without changing observable behavior**.

In:
- New module `controller::reconciler::runtime` with `RuntimeDeploymentPlan`
  struct, `RuntimePlanError` enum (`AdapterMissing`, `ShapeInvalid`),
  `validate_runtime_shape()` defensive guard, `build_runtime_plan()`
  dispatcher, `plan_openclaw()` + `plan_byo()` producers (BYO is
  unit-tested but unwired — see §6).
- Reconciler dispatch site `mod.rs:222-275` rewired to call
  `runtime::build_runtime_plan(&runtime_spec, &ctx.sandbox_image)`.
- Deployment builder consumes `plan.image` (was inline image fallback)
  and `plan.runtime_extra_env` (was `openclaw_config.extra_env`).
- New `RuntimePlanError::ShapeInvalid` → `Degraded / SpecInvalid` status
  path for CEL-disabled apiservers.

Out (deferred):
- A2.b: split deployment builder into shared scaffolding + per-runtime
  container builder; wire `plan_byo` into the deployment builder; ship
  registry-side `org.azureclaw.runtime.contract=v1` label check.
- A3 / A4: `plan_openai_agents` / `plan_microsoft_agent_framework`
  producers + adapter container builders.
- Mesh-peer offload path (`mesh_peer/offload.rs`): out of scope — it
  always offloads to OpenClaw and writes
  `spec.runtime.openclaw.extraEnv` directly; rerouting through the plan
  would add risk for no behavioral change.

---

## 2. Threat model

| Threat | Phase 1 / S10.A1 mitigation | What S10.A2 changes |
|---|---|---|
| Operator submits `kind: OpenAIAgents` to a controller that doesn't yet implement it; controller silently runs the OpenClaw image instead. | A1: explicit `AdapterMissing` skip + 300 s requeue, `RuntimeReady=False` Condition stamped. | Path moved into `build_runtime_plan` — but the same condition + same skip + same requeue interval. **Equivalent posture.** |
| Operator submits `kind: BYO` *with* `openclaw:` block populated (or vice-versa) on a CEL-disabled apiserver; controller proceeds with mismatched config. | A1: only the helm CEL rules guarded this; no controller-side validation. **Latent gap.** | A2: `validate_runtime_shape()` mirrors all 7 CEL rules in Rust; raises `ShapeInvalid` → `Degraded / SpecInvalid` Condition + 300 s requeue. **Net improvement.** |
| Adapter producer drifts from dispatcher on the wire-format kind string (e.g. `"OpenAiAgents"` vs `"OpenAIAgents"`); status patches and logs disagree. | A1: single inline match in reconciler — no drift surface. | A2: `kind_str()` is the single source of truth, called by both the dispatcher and the reconciler. `debug_assert_eq!(plan.kind_str, runtime_kind_str)` at the call site catches drift in debug builds + tests. |
| Reserved-prefix env-var smuggling via `runtime.openclaw.extraEnv` (e.g. `AZURECLAW_FOO`). | A1: reserved-prefix + NUL-byte + duplicate filter in deployment builder. | A2: filter preserved **byte-for-byte** in the deployment builder; producer hands over the raw map without sanitization (intentional — keeps filtering centralized at the rendering boundary). |
| Image silently sourced from `runtime.openclaw.image` even after S10.A2's image-resolution refactor. | A1: image resolution happened inline. | A2: deployment builder reads `plan.image` only; comment explicitly says do not re-derive. The producer is the *only* place that consults `runtime.openclaw.image`. |

### Residual risks

- **CEL fail-open is still possible** if both the apiserver has CEL
  disabled **and** the cluster has a mutating webhook that strips
  `validate_runtime_shape`'s reachable code path. The defensive guard
  closes the first half (the part the controller controls); the second
  half is operator-controlled. Plan §S10.A1 rubber-duck #7 acknowledged
  this and accepted it.
- **`plan_byo` is unit-tested but unwired** — it cannot affect production
  until A2.b lands the deployment-builder split. This was deliberate to
  keep the seam slice small. The unit tests still gate the producer
  shape so A2.b's wiring lands against a known contract.

---

## 3. Invariants (per `docs/internal/phase-2-story.md` §4)

| Invariant | How A2 preserves it |
|---|---|
| **§4.1 Runtime-agnostic governance** — no Layer-2 CRD dispatches on `runtime.kind`. | A2 only touches Layer 1. The plan is consumed in the deployment builder, not in any governance reconciler. |
| **§4.2 Controller-enforced posture** — UID 1000/1001, egress-guard init container, NetworkPolicy, WI binding, seccomp, reserved-prefix env filter — all controller-owned. | The full `securityContext`, init container, NetworkPolicy, SA, seccomp profile, and reserved-prefix env filter are all rendered after `build_runtime_plan` returns. The plan only narrows *what is run inside the agent container*; everything around it is unchanged. |
| **§4.3 Router as single gate.** | A2 makes no networking changes. Router URL injection, `INFERENCE_ROUTER_URL` env, sidecar config — all unchanged. |
| **§4.4 No custom crypto, no parallel implementations.** | A2 introduces no new crypto path. `validate_runtime_shape` is a pure Rust mirror of the helm CEL rules — it computes presence booleans, no cryptographic comparisons. The `kind_str` helper is the **single** source of truth for the runtime-kind string (replaces a duplicate inline match that S10.A1 had introduced). |

---

## 4. Test coverage matrix

| Concern | Test location | Test name |
|---|---|---|
| Wire-format kind string is PascalCase, stable across all 7 variants | `runtime.rs::tests` | `kind_str_emits_pascal_case_for_all_variants` |
| Well-formed OpenClaw spec validates | `runtime.rs::tests` | `validate_accepts_well_formed_openclaw` |
| Kind without matching variant struct rejected | `runtime.rs::tests` | `validate_rejects_kind_without_matching_variant_struct` |
| Extra variant struct (e.g. `kind=BYO` + `openclaw=Some`) rejected | `runtime.rs::tests` | `validate_rejects_extra_variant_struct` |
| Each Tier-2 kind without struct rejected (CEL parity) | `runtime.rs::tests` | `validate_rejects_each_tier2_kind_without_struct` |
| Each Tier-2 kind with correct struct accepted (CEL parity) | `runtime.rs::tests` | `validate_accepts_each_tier2_kind_with_correct_struct` |
| `runtime.openclaw.image` honored when set | `runtime.rs::tests` | `plan_openclaw_uses_image_from_config_when_set` |
| Falls back to controller default image when unset | `runtime.rs::tests` | `plan_openclaw_falls_back_to_default_image` |
| `extra_env` carried through verbatim | `runtime.rs::tests` | `plan_openclaw_carries_extra_env_through` |
| AdapterMissing raised for all 6 non-OpenClaw kinds (incl. BYO short-circuit) | `runtime.rs::tests` | `plan_returns_adapter_missing_for_each_non_openclaw_kind` |
| Shape-invalid input rejected before dispatch | `runtime.rs::tests` | `plan_rejects_shape_invalid_input_before_dispatch` |
| BYO producer carries image / command / args / contract version | `runtime.rs::tests` | `plan_byo_carries_image_command_args_and_contract_version` |
| BYO producer skips `valueFrom` env entries (kept for raw-EnvVar path in A2.b) | `runtime.rs::tests` | `plan_byo_skips_value_from_env_entries` |

**Result:** 306 / 306 controller tests pass (293 from S10.A1 + 12 new + 1 misc).

`cargo clippy --package azureclaw-controller --all-targets -- -D warnings` clean.

---

## 5. Reviewer's checklist (per phase-2-story §6)

1. **Layer:** Layer 1 (Runtime). The seam doesn't cross layers.
2. **§14.6 column:** Column 11 — *no flip yet; A2 is the structural prerequisite for A3 + A4 to flip it.*
3. **Invariants touched:** §4.4 directly (eliminates the duplicate `kind_str` match introduced inadvertently by S10.A1's inline dispatch). §4.1 / §4.2 / §4.3 unchanged but explicitly preserved (§3 above).
4. **Existing seam extended:** the S10.A1 reconciler `mod.rs:222-260` inline match. We did **not** parallel-implement: there is exactly one runtime-dispatch site, exactly one image resolver, exactly one extra-env consumer.

---

## 6. Deferred to S10.A2.b

- Split the deployment builder so the OpenClaw container construction
  (port 18789, `OPENCLAW_GATEWAY_TOKEN` secretKeyRef, `OPENCLAW_MODEL`
  env, `/sandbox` mount, AGT relay listener probes) and BYO container
  construction (custom image / command / args, `value: + valueFrom:`
  env, no port assumption) share the security context + router sidecar
  + volume scaffolding but produce different `containers[name=agent]`
  shapes.
- Wire `plan_byo` into the new BYO container builder.
- Ship registry-side `org.azureclaw.runtime.contract=v1` label check at
  image-pull time so a BYO image without the contract label gets
  `RuntimeReady=False / ContractMissing` at admission rather than at
  pod start.

A2.b is intentionally a separate slice so the seam in A2 lands without
the deployment-builder refactor in its diff. This keeps reviewers
focused on "is the dispatch shape right?" before "is the BYO container
right?"
