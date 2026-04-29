# Phase 2 / S15.d.1 — `cli/src/commands/up.ts` fast-upgrade extraction

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-up-cli` (sub-slice **d.1** of S15.d)
**Sub-slice of:** S15 `phase2-hotspot-pass3` (§4.2 file-budget enforcement)

## Summary

Per `docs/implementation-plan.md` §4.2, `cli/src/commands/up.ts` carries a Phase 2 cap of **800 LOC**. Pre-slice: **1849 LOC**. The file is one giant action callback with deeply-nested shared state across preflight, infra provisioning, image build, Helm install, AgentMesh setup, and sandbox bring-up. Hitting 800 LOC requires a **multi-PR sub-train**, consistent with the plan's §15 note marking up.ts as a multi-step decomposition.

This first sub-slice (**d.1**) extracts the self-contained `--upgrade` fast-path — the only code path that returns early without touching the deploy pipeline — into `cli/src/commands/up/fast_upgrade.ts`.

| File | Pre | Post | Δ |
|---|---|---|---|
| `cli/src/commands/up.ts` | 1849 | **1660** | −189 |
| `cli/src/commands/up/fast_upgrade.ts` | (new) | 212 | +212 |

Subsequent sub-slices will tackle preflight (d.2), Helm + AgentMesh deploy phases (d.3), and sandbox bring-up + summary (d.4) to bring up.ts under the 800 cap.

## Existing implementation surveyed

(§0.2 #8 anti-duplication.)

- `cli/src/commands/up.ts` (1849 LOC) — sole owner of the `azureclaw up` command surface; nothing else implements the `--upgrade` fast-path.
- `cli/src/config.ts` `loadContext()` — consumed unchanged for cached-deployment lookup.
- No second copy of the fast-upgrade Helm-rerun + federated-credential sync exists.

## Decomposition

`cli/src/commands/up/fast_upgrade.ts` exports `runFastUpgrade(options)` which performs the entire previous `if (options.upgrade) { ... return; }` body verbatim:

1. Loads cached deployment context (exits if missing).
2. `az aks get-credentials` to refresh kubeconfig.
3. Walks up to find the Helm chart (`deploy/helm/azureclaw`).
4. `helm upgrade --install` with the cached `acrLoginServer` overrides + `--set adminToken=<re-fetched>`.
5. Re-applies `kubectl rollout restart` for the controller deployment.
6. Re-syncs federated credentials for every existing `ClawSandbox` so newly added sandboxes can authenticate.
7. Prints "Fast upgrade complete".

Caller (`up.ts`) reduced to:

```ts
if (options.upgrade) {
  const { runFastUpgrade } = await import("./up/fast_upgrade.js");
  await runFastUpgrade(options);
  return;
}
```

The dynamic import preserves the existing pattern of lazy-loading heavy deps so non-`up` commands have fast cold-start.

`up.ts` action-callback also dropped two now-unused dynamic imports (`ora`, top-level `path`) — both are still imported inside their respective scopes (fast_upgrade.ts owns `ora`; the deploy try-block already had its own `const path = await import("path")` shadowing the outer one).

## Verification

| Gate | Result |
|---|---|
| `cli/src/commands/up.ts` LOC | 1849 → **1660** (−189) |
| `npx tsc --noEmit` | clean |
| `npm test -- --run` | 454/454 pass (2 skipped pre-existing) |
| `npm run lint` | 27 warnings (matches baseline; 0 new), 0 errors |
| `npm run build` | success; `dist/` regenerated |

## Behavior delta

**None.** The `if (options.upgrade) { ... }` body moved verbatim into `runFastUpgrade(options)`. Outer-scope captures (`chalk`, `ora`, `execa`, `fs`, `path`, `existsSync`, `loadContext`) all become module-level imports of the new file.

`azureclaw up --upgrade` users see the same prompts (none), same Helm command, same federated-credential sync, same exit conditions.

## Threat-model considerations

No new attack surface. No change to:

- `loadContext()` lookup or filesystem trust boundary.
- `az aks get-credentials` / `helm upgrade` / `az identity federated-credential create` invocations.
- Admin-token retrieval (still fetched from `kubectl get secret`).
- Federated-credential issuer/audience (`api://AzureADTokenExchange`).

## Sign-offs

- Core: ✅ — pure extraction; behavior preserved; partial (sub-slice d.1) progress toward §15 cap.
- Security: ✅ — no change to deploy auth, kubeconfig handling, or FedCred audience; threat model unchanged.
