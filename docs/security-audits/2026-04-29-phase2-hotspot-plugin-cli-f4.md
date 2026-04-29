# Phase 2 — S15.f.4 — plugin.ts task-tools array extraction

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-plugin-cli-f4`
**Sign-offs:** Core ✅, Security ✅

## Scope

Fourth sub-slice of S15.f. Lifts the OpenAI function-call tool-spec
array consumed by `processTaskWithTools` (the offload / sub-agent
LLM loop) into a dedicated module.

These are inert OpenAPI-style schema descriptors — no closures, no
captured variables, no runtime behavior. They define how the LLM
*sees* the available tools; the actual handlers stay in plugin.ts's
switch block.

## What moved

| File | Symbols | LOC |
|---|---|---|
| `cli/src/core/agt-task-tools.ts` (new) | `TASK_TOOLS` (typed `any[]`) — 11 tool descriptors: `exec_command`, `file_write`, `http_fetch`, `foundry_web_search`, `foundry_code_execute`, `foundry_file_search`, `foundry_memory`, `foundry_image_generation`, `mesh_send`, `mesh_inbox`, `discover` | 171 |

`plugin.ts processTaskWithTools` body now reads `const tools = TASK_TOOLS;`.

## Behavior delta

**None.** Array literal byte-identical (only outer indentation
stripped). Same descriptor count, same JSON Schema shape, same tool
names.

## LOC delta

| Slice | plugin.ts | Δ | Cumulative |
|---|---|---|---|
| pre-S15.f | 7139 | — | — |
| S15.f.1 | 6974 | −165 | −165 |
| S15.f.2 | 6890 | −84 | −249 |
| S15.f.3 | 6648 | −242 | −491 |
| **S15.f.4** | **6488** | **−160** | **−651** |
| §4.2 cap | 800 | | 5688 LOC remaining |

## Verification

- ✅ `npx tsc --noEmit` clean
- ✅ `npm run lint` 29 warnings (unchanged from f.3), 0 errors
- ✅ `npm run build` clean
- ✅ `npm test -- --run` → **454 pass / 2 skipped** (same as baseline)

## Risk + rollback

- **Risk: minimal.** Inert data move; all consumer paths via the
  single import.
- **Rollback:** simple revert.

## Next slices

- **S15.f.5** — Either continue chunking of `processTaskWithTools`
  (system-prompt strings, tool-handler switch body) or pivot to the
  Class A Foundry-shim migration to S10.B `/platform/mcp` once
  that endpoint exists.
