# Phase 2 — S15.f.6 — plugin.ts in-process tool-calling loop extraction

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-plugin-cli-f6`
**Sign-offs:** Core ✅, Security ✅

## Scope

Sixth sub-slice of S15.f. Extracts the AGT in-process tool-calling
loop — the function that drives 25 rounds of LLM ↔ tool execution
against the inference router for sub-agent task processing.

## What moved

| File | Symbol | LOC |
|---|---|---|
| `cli/src/core/agt-task-loop.ts` (new) | `processTaskWithTools(taskContent, deps, log)` + `TaskLoopDeps` interface | 540 |

## TaskLoopDeps

The extracted function reads / mutates two slices of plugin.ts state
(mesh client + handoff-interrupt flags) so it takes a deps bag:

```ts
interface TaskLoopDeps {
  meshClient: () => AnyMeshClient | null;
  isInterruptRequested: () => boolean;
  interruptReason: () => string;
  setInterrupt: (requested: boolean, reason: string) => void;
}
```

Pure imports (`TASK_TOOLS`, `routerUrl`, `resolveAmidByName`,
`sanitizeLog`) are now imported directly in the module. `nameToAmid`
is not used by this function. `resolveAmidByName` calls in the new
module use the canonical 2-arg form `(name, routerUrl, opts?)`
since the wrapper on plugin.ts is no longer reachable from
inside `core/`.

## Behavior delta

**None.** Function body byte-identical apart from:
- `TASK_TOOLS`, `routerUrl`, `resolveAmidByName`, `sanitizeLog`
  imported directly instead of plugin-scope.
- 2-arg `resolveAmidByName` (passing `routerUrl` explicitly).
- Mesh client / interrupt flags accessed via `deps.*` instead of
  module-scope `let`s.

## LOC delta

| Slice | plugin.ts | Δ | Cumulative |
|---|---|---|---|
| pre-S15.f | 7139 | — | — |
| S15.f.1 | 6974 | −165 | −165 |
| S15.f.2 | 6890 | −84 | −249 |
| S15.f.3 | 6648 | −242 | −491 |
| S15.f.4 | 6488 | −160 | −651 |
| S15.f.5 | 6104 | −384 | −1035 |
| **S15.f.6** | **5598** | **−506** | **−1541** |
| §4.2 cap | 3000 | | 2598 LOC remaining |

## Verification

- ✅ `npx tsc --noEmit` clean
- ✅ `npm run lint` 30 warnings (was 29; +1 cross-module `any` cast), 0 errors
- ✅ `npm run build` clean
- ✅ `npm test -- --run` → **454 pass / 2 skipped** (baseline)

## Risk + rollback

- **Risk: medium.** This is the single hottest code path in the sub-
  agent — every offload + every parent-driven sub-task hits 1+ rounds
  here. But the body is byte-identical and the deps bag is constructed
  at every call (always reflects current singleton values), so there
  is no possibility of stale state.
- **Rollback:** simple revert.

## Remaining S15.f train (3 slices to cap)

After f.6:
- **f.7** `core/agt-handoff.ts` — `_runHandoffOrchestration` (531 LOC)
  + `_hp` helper. The other big function in plugin.ts.
- **f.8** `core/agt-tools/{foundry,mesh,spawn,handoff,http-fetch}.ts`
  cluster — lifts ~21 `api.registerTool` blocks from initAGT
  (~1500 LOC).
- **f.9** `core/openclaw-commands/*.ts` — slash commands from
  definePluginEntry (~500 LOC). Closes §4.2 cap of 3000.

Then **S15.g** moves the lot under `runtimes/openclaw/` per the
package-split plan recorded in `plan.md`.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
