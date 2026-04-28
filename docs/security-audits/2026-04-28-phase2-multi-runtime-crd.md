# 2026-04-28 — Phase 2 / S10.A1 — Multi-runtime CRD spine

**Slice branch:** `phase2-multi-runtime-crd`
**Plan refs:** `plan.md` §S10 (RESHAPED 2026-04-28b) → S10.A1 row
**Doc cross-refs:**
- `docs/competitive.md` §14.6 column 11 (Multi-runtime hosting)
- `docs/competitive.md` §15.2 #9 (redefined: multi-runtime hosting first wave)
- `docs/implementation-plan.md` §8 item 5 (superseded framing)

## Scope

In-place `v1alpha1` schema edit migrating `ClawSandbox.spec.openclaw` to
`ClawSandbox.spec.runtime.{kind, openclaw|openaiAgents|microsoftAgentFramework|semanticKernel|langGraph|anthropic|byo}`,
plus the runtime-aware status surface (`status.runtimeKind` field +
`RuntimeReady` Condition) and the explicit reconciler dispatch guard that
**refuses** to create a Pod for a runtime kind whose adapter has not yet
shipped (S10.A3/A4 territory). Tier-2 placeholders (`SemanticKernel`,
`LangGraph`, `Anthropic`) lock the wire shape now so the public CRD
serves as a roadmap signal and adding the adapter image later is not a
breaking change. No new container image; no new RBAC; no new networking.
Pure CRD + reconciler-dispatch + status-vocabulary slice.

## Threat model addressed

