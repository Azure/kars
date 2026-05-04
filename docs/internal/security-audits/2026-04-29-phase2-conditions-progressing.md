# Phase 2 — S7.B: Conditions matrix `Progressing` emission

**Date:** 2026-04-29
**Slice:** `phase2-conditions-ssa-leader-b` (sub-slice S7.B of S7 craftsmanship train)
**Author:** AzureClaw maintainers
**Sign-offs:** `@maintainer-1`, `@maintainer-2`

## Scope

Close the `Progressing` Condition gap in the `ClawSandbox` status-patch
builders so the controller writes a uniform Conditions matrix on every
reconcile. Specifically:

- `build_running_status_patch` now stamps `Progressing=False / Reconciled`
  alongside the existing `Ready=True` and `RuntimeReady=True`.
- `build_degraded_status_patch` now stamps `Progressing=False / <degraded-reason>`
  alongside `Degraded=True` and `Ready=False`.
- `build_runtime_unsupported_status_patch` now stamps
  `Progressing=False / AdapterMissing` alongside `Degraded=True`,
  `Ready=False`, and `RuntimeReady=False`.
- The corresponding idempotency guards (`running_status_matches`,
  `runtime_unsupported_status_matches`) verify the new condition so a
  pre-S7.B status (Ready+RuntimeReady only) is treated as stale and
  back-filled on the next reconcile rather than masked as a no-op.

Reason: KEP-1623 §Conditions and operator UX both expect that every
condition type the controller writes is present on every reconcile so
`kubectl wait --for=condition=Progressing=False` resolves consistently
across success / overlay / degraded / adapter-missing paths. Pre-S7.B,
the running and degraded paths emitted Conditions arrays of length 2,
while the overlay path (S8) emitted the full four-condition matrix.

## Out of scope

- **Mid-reconcile `Progressing=True` step emissions** between the
  Namespace → ServiceAccount → FederatedCredential → NetworkPolicy →
  ConfigMap → Deployment → Service reconcile steps. Threading those
  emissions through `reconciler/mod.rs` would add a status-write per
  step on every reconcile and churn `resourceVersion`. Deferred to
  S7.B.2 if and when operator demand warrants it. The current sub-slice
  emits `Progressing=False` on success/degraded/adapter-missing only,
  which already unblocks the `kubectl wait` use case.
- Step-reason vocabulary constants (`STEP_NAMESPACE`, `STEP_DEPLOYMENT`,
  …). They are only useful in concert with mid-reconcile emission, so
  they ship together in S7.B.2 to avoid dead vocabulary.
- All other reconcilers (McpServer, ToolPolicy, A2AAgent, InferencePolicy,
  ClawMemory, ClawEval, MeshPeer, ClawPairing). The 6 dedicated CRD
  reconcilers from S1–S6 already emit `Progressing` on their happy paths
  (verified 2026-04-29 by source survey). The two helpers that don't —
  `pairing_reconciler` and `mesh_peer/*` — operate over child resources
  with their own Conditions semantics and are intentionally outside this
  sub-slice's scope. Audited as part of S7.B.3 if needed.
- Leader election (S7.C), backoff/jitter (S7.D), workqueue metrics
  (S7.E), VAP/MAP expansion (S7.F).

## Hard-rule checklist (`docs/implementation-plan.md` §0.2)

| # | Rule | Status |
|---|------|--------|
| 1 | No fork; no upstream re-implementation | ✓ — pure controller-internal change |
| 3 | No file grew past Phase 2 cap | ✓ — `controller/src/status/mod.rs` 1064 → 1146 (no cap; not in §4.2 budgeted list) |
| 8 | No custom-crypto / framing | ✓ — N/A |
| 9 | Audit doc with two sign-offs | ✓ — this doc |
| 10 | Verify, don't guess; cite sources | ✓ — KEP-1623 §Conditions |

## Test coverage

- All 5 pre-existing tests covering the three patch builders + their
  idempotency guards (`running_patch_emits_generation_and_ready_condition`,
  `running_status_matches_returns_true_for_settled_status`,
  `degraded_patch_stamps_degraded_true_and_ready_false`,
  `runtime_unsupported_patch_stamps_three_conditions_and_runtime_kind`,
  `runtime_unsupported_status_matches_returns_true_for_settled_status`)
  updated to assert the new Progressing condition shape (status, reason,
  observedGeneration).
- New regression test
  `running_status_matches_returns_false_when_progressing_missing` proves
  that a pre-S7.B status (Ready+RuntimeReady only) is treated as stale
  by the idempotency guard so the new condition is back-filled on the
  next reconcile after upgrade. Without this guard the controller would
  short-circuit reconciliation and the Progressing field would never
  appear on existing CRs after a controller bump.
- All 329 controller bin tests pass (was 328; +1 for the new guard).
- Clippy `-D warnings` clean. `cargo fmt --check` clean.

## Threat model

No new attack surface. The change is metadata-only on a status subresource
the controller already owns via SSA. Idempotency guards extend; they do
not relax. Backwards compat for upgrade-time status flap is the new
regression test's reason for being.

## Existing implementation surveyed

- `controller/src/status/mod.rs` `build_running_status_patch` /
  `build_degraded_status_patch` / `build_runtime_unsupported_status_patch`
  (the three patch builders this slice extends).
- `controller/src/status/mod.rs` `build_overlay_status_patch` (the
  reference implementation that already emits the full matrix; this
  slice brings the other three paths to parity).
- `controller/src/status/conditions.rs` `TYPE_PROGRESSING`, `RECONCILED`,
  `ADAPTER_MISSING`, `SPEC_INVALID`, `preserve_transition_time`
  (helpers reused — no new helpers introduced).
- `controller/src/reconciler/mod.rs` lines 873-915 (egress-guard /
  Deployment build site — not modified; mid-reconcile threading
  deferred to S7.B.2).

No new module created. No code duplicated. No dead code carried.

## §14.6 / §15 impact

- §14.6 column 12 (Governance as K8s primitives): improved — operator
  observability of every CRD now shipped as K8s primitive is uniform.
- §10.4 #11 (Conditions matrix completeness): one of the §9 P0
  craftsmanship items the S7 train is paying off; S7.B closes the
  ClawSandbox-side gap. McpServer / ToolPolicy / A2AAgent / etc.
  reconcilers already match parity per 2026-04-29 source survey.
- §15.2 #10: incremental progress.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
