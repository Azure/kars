# Phase 2 — Stable SSA field managers (S7.A)

**Date:** 2026-04-28
**Slice:** S7.A `phase2-conditions-ssa-leader` — first sub-slice (stable field managers)
**Branch:** `phase2-conditions-ssa-leader`
**Plan reference:** `docs/implementation-plan.md` §10.4 #1; session-plan §S7.

## Scope

S7 is a multi-sub-slice train (Conditions matrix, SSA, leader election, predicated informers, reconcile-DAG, workqueue metrics, VAP/MAP expansion). This first sub-slice closes **only the SSA / stable-field-manager** piece per §10.4 #1.

### What landed

- New `controller/src/field_managers.rs` — central registry of every SSA `fieldManager` the controller emits.
- Each per-CRD reconciler that previously held a private `FIELD_MANAGER` constant now references the central registry. The constants are physically identical to the old strings — no field-ownership migration on existing clusters.
- The five remaining bare-string sites (`reconciler/mod.rs`, `pairing.rs`, `pairing_reconciler.rs`, `mesh_peer/offload.rs`, `mesh_peer/pair.rs`) now reference named constants:
  - `reconciler/mod.rs` (13 sites) — `"azureclaw-controller"` → `field_managers::CLAWSANDBOX` (`azureclaw-controller/clawsandbox`).
  - `pairing.rs` + `pairing_reconciler.rs` (3 sites) — `"azureclaw-controller"` → `field_managers::PAIRING` (`azureclaw-controller/pairing`).
  - `mesh_peer/offload.rs` + `mesh_peer/pair.rs` (3 sites) — `"azureclaw-mesh-peer"` → `field_managers::MESH_PEER` (string verbatim, just constantized).
- `providers::field_managers` becomes a backwards-compat re-export so any future caller using the original Phase 1 path keeps working.
- 4 new tests assert: uniqueness across the registry, namespaced-format convention, no bare `"azureclaw-controller"` reuse, and per-CRD constants match their pre-S7 strings (zero-migration invariant).

### Field-manager renames (SSA migration footprint)

The `reconciler/mod.rs` and `pairing*.rs` sites previously all wrote with bare `"azureclaw-controller"`. After this slice they write with subsystem-specific managers (`/clawsandbox`, `/pairing`). On an existing cluster, the API server will record the new manager as a separate owner alongside the legacy one for the same field paths until the next conflict-forcing apply.

**Mitigation:** every changed site already had `.force()` on its `PatchParams`, so the first reconcile after upgrade transparently transfers ownership. No operator action required. Verified by inspection of every site listed above.

The `mesh_peer/*` sites kept the legacy string `azureclaw-mesh-peer` exactly to avoid this migration entirely; only the constant name changes (string is identical).

## Out of scope (deferred to subsequent S7 sub-slices)

- Full Conditions matrix (`Progressing` step-wise emission) — S7.B.
- Leader election + predicated informers — S7.C.
- Backoff with jitter + reconcile-DAG cold-start optimisation — S7.D.
- Workqueue metrics on `/metrics` + reconcile spans — S7.E.
- VAP/MAP expansion (Content-Safety floor admission, posture-downgrade denials) — S7.F.

## Hard-rule checklist (§0.2)

- [x] **#1 No duplication** — central registry; old `providers::field_managers` becomes a re-export, not a parallel implementation.
- [x] **#3 No dead schema** — every constant in `ALL_FIELD_MANAGERS` is consumed at a real call site (or, for `ROUTER_RECONCILER`, explicitly reserved with a doc comment for cross-crate use).
- [x] **#8 No custom crypto** — no crypto in this slice.
- [x] **#9 Audit doc** — this file.
- [x] **#10 Verify, don't guess** — every replacement was preceded by `grep` on `PatchParams::apply` to enumerate all 44 call sites; `cargo build` + `cargo test --workspace` + `cargo clippy -- -D warnings` + `cargo fmt --check` all pass.

## Test coverage

- `controller/src/field_managers.rs` — 4 new tests:
  - `all_field_managers_are_unique` (HashSet equality with slice length).
  - `field_managers_use_namespaced_format` (loop guard against future bare strings).
  - `no_bare_azureclaw_controller_string` (regression-prevention).
  - `legacy_provider_constants_match` (zero-migration invariant).
- Existing per-reconciler `field_manager_is_per_reconciler` tests (in `claw_eval_reconciler.rs`, `inference_policy_reconciler.rs`, etc.) continue to pass with the constants now sourced via re-export.
- Workspace test count: **328** controller tests passing (was 324 before S7.A — +4 from the new field-manager tests). Router (105) + integration (26) unchanged. Clippy clean, fmt clean.

## Threat model

| Concern | Mitigation |
|---|---|
| Two reconcilers race on the same field manager and overwrite each other's status | Uniqueness test fails the build if any two constants collide; bare strings are flagged by the no-bare-string test. |
| SSA field-ownership migration breaks running clusters on upgrade | All affected sites already used `.force()`; first reconcile transfers ownership. `mesh_peer/*` kept the legacy string verbatim to avoid migration entirely. |
| New reconciler added in a future slice forgets to register a constant | Reviewer checklist + namespaced-format test catches `"azureclaw-controller"` regressions. The pattern is now exemplified across all 9 reconcilers. |

## Sign-offs

- [x] Author: GitHub Copilot CLI agent (claude-opus-4.7).
- [x] Reviewer: Pal Lakatos-Toth (admin merge).


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
