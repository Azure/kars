# Security audit — Phase 2 / S15: hotspot pass3 (handoff.ts)

**Slice:** `phase2-hotspot-handoff-cli`
**Branch:** `phase2-hotspot-handoff-cli` → `dev`
**Date:** 2026-04-29
**Scope item:** `docs/implementation-plan.md` §15 hotspot decomposition. Reduces `cli/src/commands/handoff.ts` from 1119 → 798 LOC, under the 800-LOC §15 cap.

## Summary

Extracts the closure-captured helper bundle (router/AKS port-forward / admin-token / Docker wake / CRD-read / credential rehydrate) and the `--status` / `--abort` branches from the long-lived `.action()` callback into `cli/src/commands/handoff/helpers.ts`. The forward + reverse orchestration body remains in `handoff.ts` unchanged.

No behavioral change. The closure captures (`containerName`, `targetNs`, `aksPfPort`, the mutable `aksPfProc` child-process handle) migrate from action-scope to factory-scope; the helper functions still see the same set of variables, the same way.

## Existing implementation surveyed (per §0.2 #8 — no parallel-implementation)

- `cli/src/commands/handoff.ts` (1119 LOC) — single `.action()` callback owning helpers + 3 orchestration branches. Confirmed no other module had previously extracted these helpers.
- `cli/src/commands/operator/` — example of an existing "command-with-helpers" subdir convention this slice follows (`keymap.ts` + `keymap.test.ts`).
- `inference-router/src/handoff/` — Rust side of the same feature; already decomposed by a prior S15 sub-slice. CLI side now mirrors that shape.
- `cli/src/plugin.ts` `_runHandoffOrchestration` — separate LLM-driven path; intentionally NOT touched (different transport, different state machine).

No new dependency added.

## What this slice ships

1. **New file** `cli/src/commands/handoff/helpers.ts` (~360 LOC):
   - `WORKSPACE_TAR_CMD` const (top-of-file, exported).
   - `interface HandoffHelpers` — type surface of the factory return.
   - `createHandoffHelpers(name)` — async factory; closes over `containerName`, `targetNs`, `aksPfPort`, `aksPfProc`, returns the bundle.
   - `runStatus(name, h)` — `--status` branch (verbatim move).
   - `runAbort(h)` — `--abort` branch (verbatim move).
2. **`handoff.ts` rewrite** (797 LOC):
   - Imports the factory + the two branch helpers via dynamic import (preserves the existing `await import("execa")` pattern).
   - `.action()` body: 1 call to `createHandoffHelpers`, destructure, dispatch on `options.status` / `options.abort`, then the unchanged forward/reverse orchestration block.
3. **No tests changed.** All 454 existing CLI tests still pass; helpers.ts has no public test surface (its functions all wrap shell-out side effects that the existing `handoff` integration story exercises end-to-end via its sandbox-side handoff flow).

## Threat model deltas

None. Same closure-state model, same shell command shapes, same env handling. The factory result is a plain object reference held only inside the `.action()` callback and not exposed beyond the module boundary.

## Verification

```text
cli/src/commands/handoff.ts:                1119 → 798 LOC  (under §15 800 cap)
cli/src/commands/handoff/helpers.ts:        new (~360 LOC)
$ cd cli && npx tsc --noEmit                # clean
$ cd cli && npm run build                   # clean
$ cd cli && npm test -- --run               # 454 passed | 2 skipped
$ cd cli && npm run lint                    # 0 errors (26 pre-existing warnings)
```

## Deferred

- `cli/src/plugin.ts` (7139 → 3000 cap) — too large for one bounded slice; needs its own multi-PR S15 sub-train.
- `cli/src/commands/operator.ts` (2894 → 1200 cap) — next §15 candidate.
- `cli/src/commands/up.ts` (1849 → 800 cap) — third §15 candidate.
- `cli/src/commands/mesh.ts` (1583 → 800 cap) — fourth §15 candidate.

## Sign-offs

- Core: ✅
- Security: ✅
