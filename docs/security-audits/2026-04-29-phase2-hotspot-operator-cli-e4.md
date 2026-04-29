# Phase 2 — S15.e.4 — operator.ts action helpers extraction

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-operator-cli-e4`
**Sign-offs:** Core ✅, Security ✅

## Scope

Fourth sub-slice of the S15.e operator.ts decomposition train. Lifts the
four egress action helpers (`approveDomain`, `denyDomain`, `enforceEgress`,
`learnEgress`) out of the giant `startDashboard` closure into a dedicated
`operator/actions.ts` module.

## What moved

| File | Functions | LOC |
|---|---|---|
| `cli/src/commands/operator/actions.ts` (new) | `createActions(ctx)` → `{approveDomain, denyDomain, enforceEgress, learnEgress}` | 116 |

## Behavior delta

**None.** Bodies byte-identical to originals. Mechanical edits only:

- Closure-captured `sandboxes`, `activityLog`, `kubeContext` are now
  injected via an `ActionContext` factory.
- `sandboxes` is reassigned in `refresh()` (line ~1068, `sandboxes =
  await fetchSandboxes(...)`), so the context exposes a `getSandboxes()`
  getter rather than capturing the array reference. This preserves the
  original semantic (each invocation re-reads the latest array).
- `activityLog` is the `contrib.log` widget; the context types it
  structurally as `{ log(msg: string): void }` to avoid pulling
  `blessed-contrib` into the actions module.
- 6 call sites in `startDashboard` unchanged (closure binding via
  destructure).

## Security invariants preserved

- Same router endpoints (`POST /egress/{approve,deny,enforce,learn}`).
- Same CRD patches (`learnEgress: true|false` under
  `spec.networkPolicy`).
- Same `kctl()` wrapper for `--context` injection (no path bypassing
  the user-selected kube context).
- Same error swallowing on the secondary CRD patch (`.catch(() => {})`)
  to avoid leaving the router-side mode and CRD spec out of sync if
  the patch fails — semantics preserved verbatim.

## LOC delta

| Slice | operator.ts | Δ | Cumulative |
|---|---|---|---|
| pre-S15.e | 2894 | — | — |
| S15.e.1 (#87) | 2739 | −155 | −155 |
| S15.e.2 (#88) | 2483 | −256 | −411 |
| S15.e.3 (#89) | 1960 | −523 | −934 |
| **S15.e.4** (this PR) | **1880** | **−80** | **−1014** |
| §4.2 cap | 800 | | (renders + dialogs remaining) |

## Verification

- ✅ `npx tsc --noEmit` clean
- ✅ `npm run lint` 21 warnings (unchanged baseline), 0 errors
- ✅ `npm run build` clean
- ✅ `npm test -- --run` → 454 pass / 2 skipped (pre-existing)

## Next slices in S15.e sub-train

- **S15.e.5** — render helpers (`renderHeader`, `renderSecurity`,
  `renderAGT`, `renderTopology`, `renderCluster`, `render`, `makeBox`,
  `makeBar`, ~700 LOC). Likely 2 PRs.
- **S15.e.6** — modal/dialog state machine (`startEdit`, `launch`,
  `connectToAgent`, `deleteSelectedAgent`, ~700 LOC). Likely 1-2 PRs.

After e.4-e.6 land, operator.ts should hit ≤800 cap.
