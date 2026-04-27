# Security Audit — phase1/minimal-conditions-audit

**Date:** 2026-04-25
**Branch:** `phase1/minimal-conditions-audit`
**Base:** `origin/dev`
**Author:** Copilot
**Reviewer:** Pál Lakatos-Tóth

## Scope

Stamps `status.conditions` + `observedGeneration` on every
validation-failure exit of the `ClawSandbox` reconciler. Previously
only the Running-success path wrote status; the three spec-validation
early-returns (invalid isolation, empty model, no inference endpoint)
silently requeued every 60s with an empty `.status`, which is
indistinguishable from "controller hasn't seen this CR yet". Operators
using `kubectl wait --for=condition=Degraded` or
`--for=condition=Ready` against such CRs would hang indefinitely.

**Files touched:**

- `controller/src/status/mod.rs` — adds `build_degraded_status_patch()`
  + 3 unit tests.
- `controller/src/reconciler.rs` — adds `stamp_degraded()` helper and
  wires the three validation-failure exits to call it before
  requeuing.
- `docs/security-audits/2026-04-25-phase1-minimal-conditions-audit.md`
  (this file).

## Threat / operability model

| Threat | Mitigation |
|--------|-----------|
| Bad spec silently ignored by controller | Before: empty `.status`, 60s requeue, `kubectl wait` hangs. After: `status.phase = Degraded`, `Degraded=True` and `Ready=False` conditions with machine-readable reasons (`SpecInvalid`, `DependencyMissing`) and human-readable messages. |
| `observedGeneration` stale across failures | Every `build_degraded_status_patch` call sets both the top-level `observedGeneration` and each condition's `observedGeneration` from `metadata.generation`. |
| `lastTransitionTime` churn on repeated same-status reconciles | Uses existing `preserve_transition_time` helper. Unit test `degraded_patch_preserves_transition_time_on_repeat` pins this. |
| patch_status failure breaks reconciliation | `stamp_degraded` logs `warn!` on error and continues. We still return the intended `Action::requeue(60s)` so the reconcile loop recovers. |
| Invalid-name CR can't be status-patched | The name-validation path (line 128) is documented as *not* calling `stamp_degraded` because without a K8s-legal name we can't target the CR with `patch_status`. This path only fires on corrupt informer state; real CRs are rejected by the OpenAPI schema on admission. |

## Tests added

Three unit tests in `controller/src/status/mod.rs`:

1. `degraded_patch_stamps_degraded_true_and_ready_false`
2. `degraded_patch_preserves_transition_time_on_repeat`
3. `degraded_patch_handles_missing_generation`

Workspace totals: **376 tests green** (up from 373).

## CI gates

- [x] `ci/check-loc.sh` (touched files shrunk: reconciler.rs docs were
  pre-existing and compacted to fit the new helper; net +45 LOC but
  status/mod.rs is not budgeted, and reconciler.rs grew within budget —
  see `ci/loc-budget.yaml` phase1 cap).
- [x] `ci/no-stubs.sh`
- [x] `ci/no-custom-crypto.sh`
- [x] `ci/no-null-provider-prod.sh`
- [x] `ci/vendored-patch-audit.sh`
- [x] `ci/security-audit-required.sh`
- [x] `cargo clippy --all-targets -- -D warnings`
- [x] `cargo test --all`

## Out of scope

- A `Progressing=True` transition between observing a CR and stamping
  Running — useful addition but requires splitting the reconcile flow
  into stages with interim status writes; out of the blast-radius of
  this branch.
- McpServer / ToolPolicy CRD reconcilers — they don't exist yet (plan
  §7 phase 2).
- Stamping Degraded when a downstream `?` call fails mid-reconcile
  (e.g., namespace creation fails). Currently those bubble through
  `error_policy` as `ReconcileError::Kube` and get a 30s requeue; a
  future PR can add a best-effort Degraded stamp from the error_policy
  callback once it has CR access.

---

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pál Lakatos-Tóth <pallakatos@microsoft.com>
