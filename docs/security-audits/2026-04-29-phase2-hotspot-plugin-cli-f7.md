# Phase 2 — S15.f.7 — plugin.ts handoff orchestration extraction

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-plugin-cli-f7`
**Sign-offs:** Core ✅, Security ✅

## Scope

Seventh sub-slice of S15.f. Extracts the second of the two big plugin.ts
behemoths — `_runHandoffOrchestration` (531 LOC), the background routine
that drives a multi-step agent state transfer (cloud↔local) over the
mesh, plus its `_hp` progress-tracker helper.

## What moved

| File | Symbol | LOC |
|---|---|---|
| `cli/src/core/agt-handoff.ts` (new) | `runHandoffOrchestration(token, adminToken, direction, dirLabel, deps)`, `_hp` (closure inside), `HandoffProgress`, `AgtInboxEntry`, `HandoffDeps` interfaces | 609 |

## HandoffDeps

```ts
interface HandoffDeps {
  progress: HandoffProgress;        // mutable; mutations propagate
  inbox: AgtInboxEntry[];           // mutable; splice/findIndex
  meshClient: () => AnyMeshClient | null;
  identity: () => AnyMeshIdentity | null;
  meshSend: (client, target, msg, log?) => Promise<string|undefined>;
  log: Logger;
}
```

`progress` is passed as object reference — every `progress.foo = ...`
mutation in the extracted function updates the plugin.ts singleton
because JS objects pass by reference. Same for `inbox` (an array).

`_hp` becomes a closure inside `runHandoffOrchestration` that captures
`progress` + `log` from `deps`. plugin.ts no longer needs its own `_hp`.

## Behavior delta

**None.** Body byte-identical apart from:
- `agtMeshClient` → `deps.meshClient()`
- `agtIdentity` → `deps.identity()`
- `meshSend(...)` → `deps.meshSend(...)` (forwards to the same plugin.ts
  wrapper that captures `agtIdentity`)
- `amidToName` / `nameToAmid` imported directly from `core/amid-cache.ts`

## LOC delta

| Slice | plugin.ts | Δ | Cumulative |
|---|---|---|---|
| pre-S15.f | 7139 | — | — |
| S15.f.1 | 6974 | −165 | −165 |
| S15.f.2 | 6890 | −84 | −249 |
| S15.f.3 | 6648 | −242 | −491 |
| S15.f.4 | 6488 | −160 | −651 |
| S15.f.5 | 6104 | −384 | −1035 |
| S15.f.6 | 5598 | −506 | −1541 |
| **S15.f.7** | **5071** | **−527** | **−2068** |
| §4.2 cap | 3000 | | 2071 LOC remaining |

## Verification

- ✅ `npx tsc --noEmit` clean
- ✅ `npm run lint` 30 warnings (unchanged), 0 errors
- ✅ `npm run build` clean
- ✅ `npm test -- --run` → **454 pass / 2 skipped** (baseline)

## Risk + rollback

- **Risk: medium.** Handoff is a long, multi-phase flow over the mesh
  with sub-agent state collection — bug in dep-threading would only
  surface during a real handoff (no unit tests cover this end-to-end).
  Mitigations: deps bag built at every call (always reflects current
  singleton state), `progress` and `inbox` pass by reference (mutations
  propagate exactly as before), `meshSend` continues to flow through
  the plugin.ts wrapper that captures `agtIdentity`.
- **Rollback:** simple revert.

## Remaining S15.f train (2 slices to cap)

- **f.8** `core/agt-tools/{foundry,mesh,spawn,handoff,http-fetch}.ts`
  cluster — lifts ~21 `api.registerTool` blocks from initAGT
  (~1500 LOC).
- **f.9** `core/openclaw-commands/*.ts` — slash commands from
  definePluginEntry (~500 LOC). Closes §4.2 cap of 3000.

Then **S15.g** moves the lot under `runtimes/openclaw/` per
`plan.md` (executes after cap is met, NOT before, to avoid
re-rooting the f.8/f.9 diffs).
