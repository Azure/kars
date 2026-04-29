# Phase 2 hotspot — `cli/src/plugin.ts` S15.f.9 (stateful AGT tool registrations)

**Slice:** S15.f.9 — sub-slice of `phase2-hotspot-pass3` per `docs/implementation-plan.md` §4.2.
**Date:** 2026-04-29.
**Branch:** `phase2-hotspot-plugin-cli-f9`.
**Cap target (Phase 2):** `cli/src/plugin.ts` ≤ 3000 LOC.

## Scope

Extract the cluster of 11 stateful AGT `api.registerTool` blocks (spawn lifecycle, mesh interactions, agent discovery, live handoff) from `cli/src/plugin.ts` into a new `cli/src/core/agt-tools/agt.ts` module. Together with the previously-extracted `core/agt-tools/{foundry,http-fetch}.ts` (S15.f.8), all 21 inline tool registrations in `register()` are now lifted to dedicated modules under `core/agt-tools/`.

This is the ninth sub-slice of S15.f, the multi-PR train decomposing `cli/src/plugin.ts` toward the §4.2 cap.

## LOC delta

| File | Before | After | Δ |
|---|---:|---:|---:|
| `cli/src/plugin.ts` | 4323 | 3233 | **−1090** |
| `cli/src/core/agt-tools/agt.ts` (new) | 0 | ~1130 | +1130 |

Cumulative S15.f progress: 7139 → **3233 LOC, −3906, only 233 LOC above §4.2 cap of 3000.**

## What moved

The block from the `// ── Register AzureClaw agent tools …` comment through the closing `if (registryMode !== "global") log.info(...)` else branch — 11 tool registrations plus three local helpers (`POD_DEAD_PHASES`, `probeSubAgentAlive`, `safeJson`) that were only consumed by the cluster — was excised wholesale. Tool bodies inside `core/agt-tools/agt.ts` are byte-identical.

| Tool | Cluster | New home |
|---|---|---|
| `azureclaw_spawn` | spawn | `core/agt-tools/agt.ts` |
| `azureclaw_spawn_status` | spawn | `core/agt-tools/agt.ts` |
| `azureclaw_spawn_destroy` | spawn | `core/agt-tools/agt.ts` |
| `azureclaw_spawn_list` | spawn | `core/agt-tools/agt.ts` |
| `azureclaw_mesh_send` | mesh | `core/agt-tools/agt.ts` |
| `azureclaw_mesh_inbox` | mesh | `core/agt-tools/agt.ts` |
| `azureclaw_mesh_transfer_file` | mesh | `core/agt-tools/agt.ts` |
| `azureclaw_discover` | mesh | `core/agt-tools/agt.ts` |
| `azureclaw_handoff_status` | handoff | `core/agt-tools/agt.ts` |
| `azureclaw_handoff_request` | handoff (registry_mode=global only) | `core/agt-tools/agt.ts` |
| `azureclaw_handoff_confirm` | handoff (registry_mode=global only) | `core/agt-tools/agt.ts` |

`plugin.ts` now invokes the cluster via one helper call:

```ts
registerAgtTools(api, {
  log,
  bannerAlreadyPrinted,
  inbox: agtInbox,
  meshClient: () => agtMeshClient,
  identity: () => agtIdentity,
  sandboxName: () => agtSandboxName,
  meshSend,
  handoffState,
  runHandoffOrchestration: _runHandoffOrchestration,
  recordMeshSession,
});
```

## State coupling — `handoffState` holder

`handoffProgress` was a module-level `let` in `plugin.ts` previously, mutated both by tool bodies (`handoff_request` writes a fresh tracker, `handoff_confirm` updates `.status`/`.steps`) and by the `_runHandoffOrchestration` wrapper. To allow the extracted module to mutate it through a stable reference, the variable was promoted to a holder object:

```ts
const handoffState: { current: HandoffProgress | null } = { current: null };
```

Both `plugin.ts` (3 sites: declaration + `_runHandoffOrchestration` wrapper) and the extracted module read/write `handoffState.current`. Object identity preserved across module boundaries.

## Other deps

