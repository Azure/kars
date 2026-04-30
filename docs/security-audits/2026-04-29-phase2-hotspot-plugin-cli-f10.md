# 2026-04-29 — Phase 2 / S15.f.10 — `cli/src/plugin.ts` hotspot pass (final slice; §4.2 cap reached)

## Scope

Final S15.f sub-slice. Extracts the OpenClaw command / provider / CLI
registration cluster (lines 2447–3229 in the post-S15.f.9 plugin.ts)
into a dedicated module so `cli/src/plugin.ts` lands under the §4.2
Phase 2 cap of 3000 LOC.

## Existing implementation surveyed

- `cli/src/plugin.ts` post-S15.f.9 (3233 LOC) contained one remaining
  closure-bound cluster: the Foundry `api.registerProvider` call, the
  `api.registerCli(…)` registrar emitting `openclaw azureclaw
  {status,connect,dev,logs}`, and ~12 `api.registerCommand`
  slash-command (`/azureclaw …`) definitions. The cluster reads
  `foundryProject`, `agtMeshClient`, `agtIdentity`, `agtPolicy`,
  `agtTrustStore`, `agtAuditLogger`, `memorySyncBuffer`, and the
  `syncToFoundryMemory` helper, all defined module-level in
  `plugin.ts`. None of the cluster mutates AGT mesh state — it is all
  router HTTP queries plus UI plumbing.
- `cli/src/core/router-client.ts` already exports `routerCall`,
  `routerUrl`, and `routerBase`, which the cluster reaches for in
  multiple places. No new HTTP helper introduced.
- Late-bound-accessor pattern (`meshClient: () => agtMeshClient`,
  `getFoundryProject: () => foundryProject`) already proven in
  S15.f.8 / S15.f.9 for the same module-level mutables — reused here
  unchanged.
- Object-reference pattern for the `memorySyncBuffer: string[]`
  mutation (the `/azureclaw-switch` slash command flushes the buffer
  to Foundry memory before the model swaps) — array passed by
  reference; `.splice(0)` mutation propagates naturally.
- `cli/src/core/safe-json.ts` (S15.f.8) consumed for any
  router-response stringification (no body change).

## LOC delta

| File | Before | After | Δ |
|---|---|---|---|
| `cli/src/plugin.ts` | 3233 | **2463** | −770 |
| `cli/src/core/commands/openclaw.ts` (new) | 0 | 833 | +833 |
| **Cumulative S15.f** (`plugin.ts` 7139 → 2463) | | | **−4676** |

§4.2 Phase 2 cap = 3000 LOC. **plugin.ts is now 537 LOC under cap.**
Final S15.f sub-slice.

## Verification

- `npx tsc --noEmit` — clean.
- `npm run lint` — 33 warnings, 0 errors. Net +1 vs S15.f.9 — one
  pre-existing-style "import declared but unused" warning whose sole
  consumer was the extracted block; matches the pattern accepted in
  prior S15.f slices.
- `npm test -- --run` — 21 files / 454 pass / 2 skipped (unchanged).
- `npm run build` — clean.
- Container-name `"openclaw"` for the `register()`-emitted Pod is
  unaffected; the extracted module never references it.

## Risk + rollback

- Risk: low. Tool / command bodies are byte-identical to the previous
  inline registrations; the only change is closure capture replaced
  with explicit Deps threading, identical to the pattern proven by
  S15.f.8 (Foundry tools), S15.f.9 (stateful AGT tools), and earlier
  f.* slices. No public API surface change. Sandbox Dockerfile
  `COPY cli/src/ … cli/dist/ …` already includes the new
  `core/commands/` subdirectory tree — no Dockerfile patch required.
- Rollback: revert this PR. plugin.ts returns to 3233 LOC and the
  module file disappears. No data-plane / control-plane
  compatibility implications.

## Remaining slices (post-cap)

- **S15.g** `phase2-runtime-package-split` — mechanical move of the
  decomposed `cli/src/plugin.ts` + `cli/src/core/` → `runtimes/openclaw/`,
  per the plan.md addendum. Three sub-slices (g.1 plugin/core move,
  g.2 skills move, g.3 operator-CLI rename).
- §4.2 caps that remain: `inference-router/src/routes/handoff.rs`
  (1200 → 800).

## Sign-offs

- Implementer: Copilot CLI agent (S15.f.10 ship).
- Reviewer: pending PR review.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
