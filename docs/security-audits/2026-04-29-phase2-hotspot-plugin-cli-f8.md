# Phase 2 hotspot — `cli/src/plugin.ts` S15.f.8 (Foundry + http_fetch tool registrations)

**Slice:** S15.f.8 — sub-slice of `phase2-hotspot-pass3` per `docs/implementation-plan.md` §4.2.
**Date:** 2026-04-29.
**Branch:** `phase2-hotspot-plugin-cli-f8`.
**Cap target (Phase 2):** `cli/src/plugin.ts` ≤ 3000 LOC.

## Scope

Extract the cluster of 10 `api.registerTool` blocks that register the Foundry-shim tools and the `http_fetch` egress-proxy tool from `cli/src/plugin.ts` into a new `cli/src/core/agt-tools/` directory. The shared `safeJson` pretty-printer (previously inlined inside `register()`) is lifted to `cli/src/core/safe-json.ts` so cluster modules consume it as a normal module-level helper.

This is the eighth sub-slice of S15.f, the multi-PR train decomposing `cli/src/plugin.ts` toward the §4.2 cap.

## LOC delta

| File | Before | After | Δ |
|---|---:|---:|---:|
| `cli/src/plugin.ts` | 5071 | 4323 | **−748** |
| `cli/src/core/safe-json.ts` (new) | 0 | 12 | +12 |
| `cli/src/core/agt-tools/http-fetch.ts` (new) | 0 | 39 | +39 |
| `cli/src/core/agt-tools/foundry.ts` (new) | 0 | 763 | +763 |

Cumulative S15.f progress: 7139 → **4323 LOC, −2816, 78% to §4.2 cap of 3000.**

## What moved

Ten tool registrations (lines 3522–4281 of pre-f.8 plugin.ts) were excised wholesale; their bodies are byte-identical inside the new modules.

| Tool | Lines pre-f.8 | New home |
|---|---:|---|
| `http_fetch` | 3526–3554 | `core/agt-tools/http-fetch.ts` |
| `foundry_code_execute` | 3559–3614 | `core/agt-tools/foundry.ts` |
| `foundry_image_generation` | 3617–3691 | `core/agt-tools/foundry.ts` |
| `foundry_web_search` | 3696–3766 | `core/agt-tools/foundry.ts` |
| `foundry_file_search` | 3770–3873 | `core/agt-tools/foundry.ts` |
| `foundry_memory` | 3876–4033 | `core/agt-tools/foundry.ts` |
| `foundry_conversations` | 4036–4115 | `core/agt-tools/foundry.ts` |
| `foundry_evaluations` | 4118–4174 | `core/agt-tools/foundry.ts` |
| `foundry_deployments` | 4177–4246 | `core/agt-tools/foundry.ts` |
| `foundry_agents` | 4249–4281 | `core/agt-tools/foundry.ts` |

`plugin.ts` now invokes the cluster via two helper calls:

```ts
registerHttpFetchTool(api);
registerFoundryTools(api, {
  log,
  config,
  getFoundryProject: () => foundryProject,
});
```

## Deps surface (param threading)

`FoundryToolsDeps` carries the three pieces of `register()` state the foundry tools touch:

| Field | Why |
|---|---|
| `log` | Logger used by `foundry_memory` (5 sites) and one debug line in `foundry_file_search`. |
| `config` | Only `config.model` is read (one site in `foundry_deployments`). |
| `getFoundryProject` | **Late-bound** accessor returning the module-level `foundryProject` mutable. `initFoundry()` is fired-and-forgotten alongside `initAGT()` in `register()`, so `foundryProject` is potentially `null` at registration time and populated later. The accessor pattern preserves the previous semantics where each tool execution observed the current value. |

`http_fetch` has no plugin.ts state coupling — `registerHttpFetchTool(api)` takes only `api`.

## Operational invariants preserved

- **Tool surface unchanged.** Names, descriptions, parameter schemas, and execute return shapes are byte-identical to the previous inline registrations. The vendored extension manifest in `~/.openclaw-data/extensions/azureclaw/` continues to surface the same 10 tools with the same metadata.
- **Egress posture unchanged.** `http_fetch` continues to route through the inference router's egress proxy on `127.0.0.1:8443`; iptables egress-guard for UID 1000 untouched (`sandbox-images/openclaw/entrypoint.sh:713-747`).
- **Foundry router endpoints unchanged.** Every `routerCall` / `_routerCall` invocation in the extracted tools keeps the original method, path, body, and timeout.
- **safeJson pretty-printer unchanged.** Same 8000-byte default cap, same truncation marker.

## Risk + rollback

- Risk: low. Closure capture restructuring; tool bodies untouched.
- The only behavioural-shaped change is the `Object is possibly 'null'` adjustment in `foundry_deployments` cached-discovery branch (assigned the result of `getFoundryProject()` to a local before the optional-chain check) — semantically identical to the prior inline read.
- Rollback: revert the single PR; no migration.

## Verification

| Step | Result |
|---|---|
| `npx tsc --noEmit` (cli) | clean |
| `npm run lint` (cli) | 30 warnings (unchanged) |
| `npm test -- --run` (cli) | 454 pass / 2 skipped (unchanged) |
| `npm run build` (cli) | clean |
| `wc -l cli/src/plugin.ts` | 4323 |

## Remaining S15.f slices

Per the post-f.8 inventory, two slices to cap:

- **S15.f.9** — stateful AGT tools cluster (~1021 LOC):
  - `core/agt-tools/spawn.ts`: 4 spawn tools (`azureclaw_spawn`, `azureclaw_spawn_status`, `azureclaw_spawn_destroy`, `azureclaw_spawn_list`).
  - `core/agt-tools/mesh.ts`: 4 mesh tools (`azureclaw_mesh_send`, `azureclaw_mesh_inbox`, `azureclaw_mesh_transfer_file`, `azureclaw_discover`).
  - `core/agt-tools/handoff.ts`: 3 handoff tools (`azureclaw_handoff_status`, `azureclaw_handoff_request`, `azureclaw_handoff_confirm`).
- **S15.f.10** — `core/openclaw-commands/` cluster (~500 LOC, slash commands + `registerCommand`/`registerProvider`/`registerCli`).

After f.10 the file should land at ~2800 LOC, closing the §4.2 hotspot for `plugin.ts`. S15.g `phase2-runtime-package-split` then relocates it to `runtimes/openclaw/src/index.ts`.

## Sign-offs

- Implementation: GitHub Copilot CLI (this slice).
- Review: pending CodeQL `Analyze (javascript-typescript)` on PR.
