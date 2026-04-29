# Phase 2 — S15.f.5 — plugin.ts heartbeat + offload extraction

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-plugin-cli-f5`
**Sign-offs:** Core ✅, Security ✅

## Scope

Fifth sub-slice of S15.f. Lifts two related functional clusters out of
`plugin.ts` in **one PR** (per the OSS-prep "no micro-PR" mandate):

1. **AGT heartbeat / mesh-session telemetry** — three short helpers:
   `recordMeshSession`, `agtReconnect`, `notifyInboxToMemory`.
2. **Offload executor** — `runOffloadTask` (the workspace-harvesting
   in-process tool-loop driver) and `startProactiveOffloadIfNeeded`
   (boot-time announce-and-execute when the controller injects
   `OFFLOAD_REQUEST_ID`).

## What moved

| File | Symbols | LOC |
|---|---|---|
| `cli/src/core/agt-heartbeat.ts` (new) | `recordMeshSession(identity, client, target, sessionId, intent, outcome, startedAt)`, `agtReconnect(client, isConnected, sandboxName, setConnected, log)`, `notifyInboxToMemory(inbox, log)` | 156 |
| `cli/src/core/agt-offload.ts` (new) | `runOffloadTask(opts, deps, log)`, `startProactiveOffloadIfNeeded(deps, log)`, `OffloadDeps` + `RunOffloadOpts` interfaces | 339 |

`plugin.ts` keeps thin wrappers that capture the singleton state via
closure:

```ts
function _offloadDeps() {
  return {
    meshClient: agtMeshClient, identity: agtIdentity,
    sandboxName: agtSandboxName, isConnected: () => agtConnected,
    offloadInFlight, meshSend, processTaskWithTools,
  };
}
async function runOffloadTask(opts, log) {
  return _runOffloadTask(opts, _offloadDeps(), log);
}
```

This deliberately **avoids** rewriting the ~310 read/write sites that
touch `agtMeshClient`/`agtIdentity`/`agtSandboxName`/etc. across
plugin.ts. State remains in plugin.ts as `let`-vars; the extracted
modules receive what they need explicitly. Same pattern as
`meshSend` in S15.f.3.

## Behavior delta

**None.** Function bodies byte-identical. Only changes:

- `recordMeshSession` no longer reads module-scope `agtIdentity`/
  `agtMeshClient` directly — they arrive as the first two args.
- `agtReconnect` mutates `agtConnected` via a `setConnected` callback
  instead of direct assignment (necessary because ES modules disallow
  cross-module reassignment of imported `let` bindings).
- `runOffloadTask` / `startProactiveOffloadIfNeeded` consume `deps`
  bag instead of free vars; `nameToAmid` now imported from
  `core/amid-cache.ts` directly (since S15.f.1).

## LOC delta

| Slice | plugin.ts | Δ | Cumulative |
|---|---|---|---|
| pre-S15.f | 7139 | — | — |
| S15.f.1 | 6974 | −165 | −165 |
| S15.f.2 | 6890 | −84 | −249 |
| S15.f.3 | 6648 | −242 | −491 |
| S15.f.4 | 6488 | −160 | −651 |
| **S15.f.5** | **6104** | **−384** | **−1035** |
| §4.2 cap | 3000 | | 3104 LOC remaining |

## Verification

- ✅ `npx tsc --noEmit` clean
- ✅ `npm run lint` 29 warnings (unchanged), 0 errors
- ✅ `npm run build` clean
- ✅ `npm test -- --run` → **454 pass / 2 skipped** (same as baseline)

## Risk + rollback

- **Risk: low-medium.** Offload is a long-lived background task with
  many side-effects; the dep-bag captured at call time is identical
  to what the original closure saw. The sole semantic change is the
  `setConnected` callback in `agtReconnect`, exercised once at
  reconnect time.
- **Rollback:** simple revert.

## Folder layout (architectural target)

After f.5, `cli/src/core/` is on track for a clean post-S15.f shape:

```
cli/src/core/
  agt-heartbeat.ts      ← new (f.5)
  agt-offload.ts        ← new (f.5)
  agt-task-tools.ts     ← f.4
  agt-task-delegate.ts  ← f.2
  amid-cache.ts         ← f.1
  log-redact.ts         ← f.1
  mesh-transport.ts     ← f.3
  foundry-discovery.ts  ← pre-S15.f
  router-client.ts      ← pre-S15.f
```

Future slices (locked):
- **f.6** `agt-task-loop.ts` (processTaskWithTools, ~521 LOC) +
  `agt-handoff.ts` (_runHandoffOrchestration, ~531 LOC)
- **f.7** `agt-tools/{foundry,mesh,spawn,handoff,http-fetch}.ts`
  (~1500 LOC of registerTool blocks from initAGT)
- **f.8** `openclaw-commands/*.ts` (the OpenClaw plugin slash
  commands, ~500 LOC) — closes §4.2 cap of 3000.
