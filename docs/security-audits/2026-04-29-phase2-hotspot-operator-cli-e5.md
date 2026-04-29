# Phase 2 — S15.e.5 — operator.ts cluster + topology render extraction

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-operator-cli-e5`
**Sign-offs:** Core ✅, Security ✅

## Scope

Fifth sub-slice of the S15.e operator.ts decomposition train. Lifts
`renderTopology` (with its nested `makeBox`/`fitVis`/`statusIcon`/
`visualLen` helpers) and `renderCluster` (with `makeBar`) out of the
giant `startDashboard` closure into a new `operator/render/` module
group.

The remaining renders (`renderHeader`, `renderSecurity`, `renderAGT`,
`renderAGTFull`, `render` orchestrator) stay in operator.ts for S15.e.5b
because they touch a much larger surface of the closure state.

## What moved

| File | Functions | LOC |
|---|---|---|
| `cli/src/commands/operator/render/cluster.ts` (new) | `renderCluster(ctx)`, `makeBar(pct)` | ~143 |
| `cli/src/commands/operator/render/topology.ts` (new) | `renderTopology(ctx)` (with nested helpers) | ~199 |

Two thin wrappers remain in operator.ts so call sites in the orchestrator
`render()` are unchanged:

```ts
function renderTopology(): void {
  _renderTopology({ sandboxes, securityStates, topologyBox });
}
function renderCluster(): void {
  _renderCluster({ clusterData, clusterNodeBox, clusterInfoBox });
}
```

## Behavior delta

**None.** Bodies byte-identical. Closure-captured `sandboxes`,
`securityStates`, `topologyBox`, `clusterData`, `clusterNodeBox`,
`clusterInfoBox` are now injected via `RenderContext` interfaces.
Blessed widgets typed structurally (`{ setContent, setLabel? }`) to
avoid pulling `blessed-contrib` into the render modules.

The `sandboxes` array is reassigned in `refresh()` (line ~1068), but
the wrapper closes over the variable and re-reads it on each call —
preserving the original semantic.

## Removed (now unused) imports

- `NodeInfo` (only used inside `renderCluster`, now in cluster.ts)
- `sumPrometheusCounter` (was unused after S15.e.3 fetcher extraction;
  audit dropped it now)

## LOC delta

| Slice | operator.ts | Δ | Cumulative |
|---|---|---|---|
| pre-S15.e | 2894 | — | — |
| S15.e.1 (#87) | 2739 | −155 | −155 |
| S15.e.2 (#88) | 2483 | −256 | −411 |
| S15.e.3 (#89) | 1960 | −523 | −934 |
| S15.e.4 (#90) | 1880 | −80 | −1014 |
| **S15.e.5** (this PR) | **1586** | **−294** | **−1308** |
| §4.2 cap | 800 | | (S15.e.5b + S15.e.6 remaining) |

Lint warnings dropped from 21 → 20.

## Verification

- ✅ `npx tsc --noEmit` clean
- ✅ `npm run lint` 20 warnings (was 21), 0 errors
- ✅ `npm run build` clean
- ✅ `npm test -- --run` → 454 pass / 2 skipped (pre-existing)

## Next slices

- **S15.e.5b** — `renderHeader` + `renderSecurity` + `renderAGT` +
  `renderAGTFull` + `render()` orchestrator extraction (~440 LOC).
  Heavier closure-capture footprint (header includes `viewMode`,
  `clusterData`, `meshHealth`, `isRefreshing`, `spinFrames`, `spinIdx`,
  `clusterName`, `screen`, etc.).
- **S15.e.6** — modal/dialog state machine (`startEdit`, `launch`,
  `connectToAgent`, `deleteSelectedAgent`, ~700 LOC).