| Field | Why |
|---|---|
| `log` | Logger (39 references inside the block). |
| `bannerAlreadyPrinted` | One-shot guard for log-spam suppression on re-registration; passed by value (no mutation in the cluster). |
| `inbox` | Shared `agtInbox` array — the cluster only mutates via `.push()` and `.findIndex()` so reference passing is sufficient. |
| `meshClient` / `identity` / `sandboxName` | Late-bound accessors. The three module-level mutables can rotate (re-init, reconnect, etc.). Tool execution must observe the current value, not the value at registration time. |
| `meshSend` | The auto-chunking wrapper that captures `agtIdentity` for signing — passed as a function reference. |
| `runHandoffOrchestration` | The `plugin.ts` wrapper that bridges to `core/agt-handoff.ts` (preserved here so the wrapper still owns `agtIdentity`/`agtMeshClient` capture). |
| `recordMeshSession` | The 5-arg wrapper from `plugin.ts` that captures identity + mesh client; also passed as a function reference. |

## Helpers moved

- `POD_DEAD_PHASES` (Set of terminal pod phases) and the local `probeSubAgentAlive` helper were only consumed by `azureclaw_spawn_status`. Both now live inside `core/agt-tools/agt.ts`. `plugin.ts` had no other references.
- `safeJson` was already lifted to `core/safe-json.ts` in S15.f.8; the duplicate inline definition in `register()` was removed.
- The duplicate `interface HandoffProgress` declaration in `plugin.ts` was removed; the type is now imported once from `core/agt-handoff.ts` (which has been the canonical source since S15.f.7).

## Operational invariants preserved

- **Tool surface unchanged.** Names, descriptions, parameter schemas, and execute return shapes are byte-identical to the previous inline registrations. The vendored extension manifest in `~/.openclaw-data/extensions/azureclaw/` continues to surface the same 11 tools.
- **AGT mesh wire format unchanged.** No changes to KNOCK protocol, X3DH session establishment, signing, ratchet step, or chunking thresholds (`MESH_CHUNK_THRESHOLD`/`MESH_CHUNK_SIZE`/`MESH_MAX_CHUNKS`/`MESH_TRANSFER_TTL` re-imported with same values).
- **Trust + audit unchanged.** `pushTrustToRouter` calls preserved verbatim; AGT audit logger calls unchanged.
- **Handoff flow unchanged.** `handoff_request` still calls `_runHandoffOrchestration` (which still calls `_runHandoffOrchestrationCore` from `core/agt-handoff.ts` from S15.f.7); the holder pattern just relocates the storage of `handoffProgress`.
- **Sandbox image build path unchanged.** `sandbox-images/openclaw/Dockerfile` lines 28–46 COPY `cli/src/` and `cli/dist/` as whole trees, so the new `core/agt-tools/agt.ts` ships into the sandbox automatically.

## Risk + rollback

- Risk: low. Closure capture restructuring; tool bodies untouched. The `handoffProgress` holder change is mechanical (single-pointer indirection).
- Late-bound accessors (`meshClient` / `identity` / `sandboxName`) introduce a per-call function dispatch but no semantic change — values are still read from the same module-level mutables.
- Rollback: revert the single PR; no migration.

## Verification

| Step | Result |
|---|---|
| `npx tsc --noEmit` (cli) | clean |
| `npm run lint` (cli) | 32 warnings (was 30; +2 for new module's `any` annotations) |
| `npm test -- --run` (cli) | 454 pass / 2 skipped (unchanged) |
| `npm run build` (cli) | clean |
| `wc -l cli/src/plugin.ts` | 3233 |

## Remaining S15.f slices

One slice to cap:

- **S15.f.10** — `core/openclaw-commands/` cluster (~500 LOC). The OpenClaw slash commands (`registerCommand` / `registerProvider` / `registerCli`) and any remaining init-time cleanup. Closes §4.2 cap for `plugin.ts` (target: ~2700 LOC).

After f.10 the file should land below the §4.2 cap. S15.g `phase2-runtime-package-split` then relocates it to `runtimes/openclaw/src/index.ts`.

## Sign-offs

- Implementation: GitHub Copilot CLI (this slice).
- Review: pending CodeQL `Analyze (javascript-typescript)` on PR.
