# Phase 2 — S15.e.5c — operator.ts header render extraction

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-operator-cli-e5c`
**Sign-offs:** Core ✅, Security ✅

## Scope

Seventh sub-slice of the S15.e operator.ts decomposition train. Lifts
`renderHeader` and `healthSummary` out of the `startDashboard` closure
into a dedicated render module.

## What moved

| File | Functions | LOC |
|---|---|---|
| `cli/src/commands/operator/render/header.ts` (new) | `renderHeader(ctx)`, `healthSummary(sandboxes)` | ~98 |

A thin wrapper in operator.ts so call sites in `render()` are unchanged.

## Behavior delta

**None.** Bodies byte-identical. Closure-captured `sandboxes`,
`clusterData`, `meshHealth`, `isRefreshing`, `spinFrames`, `spinIdx`,
`clusterName`, `viewMode`, `header`, `screen` injected via
`HeaderRenderContext`. `totalEgressCount()` is invoked at call time
and the resulting number passed in (helper still lives in operator.ts).

## LOC delta

| Slice | operator.ts | Δ | Cumulative |
|---|---|---|---|
| pre-S15.e | 2894 | — | — |
| S15.e.1-5b (#87-#92) | 1318 | | −1576 |
| **S15.e.5c** (this PR) | **1279** | **−39** | **−1615** |
| §4.2 cap | 800 | | (S15.e.6 dialogs ~480 LOC remaining) |

Smaller than typical because the header is a tight ~47 LOC function;
most of the 1318 → 800 path lives in the dialog state machine
(S15.e.6).

## Verification

- ✅ `npx tsc --noEmit` clean
- ✅ `npm run lint` 20 warnings (unchanged), 0 errors
- ✅ `npm run build` clean
- ✅ `npm test -- --run` → 454 pass / 2 skipped (pre-existing)

## Next slices

- **S15.e.6** — modal/dialog state machine (`startEdit`, `launch`,
  `connectToAgent`, `deleteSelectedAgent`, ~480 LOC). Heaviest
  closure-capture; will close the §4.2 cap.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
