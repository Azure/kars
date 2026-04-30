# 2026-04-29 — Phase 2 / S15.e.2 — operator.ts sandbox-list fetcher extraction

**Slice:** `phase2-hotspot-operator-cli-e2`
**Branch:** `phase2-hotspot-operator-cli-e2` → `dev`
**Hotspot tracker:** `cli/src/commands/operator.ts` — Phase 2 cap **800** (§4.2)

## Scope

Second sub-slice of S15.e. Lifts the three sandbox-list fetcher
functions (`fetchSandboxes`, `fetchSandboxesAKS`, `fetchSandboxesDocker`)
out of the giant `startDashboard` closure into a sibling module.
Also moves the 2-line module-level `kctl(args, context?)` helper to
`operator/helpers.ts` so the new fetcher module can reuse it.

## What moved

| File | Type | LOC moved |
|---|---|---|
| `cli/src/commands/operator/fetchers/sandboxes.ts` (new) | data-fetching | 286 |
| `cli/src/commands/operator/helpers.ts` (existing) | `kctl()` (4 LOC added) | 4 |

`operator.ts` imports `fetchSandboxes` from the new module; the AKS
+ Docker variants are exported but used only internally by
`fetchSandboxes`, so the top-level module pulls in just the unified
entry point.

## Behavioral delta

**None.** Bodies are byte-identical to the originals apart from one
mechanical edit: the closure-captured outer-scope `kubeContext` is now
passed as an explicit parameter (`fetchSandboxes(kubeContext)`,
`fetchSandboxesAKS(kubeContext)`). The single call site in
`startDashboard.refresh()` was updated to pass `kubeContext`
(previously closure-captured); semantics are identical.

The 2-line `kctl(args, context?)` helper is byte-identical (still:
`return context ? ["--context", context, ...args] : args;`).

## LOC delta

| Slice | `operator.ts` | Δ | Cumulative |
|---|---|---|---|
| pre-S15.e | 2894 | — | — |
| S15.e.1 | 2739 | −155 | −155 |
| **S15.e.2** | **2483** | **−256** | **−411** |
| §4.2 cap | 800 | | (cap not yet met; multi-PR sub-train continues) |

## Verification

- `npx tsc --noEmit` → clean.
- `npm run lint` → 27 warnings, 0 errors (baseline).
- `npm run build` → clean.
- `npm test -- --run` → **454 / 454 passing** (2 skipped pre-existing).

Removed unused `HealthState` import from operator.ts (now only used
inside the extracted module). All other types remain consumed by the
remaining in-file rendering / action / dialog code.

## §0.2 hard-rule check

- ✓ no new TODOs / `unimplemented!` / `panic!` / stubs
- ✓ no new file exceeds Phase 2 cap (`fetchers/sandboxes.ts` 287 LOC)
- ✓ touched hotspot file shrank (`operator.ts` 2739 → 2483)
- ✓ no custom-crypto added (none touched)
- ✓ no duplication — local `kctl` removed; the new helpers.ts
  `kctl` is the sole owner; no second copy
- ✓ no dead code carried — original three fetcher functions deleted
  in this slice
- ✓ existing implementation surveyed — `operator/types.ts` +
  `operator/helpers.ts` (S15.e.1) consumed; no parallel abstraction

## Sign-off

| Lens | Verdict |
|---|---|
| Core — correctness, completeness, regression risk | ✅ pure move + one mechanical param-pass; no behavior change |
| Security — threat model, secrets, crypto, attack surface | ✅ no new attack surface; no secrets / crypto / RBAC touched; Docker-only `printenv` exec scope unchanged |

## Tracker — §15 hotspot status (post-merge state)

| File | Pre-Phase 2 | Cap | Status |
|---|---|---|---|
| `cli/src/commands/handoff.ts` | 1119 | 800 | ✅ S15.a (798) |
| `cli/src/commands/mesh.ts` | 1583 | 800 | ✅ S15.b (667) |
| `inference-router/src/routes/inference.rs` | 1359 | 800 | ✅ S15.c (776) |
| `cli/src/commands/up.ts` | 1849 | 800 | ✅ S15.d (766) |
| `cli/src/commands/operator.ts` | 2894 | 800 | 🔄 **S15.e.2 (2483)** — multi-PR sub-train |
| `cli/src/commands/plugin.ts` | 7139 | 800 | pending S15.f |

## Follow-up sub-slices (each its own PR)

- `fetchSecurityState` (~254 LOC) → `operator/fetchers/security.ts`
- `fetchEgressDomains` + `fetchAgtQuick` + `fetchClusterHealth` + `fetchMeshHealth` → `operator/fetchers/`
- Render helpers (`renderSecurity`, `renderAGT*`, `renderTopology`, `renderCluster`) → `operator/render/`
- Action helpers (`approveDomain`/`denyDomain`/`enforceEgress`/`learnEgress`) → `operator/actions.ts`
- Modal/dialog (`startEdit`, `launch`, `connectToAgent`) — context-object pattern (similar to S15.d.4)


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
