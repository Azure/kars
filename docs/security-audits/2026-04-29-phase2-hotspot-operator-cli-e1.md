# 2026-04-29 — Phase 2 / S15.e.1 — operator.ts type + helper extraction

**Slice:** `phase2-hotspot-operator-cli-e1`
**Branch:** `phase2-hotspot-operator-cli-e1` → `dev`
**Hotspot tracker:** `cli/src/commands/operator.ts` — Phase 2 cap **800** (§4.2)

## Scope

First sub-slice of S15.e (`operator.ts` decomposition). Lifts the
purely declarative + purely-functional pieces — module-level types
and the two module-level pure helpers — into dedicated files under
`cli/src/commands/operator/`.

S15.e is necessarily a multi-PR sub-train: `operator.ts` is
**2,894 LOC, 99% inside one giant `startDashboard` closure**. There
is no honest way to flip it under cap in a single review-able PR.
S15.e.1 is the lowest-risk first cut that establishes the
`operator/` directory pattern (already used for `keymap.ts`) for
follow-up sub-slices to extend.

## What moved

| File | Type | LOC moved |
|---|---|---|
| `cli/src/commands/operator/types.ts` (new) | pure interfaces / types | 118 |
| `cli/src/commands/operator/helpers.ts` (new) | pure functions | 51 |

Source declarations (verbatim):

- **Types** (originally `operator.ts:21-139`):
  `HealthState`, `SandboxInfo`, `EgressDomain`, `SecurityState`,
  `NodeInfo`, `ClusterHealth`, `MeshHealth`. All re-exported
  unchanged; each `interface` was promoted from internal to
  `export interface` (no shape edits).
- **Helpers** (originally `operator.ts:2851-2894`):
  `timeSince(date)`, `sumPrometheusCounter(text, metricName, labelFilter?)`.
  Both module-level (not nested in `startDashboard`); zero closure
  capture; byte-identical bodies.

`operator.ts` now imports these via `type { ... } from "./operator/types.js"`
and `{ timeSince, sumPrometheusCounter } from "./operator/helpers.js"`.

## LOC delta

| Slice | `operator.ts` | Δ | Cumulative |
|---|---|---|---|
| pre-S15.e | 2894 | — | — |
| **S15.e.1** | **2739** | **−155** | **−155** |
| §4.2 cap | 800 | | (cap not yet met; S15.e.2+ pending) |

## Behavior delta

**None.** Types are byte-identical. Both helpers' bodies are
byte-identical. No call sites touched. No public surface change
(types were never exported; their visibility is now wider but
no consumer outside `operator.ts` exists today — verified via
`rg 'SandboxInfo|EgressDomain|SecurityState|NodeInfo|ClusterHealth|MeshHealth|HealthState' cli/src/`).

## Verification

- `npx tsc --noEmit` → clean.
- `npm run lint` → 27 warnings, 0 errors (baseline).
- `npm run build` → clean.
- `npm test -- --run` → **454 / 454 passing** (2 skipped pre-existing).

## §0.2 hard-rule check

- ✓ no new TODOs / `unimplemented!` / `panic!` / stubs
- ✓ no new file exceeds Phase 2 cap (`types.ts` 128, `helpers.ts` 65)
- ✓ touched hotspot file shrank (`operator.ts` 2894 → 2739)
- ✓ no custom-crypto added (none touched)
- ✓ no duplication — types + helpers had a single home (operator.ts);
  moved to a single new home (`operator/`); no parallel implementation
- ✓ no dead code carried — superseded declarations removed in this slice
- ✓ existing implementation surveyed — `operator/keymap.ts` was the
  only prior decomposition; this slice extends the same pattern; no
  new abstraction layer introduced

## Sign-off

| Lens | Verdict |
|---|---|
| Core — correctness, completeness, regression risk | ✅ pure type/helper move, no behavior change |
| Security — threat model, secrets, crypto, attack surface | ✅ no new attack surface; no secrets / crypto / RBAC touched |

## Tracker — §15 hotspot status (post-merge state)

| File | Pre-Phase 2 | Cap | Status |
|---|---|---|---|
| `cli/src/commands/handoff.ts` | 1119 | 800 | ✅ S15.a (798) |
| `cli/src/commands/mesh.ts` | 1583 | 800 | ✅ S15.b (667) |
| `inference-router/src/routes/inference.rs` | 1359 | 800 | ✅ S15.c (776) |
| `cli/src/commands/up.ts` | 1849 | 800 | ✅ S15.d (766) |
| `cli/src/commands/operator.ts` | 2894 | 800 | 🔄 **S15.e.1 (2739)** — multi-PR sub-train |
| `cli/src/commands/plugin.ts` | 7139 | 800 | pending S15.f |

## Follow-ups

S15.e.2+ candidate seams (each is its own PR):

- `fetchSandboxes*` (Docker + AKS, ~240 LOC) → `operator/fetchers/sandboxes.ts`
- `fetchSecurityState` (~254 LOC) → `operator/fetchers/security.ts`
- `fetchEgressDomains` + `fetchAgtQuick` + `fetchClusterHealth` + `fetchMeshHealth` (~280 LOC combined) → `operator/fetchers/`
- `renderSecurity` + `renderAGT*` + `renderTopology` + `renderCluster` rendering helpers (~600 LOC) → `operator/render/`
- `approveDomain` / `denyDomain` / `enforceEgress` / `learnEgress` action helpers (~90 LOC) → `operator/actions.ts`
- Modal / dialog interaction helpers (`startEdit`, `launch`, `connectToAgent`) — ~700 LOC, needs context object pattern (similar to `up.ts` S15.d.4 `SandboxBringUpContext`)

Each sub-slice should land independently behind its own audit doc;
target is `operator.ts` ≤ 800 LOC by S15.e close-out.
