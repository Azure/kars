# Phase 2 / S15.d.3 — `cli/src/commands/up.ts` AgentMesh deploy extraction

| Metadata    | Value                                                             |
|-------------|-------------------------------------------------------------------|
| Slice       | S15.d.3 (sub-slice 3 of 4 in `phase2-hotspot-up-cli` sub-train)   |
| Branch      | `phase2-hotspot-up-cli-d3`                                        |
| Date        | 2026-04-29                                                        |
| Sign-offs   | Core ✅, Security ✅                                              |
| Linked PRs  | #80 (S15.a), #81 (S15.b), #82 (S15.c), #83 (S15.d.1), #84 (S15.d.2) |

## Summary

Continues the §4.2 800-LOC enforcement on `cli/src/commands/up.ts`.

| File                                       | Pre-d.3 LOC | Post-d.3 LOC | Δ      |
|--------------------------------------------|-------------|--------------|--------|
| `cli/src/commands/up.ts`                   | 1296        | **1182**     | −114   |
| `cli/src/commands/up/agentmesh_deploy.ts`  | (new)       | 174          | +174   |

Extracts the AgentMesh infrastructure deploy phase (Inspektor Gadget eBPF
deploy, AgentMesh relay + registry deployment in-cluster, external-registry
shortcut, optional AGIC ingress publication) into
`cli/src/commands/up/agentmesh_deploy.ts`.

Caller in up.ts becomes a 9-line dispatch:

```ts
const { deployAgentMesh } = await import("./up/agentmesh_deploy.js");
const meshResult = await deployAgentMesh(
  { repoRoot, acr, acrLoginServer, baseName, rg, stepper },
  { globalRegistry: options.globalRegistry, exposeRegistry: options.exposeRegistry },
);
const registryMode = meshResult.registryMode;
const globalRegistryUrl = meshResult.globalRegistryUrl;
const globalRelayUrl = meshResult.globalRelayUrl;
```

The result triple `{ registryMode, globalRegistryUrl, globalRelayUrl }`
flows forward unchanged to:

- The Step 7 ClawSandbox CR creation (env-var injection).
- The end-of-deploy `saveContext()` call.

## Decomposition contract

`deployAgentMesh(ctx, options)` is a pure orchestration helper:

- **Inputs:** `{ repoRoot, acr, acrLoginServer, baseName, rg, stepper }`
  for context; `{ globalRegistry, exposeRegistry }` for option shape.
- **Outputs:** `{ registryMode: "local" | "global", globalRegistryUrl?,
  globalRelayUrl? }`.
- **Side effects:** identical to inline body — `kubectl gadget deploy`,
  `az acr import`, `kubectl create namespace agentmesh`, postgres
  credentials secret, `kubectl apply -f` on the patched manifest,
  `kubectl wait` for relay + registry pods, optional ingress apply.
- **Tmp-file lifecycle preserved:** `.tmp-agentmesh.yaml` + 
  `.tmp-agentmesh-ingress.yaml` are written under `try`/`finally` and
  unlinked on both success and failure paths (verbatim from inline).

The outer-scope `const fs = await import("fs")` in `up.ts` is removed
(was only used by the AgentMesh manifest-substitution block, which is
now scoped to the new module). Inner deploy phases each declare their
own `fs` import inline as before.

## Existing implementation surveyed

- `cli/src/commands/up/fast_upgrade.ts` (S15.d.1) and
  `cli/src/commands/up/preflight.ts` (S15.d.2) — adjacent helpers, same
  dynamic-import pattern.
- `cli/src/stepper.ts` — `kvLine` reused. No re-implementation.
- `deploy/agentmesh.yaml` and `deploy/agentmesh-ingress.yaml` —
  consumed unchanged.
- `crypto.randomBytes` for postgres password — stdlib, same as before.

No duplication, no parallel implementation, no hand-rolled crypto, no
new network endpoints.

## Behavior delta

**None.** The body moved verbatim. Order of operations, kubectl apply
arguments, manifest substitution patterns (`azureclawacr.azurecr.io`,
`DOMAIN_PLACEHOLDER`, `SUBSCRIPTION_ID`, `RESOURCE_GROUP`), tmp-file
cleanup, `kubectl wait` timeouts, and the registry-mode triple flowing
forward are all byte-identical.

## Verification

| Check                                           | Result          |
|-------------------------------------------------|-----------------|
| `cd cli && npx tsc --noEmit`                    | ✅ clean        |
| `cd cli && npm run lint`                        | ✅ 27 warnings (baseline-matched), 0 errors |
| `cd cli && npm run build`                       | ✅ clean        |
| `cd cli && npm test -- --run`                   | ✅ 454 pass / 2 skipped |
| `up.ts` LOC ≤ pre-slice                         | ✅ 1296 → 1182  |

## Threat model

Unchanged. The agent-mesh deploy phase has the same Azure surface:

- `az acr import` of the postgres image (same arguments).
- `kubectl create secret generic agentmesh-db-credentials` (same
  randomBytes(24) base64url password generation).
- `kubectl apply` of `deploy/agentmesh.yaml` (same manifest).
- AGIC ingress apply with the same template substitutions.

No new credentials, no new manifests, no new substitution patterns.
The single removed line (the redundant `const fs = await import("fs")`
inside the inline AgentMesh block at original line 780) is replaced by
named-imports in the new module file, which use the same `node:fs`
named-import API surface as the rest of the repo.

## Tracker

- §4.2 budget for `cli/src/commands/up.ts`: 800. Post-d.3: 1182. Cap
  achievement deferred to **d.4** (sandbox bring-up + summary), which
  will extract the ~315-line ClawSandbox CR creation block.
- Subsequent sub-slices remaining: **d.4** only.

## Sign-offs

- Core ✅ — body moved verbatim; tests + tsc + lint + build green.
- Security ✅ — no threat-model surface change; no new dependencies; no
  new network or auth flows; `randomBytes(24)` and existing manifest
  patches preserved verbatim.
