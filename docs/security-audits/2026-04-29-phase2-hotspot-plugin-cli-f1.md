# Phase 2 — S15.f.1 — plugin.ts redact + AMID-cache extraction

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-plugin-cli-f1`
**Sign-offs:** Core ✅, Security ✅

## Scope

First sub-slice of the **S15.f plugin.ts decomposition train**. plugin.ts
is the 7139-LOC OpenClaw plugin entrypoint — at the §4.2 800-LOC cap
this is the largest remaining hotspot in the repo, by an order of
magnitude. Plan from §4.2 / S15.f playbook:

- **Class A** (~9 Foundry shims) → lift to `/platform/mcp` via S10.B
- **Class B** (~11 mesh/handoff/spawn tools) → stays per-runtime
- **Class C** (OpenClaw slash commands) → stays per-runtime

This first slice is **not** Class A/B/C work yet — it's the
prerequisite untangling: lifting the **shared utility primitives** out
of plugin.ts so subsequent slices can move tools without dragging
plugin.ts state with them.

## What moved

| File | Symbols | LOC |
|---|---|---|
| `cli/src/core/log-redact.ts` (new) | `redactSecrets(m)`, `sanitizeLog(s, maxLen)` | 40 |
| `cli/src/core/amid-cache.ts` (new) | `amidToName`, `nameToAmid`, `nameToAmidTs`, `parentTrustedAmids`, `peerSigningKeys`, `AMID_CACHE_TTL_MS`, `getCachedAmid`, `setCachedAmid`, `pickFreshestRegistryMatch`, `resolveAmidByName`, `resolveAmidToName`, `resolveSigningKey` | 213 |

`redactSecrets` is **re-exported** from plugin.ts to preserve the
existing `import { redactSecrets } from "./plugin.js"` surface (used by
internal log sinks and any unit-testing call-sites; current tests do
not import it directly so the re-export is precautionary).

`resolveAmidByName/ToName/SigningKey` take `routerUrl` as a parameter
to avoid a circular dep between `amid-cache.ts` and `plugin.ts`. Thin
wrappers in plugin.ts thread `routerUrl` through so existing call-sites
continue to use the no-arg signature.

## Behavior delta

**None.** All function bodies byte-identical. Module-level cache state
(`amidToName`, `nameToAmid`, `nameToAmidTs`, `parentTrustedAmids`,
`peerSigningKeys`) is exported as live `Map`/`Set` instances — every
plugin.ts mutation site (`.set(...)`, `.delete(...)`, `.has(...)`,
`.get(...)`, etc.) operates on the same singletons it did before.
Verified by reading every cross-reference site (89 references across
plugin.ts, all read or mutate the same exported handles).

## LOC delta

| Slice | plugin.ts | Δ | Cumulative |
|---|---|---|---|
| pre-S15.f | 7139 | — | — |
| **S15.f.1** | **6974** | **−165** | **−165** (2.3% of pre) |
| §4.2 cap | 800 | | 6174 LOC remaining to cap |

This is the prerequisite slice; the bulk reductions are in S15.f.2+
where Class A Foundry shims move to platform MCP.

## Verification

- ✅ `npx tsc --noEmit` clean
- ✅ `npm run lint` 24 warnings (+2 — `any`-type `eslint-disable`
  comments in amid-cache.ts mirroring the existing plugin.ts pattern;
  the maps and registry-search results are `any`-typed at the network
  boundary), 0 errors
- ✅ `npm run build` clean
- ✅ `npm test -- --run` → **454 pass / 2 skipped** (pre-existing).
  Same as baseline. No test regressed.

The cross-cutting nature of the AMID maps means **vitest test coverage
is the load-bearing safety net** for this slice. The plugin tests
exercise the mesh tools end-to-end (handoff, spawn, send/inbox,
sub-agent restore) which all read or mutate the extracted maps. A
green test pass = green semantics.

## Risk + rollback

- **Risk: low.** Pure module split, no behavior change. The maps
  remain singletons in module scope; ES modules guarantee single-load
  per import path so all consumers see the same instance.
- **Rollback:** revert this PR — plugin.ts pre-extraction state is
  fully recoverable from git history. No data migration, no on-disk
  state change.

## Next slices

- **S15.f.2** — extract `processTaskWithTools` + `delegateToNativeAgent`
  (~600 LOC) and the AGT trust seed-init helpers to a `core/agt-task/`
  module.
- **S15.f.3+** — Class A Foundry shims to `/platform/mcp` per S10.B.
- **S15.f.N** — Class B mesh tool extractions per-tool.
