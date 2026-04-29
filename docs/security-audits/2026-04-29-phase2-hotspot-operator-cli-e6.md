# Phase 2 — S15.e.6 — operator.ts spawn-dialog extraction

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-operator-cli-e6`
**Sign-offs:** Core ✅, Security ✅

## Scope

Eighth sub-slice of the S15.e operator.ts decomposition train. Lifts
the `n`-key spawn-agent dialog (267 LOC) — the heaviest remaining
closure-capture in operator.ts — into a dedicated dialogs module.

## What moved

| File | Functions | LOC |
|---|---|---|
| `cli/src/commands/operator/dialogs/spawn.ts` (new) | `openSpawnDialog(ctx)` containing `draw`, `close`, `startEdit`, `launch`, `onKey` | ~295 |

A thin wrapper in operator.ts preserves the `screen.key(["n"], ...)`
handler signature; body is delegated to the imported function via a
`SpawnDialogContext`.

## Behavior delta

**None.** Body byte-identical to original (lines 723-987 in the
pre-slice file). Closure-captured state injected via context:

- `screen`, `activityLog`, `kctl`, `kubeContext`, `devMode` (read-only)
- `setDialogOpen(open: boolean)` — replaces direct `dialogOpen = …`
  assignments so the caller still owns the modal flag
- `refresh`, `learnEgress` — async callbacks invoked at completion

## LOC delta

| Slice | operator.ts | Δ | Cumulative |
|---|---|---|---|
| pre-S15.e | 2894 | — | — |
| S15.e.1-5c | 1279 | | −1615 |
| **S15.e.6** | **1027** | **−252** | **−1867** |
| §4.2 cap | 800 | | (`connectToAgent`+`deleteSelectedAgent` ≈ 230 LOC remaining) |

## Verification

- ✅ `npx tsc --noEmit` clean
- ✅ `npm run lint` 22 warnings (+2: two `eslint-disable any` for the
  blessed.Screen typing in the new module — same pattern as other
  render modules), 0 errors
- ✅ `npm run build` clean
- ✅ `npm test -- --run` → 454 pass / 2 skipped (pre-existing)

## Next slices

- **S15.e.7** — `connectToAgent` + `deleteSelectedAgent` modal
  extraction. Closes the §4.2 cap (1027 → ~800).