| Threat | Mitigation in this slice |
|---|---|
| **Silent runtime fallthrough** — operator declares `runtime.kind: OpenAIAgents` (or any Tier-2 placeholder), controller silently runs the `ctx.sandbox_image` (OpenClaw) image while reporting Degraded. The agent's actual code never runs but the cluster appears to host an OpenAI Agents workload. Customer reasoning about the security posture is broken. | Reconciler maps `RuntimeKind` to a static-str discriminator and **explicitly skips** namespace/SA/Deployment creation when the kind is not `OpenClaw`. Stamps `Degraded=True / Ready=False / RuntimeReady=False` all with `Reason=AdapterMissing`, requeues every 5 min, returns before any K8s-resource builder is invoked (`controller/src/reconciler/mod.rs:222-260`). Same code path covers Tier-1 variants pending adapter (S10.A3/A4) and Tier-2 placeholders (SemanticKernel, LangGraph, Anthropic). |
| **Status churn / kube-apiserver throttling** — every reconcile bumps `resourceVersion` regardless of byte-equality on the patched object, which re-triggers reconcile, which re-patches → observed Phase 1 storm of 7 reconciles in 12s with concomitant Graph API throttling. Adding a separate `RuntimeReady` stamp call would re-introduce this. | `RuntimeReady` Condition + `runtimeKind` field land **inside** `build_running_status_patch` and `build_overlay_status_patch`, never via a separate `patch_status`. The `*_status_matches` idempotency guards extended to compare `runtime_kind` AND the new Condition; controller short-circuits when settled. Plan §S10.A1 rubber-duck #1. |
| **CEL-disabled apiservers** | Defended primarily by CEL `XValidation` rules in the helm CRD (4 bidirectional `(self.kind=='X') == has(self.x)` + nested AgentCodeRef exactly-one). A controller-side `validate_runtime_shape` defensive guard is **deferred to S10.A2** — accepted residual risk: a CR with conflicting variants on a CEL-disabled cluster would currently parse as the first variant present and ignore the rest. Acceptable for a pre-release alpha; tracked in plan §S10.A1 rubber-duck #7. |
| **BYO contract bypass** — operator supplies an arbitrary image as `byo`; controller has no basis to assume UID 1000, no-privileged-ports, etc. | `byo.contractVersion` is a **required** field with no default (plan §S10.A1 rubber-duck #8); `RuntimeReady` Condition surface reserves `RouterBypassRisk` / `UnsupportedSecurityContext` reasons for the strict-mode admission webhook in S10.A2. This slice is warn-only — image is not yet deployed at all in A1 (deferred to A2). |
| **Convert tool emits non-OpenClaw → upstream Sandbox** — `azureclaw convert` (Phase 0/S9.2) round-trips between AzureClaw and `sigs/agent-sandbox`. There is no upstream shape for OpenAIAgents/MAF/BYO. | `cli/src/commands/convert.ts` hard-fails with a clear error message when `runtime.kind != "OpenClaw"` is converted toward upstream. Golden tests `cli/src/commands/convert.test.ts:376-377, 610-611` updated to verify the error path. Plan §S10.A1 rubber-duck #10. |

## Existing implementation surveyed

Per `plan.md` §0.2 "no duplication, no dead code" — this slice extends the
following Phase 0/1 seams; nothing is re-implemented:

| Surveyed | Reused / extended in this slice |
|---|---|
| `controller/src/status/mod.rs` patch helpers (Phase 1 `2026-04-25-phase1-idempotent-status-patch.md`) | `build_running_status_patch` / `running_status_matches` / `build_overlay_status_patch` / `overlay_status_matches` get `runtime_kind: &str` trailing arg; new `build_runtime_unsupported_status_patch` / `runtime_unsupported_status_matches` / `stamp_runtime_unsupported` mirror the existing `degraded_*` helper trio. No second status-patch dispatcher. |
| `controller/src/status/conditions.rs` vocabulary (Phase 1 `2026-04-24-phase1-minimal-conditions.md`) | New constants `TYPE_RUNTIME_READY` and `reason::ADAPTER_MISSING` slot into the existing module; `new_condition` constructor untouched. |
| `controller/src/crd_validations.rs` (Phase 1 CEL helpers) | `RuntimeSpec` CEL written using the same `xValidations` shape used by `McpServer.spec.oauth` and `ToolPolicy.spec.commerce`. |
| `controller/src/reconciler/mod.rs:222-260` (sandbox dispatch entrypoint) | Runtime-kind discriminator inserted **before** any K8s-resource builder; preserves existing namespace/SA/NetPol/Deployment ordering for the `OpenClaw` path. |
| `cli/src/commands/convert.ts` translator (Phase 1 `2026-04-25-phase1-convert-translator.md`) | Reads `spec.runtime.openclaw` (was `spec.openclaw`); hard-fails on non-OpenClaw kinds toward upstream. |
| `cli/src/migrate/from_kagent.ts` (S9.3) | Emits `spec.runtime.openclaw` for kagent `python|go` runtimes (was `spec.openclaw`). |
| `mesh_peer/offload.rs` cloud-offload spawn (S22) | `OFFLOAD_*` extraEnv injection now targets `spec.runtime.openclaw.extraEnv`. No second offload spawner. |

## Wire-format invariants

1. `RuntimeKind` enum values (PascalCase): `OpenClaw`, `OpenAIAgents`,
   `MicrosoftAgentFramework`, `SemanticKernel`, `LangGraph`, `Anthropic`,
   `BYO`. CRD field names (camelCase): `openclaw`, `openaiAgents`,
   `microsoftAgentFramework`, `semanticKernel`, `langGraph`, `anthropic`,
   `byo`. CLI flags (kebab-case): `--runtime openai-agents`,
   `--runtime microsoft-agent-framework`, `--runtime semantic-kernel`,
   `--runtime langgraph`, `--runtime anthropic`, `--runtime byo`.
2. `status.runtimeKind` is `Option<String>`; absent on freshly-created
   CRs; populated on the first successful reconcile for the OpenClaw
   path or the first AdapterMissing stamp for non-OpenClaw kinds.
   Printer column `Runtime` reads this field — do not rename without a
   helm CRD bump.
3. `RuntimeReady` Condition is **always** present in the `conditions`
   array on a Sandbox that has been reconciled at least once. Status
   `True` ↔ reason `Reconciled` (running), status `False` ↔ reason
   `OverlayMode` (overlay path) **or** `AdapterMissing` (unsupported
   runtime). Future reasons (`RouterBypassRisk`,
   `UnsupportedSecurityContext`) are reserved for S10.A2 strict-mode
   admission and are **not** stamped in A1.
4. The conditions array is **fully replaced** under merge-patch — every
   patch helper that touches `.conditions` must carry `RuntimeReady`
   alongside `Ready` / `Progressing` / `Suspended` / `Degraded`. CI
   covers this via the round-trip `*_status_matches` tests; a new
   helper that emits a partial conditions array would silently erase
   `RuntimeReady` on the next reconcile.

## Test coverage

| Layer | Coverage added | File |
|---|---|---|
| CRD round-trip | 8 tests asserting each `RuntimeKind` variant deserialises in isolation; conflicting variants rejected | `controller/src/crd.rs` (added in commit `d11d41d`) |
| Helm drift | `helm_clawsandbox_crd_matches_rust_schema` extended to cover the new `runtime` block + `status.runtimeKind` + `Runtime` printer column | `controller/src/helm_drift.rs` |
| Status helpers | 5 new tests: `runtime_unsupported_patch_stamps_three_conditions_and_runtime_kind`, `runtime_unsupported_status_matches_rejects_when_status_missing`, `runtime_unsupported_status_matches_rejects_when_runtime_kind_differs`, `runtime_unsupported_status_matches_returns_true_for_settled_status`, `runtime_unsupported_patch_preserves_transition_time_on_repeat` | `controller/src/status/mod.rs` |
| Status helpers (existing, updated for new signatures) | `running_patch_emits_generation_and_ready_condition`, `running_status_matches_returns_true_for_settled_status`, `overlay_patch_emits_overlay_phase_and_three_conditions`, `overlay_status_matches_returns_true_for_settled_overlay_status` | same |
| CLI convert | Golden tests updated; non-OpenClaw → upstream hard-fail | `cli/src/commands/convert.test.ts` |
| Compat fixtures | `null-provider-prod-denied.yaml` migrated; reject-reason assertion strengthened to mention null-provider admission policy (plan §S10.A1 rubber-duck #9) | `tests/compat/fixtures/` |

Result: `cargo test --package azureclaw-controller` → **289 passed,
0 failed** (was 284). `cargo clippy --package azureclaw-controller
--all-targets -- -D warnings` → clean. `cargo fmt --all -- --check` →
clean.

## Out of scope (deferred)

- **S10.A2** — `RuntimeDeploymentPlan` per-variant dispatch seam in
  `controller/src/reconciler/runtime.rs`; per-variant image / entrypoint /
  env / `agentCode` resolution; BYO contract verifier with strict-mode
  admission; `validate_runtime_shape` defensive controller-side guard.
- **S10.A3** — OpenAI Agents runtime image + adapter + reference example.
- **S10.A4** — Microsoft Agent Framework runtime image + adapter +
  reference example. **This is the slice that flips §14.6 column 11.**
- **S10.A5** — `azureclaw add --runtime` CLI flags + `azureclaw status`
  display.

## Sign-offs

- [ ] Reviewer 1 — controller correctness + status churn invariant
- [ ] Reviewer 2 — CRD wire-format + breaking-change scope
