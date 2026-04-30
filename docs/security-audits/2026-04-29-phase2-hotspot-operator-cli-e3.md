# 2026-04-29 — Phase 2 / S15.e.3 — operator.ts security + cluster fetcher extraction

**Slice:** `phase2-hotspot-operator-cli-e3`
**Branch:** `phase2-hotspot-operator-cli-e3` → `dev`
**Hotspot tracker:** `cli/src/commands/operator.ts` — Phase 2 cap **800** (§4.2)

## Scope

Third sub-slice of S15.e. Lifts the five remaining data-fetching
functions out of the giant `startDashboard` closure, splitting them
across two new fetcher modules grouped by I/O target.

## What moved

| File | Functions | LOC |
|---|---|---|
| `cli/src/commands/operator/fetchers/security.ts` (new) | `fetchEgressDomains`, `fetchSecurityState`, `fetchAgtQuick` | 351 |
| `cli/src/commands/operator/fetchers/cluster.ts` (new) | `fetchMeshHealth`, `fetchClusterHealth` | 188 |

`operator.ts` now imports from both. The 4 call sites in
`startDashboard.refresh()` were updated to forward `kubeContext`
(and `devMode` for cluster fetchers); `fetchAgtQuick`'s
closure-captured `securityStates.get(sb.name)` is now passed
explicitly as the `existing: SecurityState | undefined` parameter.

## Behavioral delta

**None.** Bodies are byte-identical to the originals apart from
mechanical edits:

- `fetchEgressDomains(sb, kubeContext?)` — was closure-captured `kubeContext`; now explicit.
- `fetchSecurityState(sb, kubeContext?)` — was closure-captured `kubeContext`; now explicit.
- `fetchAgtQuick(sb, existing, kubeContext?)` — was closure-captured `securityStates.get(sb.name)` + `kubeContext`; now explicit. Mutation semantics identical (still mutates `existing` in place).
- `fetchMeshHealth(devMode, kubeContext?)` — was closure-captured `devMode` + `kubeContext`; now explicit.
- `fetchClusterHealth(devMode, kubeContext?)` — was closure-captured `devMode` + `kubeContext`; now explicit.

The 4 call sites in `startDashboard.refresh()` were updated to
forward these arguments; semantics identical.

## LOC delta

| Slice | `operator.ts` | Δ | Cumulative |
|---|---|---|---|
| pre-S15.e | 2894 | — | — |
| S15.e.1 (#87) | 2739 | −155 | −155 |
| S15.e.2 (#88) | 2483 | −256 | −411 |
| **S15.e.3** | **1960** | **−523** | **−934** |
| §4.2 cap | 800 | | (cap not yet met; multi-PR sub-train continues) |

Lint warnings dropped from 27 → **21** (the moved blocks accounted
for several `: any` casts that are now scoped tighter inside the
new modules).

## Verification

- `npx tsc --noEmit` → clean.
- `npm run lint` → 21 warnings, 0 errors (was 27).
- `npm run build` → clean.
- `npm test -- --run` → **454 / 454 passing** (2 skipped pre-existing).

## §0.2 hard-rule check

- ✓ no new TODOs / `unimplemented!` / `panic!` / stubs
- ✓ no new file exceeds Phase 2 cap (`security.ts` 351, `cluster.ts` 188)
- ✓ touched hotspot file shrank (`operator.ts` 2483 → 1960)
- ✓ no custom-crypto added (none touched)
- ✓ no duplication — the original five fetchers had a single home; the
  new modules are now their single home; no parallel implementation
- ✓ no dead code carried — all five originals deleted in this slice
- ✓ existing implementation surveyed — `operator/types.ts`,
  `operator/helpers.ts` (S15.e.1), `operator/fetchers/sandboxes.ts`
  (S15.e.2) reused; no new abstraction layer

## Sign-off

| Lens | Verdict |
|---|---|
| Core — correctness, completeness, regression risk | ✅ pure move + 5 mechanical param-passes; no behavior change; mutation semantics of `fetchAgtQuick` preserved |
| Security — threat model, secrets, crypto, attack surface | ✅ no new attack surface; same router endpoints (`/readyz`, `/blocklist/status`, `/agt/status`, `/egress/allowlist`, `/metrics`, `/agt/audit`, `/agt/reputation`) and same K8s checks (NetworkPolicy `sandbox-policy`, secret `router-admin-token`); no secrets / crypto / RBAC touched |

## Tracker — §15 hotspot status (post-merge state)

| File | Pre-Phase 2 | Cap | Status |
|---|---|---|---|
| `cli/src/commands/handoff.ts` | 1119 | 800 | ✅ S15.a (798) |
| `cli/src/commands/mesh.ts` | 1583 | 800 | ✅ S15.b (667) |
| `inference-router/src/routes/inference.rs` | 1359 | 800 | ✅ S15.c (776) |
| `cli/src/commands/up.ts` | 1849 | 800 | ✅ S15.d (766) |
| `cli/src/commands/operator.ts` | 2894 | 800 | 🔄 **S15.e.3 (1960)** — multi-PR sub-train |
| `cli/src/commands/plugin.ts` | 7139 | 800 | pending S15.f |

## Follow-up sub-slices (each its own PR)

- Render helpers (`renderSecurity`, `renderAGT*`, `renderTopology`,
  `renderCluster`, `renderHeader`, `render`) — collectively the largest
  remaining block (~700 LOC).
- Action helpers (`approveDomain`/`denyDomain`/`enforceEgress`/`learnEgress`)
  → `operator/actions.ts` (~90 LOC).
- Modal/dialog interaction (`startEdit`, `launch`, `connectToAgent`,
  `deleteSelectedAgent`) — context-object pattern (similar to
  S15.d.4 `SandboxBringUpContext`).


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
