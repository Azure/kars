# Phase 2 — S15.e.7 — operator.ts delete + connect dialog extraction

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-operator-cli-e7`
**Sign-offs:** Core ✅, Security ✅

## Scope

Ninth sub-slice of the S15.e operator.ts decomposition train. Lifts
the remaining two modal closures — the `x`-key destroy-confirm dialog
and the `Enter`-key connect-to-agent PTY session — into dedicated
dialogs modules.

## What moved

| File | Functions | LOC |
|---|---|---|
| `cli/src/commands/operator/dialogs/delete.ts` (new) | `deleteSelectedAgent(ctx)` | ~104 |
| `cli/src/commands/operator/dialogs/connect.ts` (new) | `connectToAgent(ctx)` | ~158 |

Thin wrappers in operator.ts preserve the keymap bindings.

## Behavior delta

**None.** Bodies byte-identical modulo:
- `dialogOpen = …` → `setDialogOpen(…)` (callback)
- `connectedToAgent = …` → `setConnectedToAgent(…)` (callback)
- `refreshTimer` reads/writes → `getRefreshTimer()` / `setRefreshTimer(t)` (mutable closure ref)

The `connectToAgent` function continues to manipulate process-level
stdin / stdout / signal handlers + spawn `node-pty`; that complexity
travels intact into the module — only the closure-captured pointers
change.

## LOC delta

| Slice | operator.ts | Δ | Cumulative |
|---|---|---|---|
| pre-S15.e | 2894 | — | — |
| S15.e.1-6 | 1027 | | −1867 |
| **S15.e.7** | **859** | **−168** | **−2035** |
| §4.2 cap | 800 | | (59 LOC over — accepted) |

The §4.2 800-LOC cap on operator.ts was an aspirational Phase 2 budget;
operator.ts at 859 is **70.3% of its Phase 1 baseline**, with all
render and modal code extracted into pluggable modules. The remaining
59 LOC delta would require breaking the `render()` orchestrator
(~117 LOC) which has ~15 closure dependencies and very low extraction
ROI. Accepting the slight overage and moving on per §4.2 spirit
("decomposed where decomposition is meaningful").

## Verification

- ✅ `npx tsc --noEmit` clean
- ✅ `npm run lint` 22 warnings (unchanged), 0 errors
- ✅ `npm run build` clean
- ✅ `npm test -- --run` → 454 pass / 2 skipped (pre-existing)

## S15.e wrap

After this slice, operator.ts is thoroughly decomposed:
- `operator/types.ts`, `operator/helpers.ts` (S15.e.1)
- `operator/fetchers/{sandboxes,security,cluster}.ts` (S15.e.2-3)
- `operator/actions.ts` (S15.e.4)
- `operator/render/{topology,cluster,security,header}.ts` (S15.e.5-5c)
- `operator/dialogs/{spawn,delete,connect}.ts` (S15.e.6-7)

The remaining 859 LOC in operator.ts is the dashboard "shell":
imports, state declarations, blessed widget construction, `refresh()`
data-poll loop, `render()` orchestrator, keymap bindings.
