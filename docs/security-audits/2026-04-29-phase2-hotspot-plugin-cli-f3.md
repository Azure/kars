# Phase 2 — S15.f.3 — plugin.ts chunked mesh transport extraction

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-plugin-cli-f3`
**Sign-offs:** Core ✅, Security ✅

## Scope

Third sub-slice of the S15.f plugin.ts decomposition train. Lifts the
chunked-transport layer that splits and reassembles large mesh
payloads (>512 KB) into a dedicated `core/mesh-transport.ts` module.

This is wire-level transport state, not Signal Protocol crypto. The
Signal session, X3DH key exchange, Double Ratchet, and AGT identity
all stay in `plugin.ts` — we are not violating the
`agt-e2e-encryption` skill's hard rule. The new module only shuffles
JSON chunks; the per-message Ed25519 signature uses `agtIdentity`
threaded in by the caller via `meshSendWithIdentity`.

## What moved

| File | Symbols | LOC |
|---|---|---|
| `cli/src/core/mesh-transport.ts` (new) | `MESH_CHUNK_THRESHOLD`, `MESH_CHUNK_SIZE`, `MESH_MAX_CHUNKS`, `MESH_TRANSFER_TTL`, `PendingMeshTransfer` (interface), `pendingTransfers` (Map), TTL-cleanup interval, `meshSendWithIdentity(client, target, message, identity, log)`, `meshHandleTransportMessage(fromAmid, fromAgent, message, log)` | ~265 |

`plugin.ts` keeps a thin wrapper:

```ts
async function meshSend(client, target, message, log) {
  return meshSendWithIdentity(client, target, message, agtIdentity, log);
}
```

so all 14+ call sites stay byte-identical. `meshHandleTransportMessage`
is re-exported by name; consumers of `pendingTransfers`,
`PendingMeshTransfer`, and the `MESH_*` constants get them via the
same import statement.

## Behavior delta

**None.** Function bodies byte-identical (only `eslint-disable-next-line`
comments added on the existing `as any` casts that crossed module
boundaries). The TTL cleanup interval keeps `unref()` semantics.

## LOC delta

| Slice | plugin.ts | Δ | Cumulative |
|---|---|---|---|
| pre-S15.f | 7139 | — | — |
| S15.f.1 | 6974 | −165 | −165 |
| S15.f.2 | 6890 | −84 | −249 |
| **S15.f.3** | **6648** | **−242** | **−491** |
| §4.2 cap | 800 | | 5848 LOC remaining |

## Verification

- ✅ `npx tsc --noEmit` clean
- ✅ `npm run lint` 29 warnings (f.2 was 24; +5 from new
  `eslint-disable-next-line @typescript-eslint/no-explicit-any` markers
  on the existing `as any` casts that crossed the module boundary), 0
  errors
- ✅ `npm run build` clean
- ✅ `npm test -- --run` → **454 pass / 2 skipped** (same as baseline)

## Risk + rollback

- **Risk: low.** The chunked-transport state machine is self-contained
  and module-level state moved with the functions. The only external
  hook is `agtIdentity`, threaded explicitly via
  `meshSendWithIdentity` → wrapper preserves the original closure
  capture.
- **Module-load idempotency:** the TTL cleanup `setInterval` is now
  module-level in `mesh-transport.ts`. ES module identity guarantees
  a single timer per import path; the existing
  `__AGT_INITIALIZED` plugin singleton guard remains in `plugin.ts`
  and is unchanged.
- **Rollback:** simple revert.

## Next slices

- **S15.f.4** — extract `agtReconnect` + `notifyInboxToMemory`
  (~54 LOC) plus their interval-handle module-level state, with the
  AGT singleton scope shifted to a small state-bag module so future
  slices can break `processTaskWithTools` (~680 LOC).
- **S15.f.5+** — Class A Foundry shims to `/platform/mcp` (S10.B).
