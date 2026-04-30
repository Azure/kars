# Phase 2 — S15.f.2 — plugin.ts native-agent delegate extraction

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-plugin-cli-f2`
**Sign-offs:** Core ✅, Security ✅

## Scope

Second sub-slice of the S15.f plugin.ts decomposition train. Lifts the
`delegateToNativeAgent` helper — used by the AGT mesh task path to
dispatch incoming `task_request`s to the local OpenClaw agent loop —
into a dedicated module.

## What moved

| File | Symbols | LOC |
|---|---|---|
| `cli/src/core/agt-task-delegate.ts` (new) | `delegateToNativeAgent(taskContent, fromAgent, log)` + `TaskLogger` interface | 95 |

This function is unusually clean: it depends only on `node:child_process`
and `node:fs`, and its `log` parameter is already injected by callers.
**No AGT/router/cache singletons touched** — pure extraction.

## Behavior delta

**None.** Body byte-identical. Only the JSDoc surface gets a small
local interface (`TaskLogger`) instead of an inline structural type.

## LOC delta

| Slice | plugin.ts | Δ | Cumulative |
|---|---|---|---|
| pre-S15.f | 7139 | — | — |
| S15.f.1 | 6974 | −165 | −165 |
| **S15.f.2** | **6890** | **−84** | **−249** |
| §4.2 cap | 800 | | 6090 LOC remaining |

## Verification

- ✅ `npx tsc --noEmit` clean
- ✅ `npm run lint` 24 warnings (unchanged from f.1), 0 errors
- ✅ `npm run build` clean
- ✅ `npm test -- --run` → **454 pass / 2 skipped** (same as baseline)

## Risk + rollback

- **Risk: very low.** Function has zero plugin-internal dependencies;
  the `log` param is the only injected concern.
- **Rollback:** simple revert.

## Next slices

- **S15.f.3** — extract `meshSend` (~86 LOC) + `meshHandleTransportMessage`
  (~121 LOC) to `core/mesh-transport.ts`. These take the AGT client
  / identity by parameter so the extraction keeps the singletons in
  plugin.ts but moves the chunked-transfer state machine out.
- **S15.f.4+** — Class A Foundry shims to `/platform/mcp` (S10.B).


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
