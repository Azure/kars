# Phase 2 / S15.d.4 — `cli/src/commands/up.ts` sandbox bring-up extraction (caps S15.d)

| Metadata    | Value                                                             |
|-------------|-------------------------------------------------------------------|
| Slice       | S15.d.4 (final sub-slice of `phase2-hotspot-up-cli` sub-train)    |
| Branch      | `phase2-hotspot-up-cli-d4`                                        |
| Date        | 2026-04-29                                                        |
| Sign-offs   | Core ✅, Security ✅                                              |
| Linked PRs  | #80, #81, #82, #83 (S15.d.1), #84 (S15.d.2), #85 (S15.d.3)        |

## Summary

Final sub-slice of the **S15.d** `phase2-hotspot-up-cli` multi-PR sub-train.
This slice extracts the entire sandbox bring-up phase (Step 7: federated
credentials + Foundry RBAC + ClawSandbox CR; Step 8: wait for Running +
WebUI port-forward; deployment summary + `saveContext()`) into
`cli/src/commands/up/sandbox_bringup.ts`.

**With this slice, `cli/src/commands/up.ts` reaches the §4.2 cap of 800 LOC
(actual: 766 LOC; pre-S15.d: 1849).**

| File                                       | Pre-d.4 LOC | Post-d.4 LOC | Δ      |
|--------------------------------------------|-------------|--------------|--------|
| `cli/src/commands/up.ts`                   | 1182        | **766** ✅   | −416   |
| `cli/src/commands/up/sandbox_bringup.ts`   | (new)       | 482          | +482   |

## Cumulative S15.d delta

| Sub-slice    | up.ts LOC     | Module created                                     | PR    |
|--------------|---------------|----------------------------------------------------|-------|
| pre-S15.d    | 1849          | —                                                  | —     |
| d.1          | 1660 (−189)   | `up/fast_upgrade.ts` (212 LOC)                     | #83   |
| d.2          | 1296 (−364)   | `up/preflight.ts` (392 LOC)                        | #84   |
| d.3          | 1182 (−114)   | `up/agentmesh_deploy.ts` (174 LOC)                 | #85   |
| **d.4**      | **766 (−416)**| `up/sandbox_bringup.ts` (482 LOC)                  | this  |
| **§4.2 cap** | **800**       | ✅ achieved                                         |       |

## Decomposition contract

`bringUpSandbox(ctx)` accepts a single `SandboxBringUpContext` carrying the
deploy-state trio (`options`, `baseName`, `rg`, `acrLoginServer`,
`foundryEndpoint`, `openAiEndpoint`, `kvName`, `wiClientId`, `imdsClientId`,
`repoRoot`, `stepper`, `registryMode`, `globalRegistryUrl`, `globalRelayUrl`).
It executes Step 7, Step 8, the summary, and `saveContext()` and returns
`Promise<void>` (final phase of the deploy try-block).

Caller in `up.ts` is a 9-line dispatch:

```ts
const { bringUpSandbox } = await import("./up/sandbox_bringup.js");
await bringUpSandbox({
  options,
  baseName, rg,
  acrLoginServer, foundryEndpoint, openAiEndpoint, kvName,
  wiClientId, imdsClientId,
  repoRoot, stepper,
  registryMode, globalRegistryUrl, globalRelayUrl,
});
```

The outer try/catch error-handler in up.ts wraps the call unchanged — any
exception inside `bringUpSandbox` bubbles to the same diagnostics block
(EncryptionAtHost / quota tips, `process.exit(1)`).

## Existing implementation surveyed

- `cli/src/commands/up/fast_upgrade.ts` (S15.d.1) — same dynamic-import pattern.
- `cli/src/commands/up/preflight.ts` (S15.d.2) — same `loadContext` consumer pattern.
- `cli/src/commands/up/agentmesh_deploy.ts` (S15.d.3) — same `Stepper` + execa pattern.
- `cli/src/stepper.ts` — `section`, `kvLine`, `checkLine` reused (now imported only by `sandbox_bringup.ts`; up.ts drops them).
- `cli/src/config.ts` — `saveContext()` reused (now imported only by `sandbox_bringup.ts`; up.ts drops it).

No duplication, no parallel implementation, no hand-rolled crypto, no new
network endpoints.

## Behavior delta

**None.** The sandbox bring-up body moved verbatim. Federated credential
arguments, MI Contributor scope, Foundry RBAC Bicep templates, ClawSandbox
CR shape (`apiVersion`, `kind`, `metadata`, `spec.runtime.openclaw.image`,
`spec.sandbox.isolation`, `spec.inference.*`, `spec.networkPolicy.*`,
`spec.governance.trustThreshold: 500`), wait-for-Running timeout (120s),
WebUI port-forward semantics (5s grace, detached spawn, 18789), summary
sections, `saveContext()` payload — all byte-identical.

The two redundant `const fs = await import("fs")` calls inside the
Foundry-RBAC sub-block are replaced by named imports of `node:fs` at the
top of `sandbox_bringup.ts`, matching the rest of the new modules.

## Verification

| Check                                           | Result          |
|-------------------------------------------------|-----------------|
| `cd cli && npx tsc --noEmit`                    | ✅ clean        |
| `cd cli && npm run lint`                        | ✅ 27 warnings (baseline-matched), 0 errors |
| `cd cli && npm run build`                       | ✅ clean        |
| `cd cli && npm test -- --run`                   | ✅ 454 pass / 2 skipped |
| `up.ts` LOC ≤ §4.2 cap (800)                    | ✅ 766          |

## Threat model

Unchanged. The sandbox bring-up phase has the same Azure surface:

- Federated-credential audience: `api://AzureADTokenExchange` (verbatim).
- MI Contributor scope: self-scoped to the sandbox identity (verbatim).
- Foundry RBAC role IDs: `53ca6127-…` (Azure AI User), `5e0bd9bd-…`
  (Cognitive Services OpenAI User) — verbatim.
- Bicep tmp-file cleanup: `try`/`finally unlinkSync` preserved.
- Trust threshold default: `500` (matches sandbox env default; verbatim).
- Content Safety + Prompt Shields: both `true` (verbatim).
- WebUI port-forward: detached spawn with `unref()`, same as before.

No new credentials, no new manifests, no new substitution patterns, no
change to the `saveContext()` payload schema.

## Tracker

- §4.2 budget for `cli/src/commands/up.ts`: 800 — **✅ achieved (766)**.
- §15 hotspot status:

  | File                                          | Pre-Phase 2 | Cap | Status |
  |-----------------------------------------------|-------------|-----|--------|
  | `cli/src/commands/handoff.ts`                 | 1119        | 800 | ✅ S15.a (798) |
  | `cli/src/commands/mesh.ts`                    | 1583        | 800 | ✅ S15.b (667) |
  | `inference-router/src/routes/inference.rs`    | 1359        | 800 | ✅ S15.c (776) |
  | `cli/src/commands/up.ts`                      | 1849        | 800 | ✅ **S15.d (766)** |
  | `cli/src/commands/operator.ts`                | 2894        | 800 | pending S15.e |
  | `cli/src/commands/plugin.ts`                  | 7139        | 800 | pending S15.f |

## Sign-offs

- Core ✅ — body moved verbatim; tests + tsc + lint + build green; up.ts
  reaches §4.2 cap.
- Security ✅ — no threat-model surface change; no new dependencies; no
  new network or auth flows; all RBAC role IDs, Bicep templates, and
  CR-shape fields preserved verbatim.
