# Phase 2 / S15.d.2 — `cli/src/commands/up.ts` preflight extraction

| Metadata    | Value                                                             |
|-------------|-------------------------------------------------------------------|
| Slice       | S15.d.2 (sub-slice 2 of 4 in `phase2-hotspot-up-cli` sub-train)   |
| Branch      | `phase2-hotspot-up-cli-d2`                                        |
| Date        | 2026-04-29                                                        |
| Sign-offs   | Core ✅, Security ✅                                              |
| Linked PRs  | #80 (S15.a), #81 (S15.b), #82 (S15.c), #83 (S15.d.1)              |

## Summary

Continues the §4.2 800-LOC enforcement on `cli/src/commands/up.ts`. Sub-slice
**d.1** (#83) extracted `--upgrade` to `up/fast_upgrade.ts` (1849 → 1660).
**d.2** (this slice) extracts the entire **preflight phase** (auto-detect dev
mode, cached-context prefill, banner + tool checks, Azure auth + subscription,
interactive prompts, RBAC + provider preflight, SKU availability check, and
the `--dry-run` plan print) into `cli/src/commands/up/preflight.ts`.

| File                                  | Pre-d.2 LOC | Post-d.2 LOC | Δ      |
|---------------------------------------|-------------|--------------|--------|
| `cli/src/commands/up.ts`              | 1660        | **1296**     | −364   |
| `cli/src/commands/up/preflight.ts`    | (new)       | 392          | +392   |

Caller is now four lines:

```ts
const { runPreflight } = await import("./up/preflight.js");
const preflightResult = await runPreflight(options);
if (preflightResult === null) return; // --dry-run
const { rg } = preflightResult;
```

## Decomposition contract

- `runPreflight(options)` mutates `options` in place when cached context or
  interactive prompts override `region` / `name` / `isolation` /
  `foundryEndpoint` / `openaiEndpoint` / `build`. Identical to pre-slice
  semantics — Commander's option object is the single source of truth and
  was already mutated in the original inline body.
- Returns `null` when `--dry-run` was taken (caller `return`s; no deploy).
- Returns `{ rg }` otherwise — derived once via
  `options.resourceGroup || \`azureclaw-${options.region}\`` and reused by
  the production-deploy section unchanged.
- May call `process.exit(1)` on hard failures (missing tools, SKU
  unavailable, RBAC preflight failure). Same exit semantics as inline.

`isValidAzureHost` moved to the new preflight module and re-exported; the
deploy section in `up.ts` imports it from `./up/preflight.js`. The single
remaining usage in the deploy section (line ~995) consumes the named import.

## Existing implementation surveyed

- `cli/src/preflight.ts` — `runPreflightChecks()` (Phase 1, RBAC + provider
  preflight) is **consumed unchanged**. No second copy.
- `cli/src/stepper.ts` — `banner` / `checkLine` reused. No re-implementation.
- `cli/src/config.ts` — `loadContext()` reused. No re-implementation.
- `cli/src/commands/up/fast_upgrade.ts` (S15.d.1) — adjacent module; same
  dynamic-import pattern, no shared state.

No duplication, no parallel implementation, no hand-rolled crypto or auth.

## Behavior delta

**None.** The preflight body moved verbatim. Order of operations, exit
codes, prompt copy, banner text, and dry-run plan output are byte-identical
to the pre-slice flow.

## Verification

| Check                                           | Result          |
|-------------------------------------------------|-----------------|
| `cd cli && npx tsc --noEmit`                    | ✅ clean        |
| `cd cli && npm run lint`                        | ✅ 27 warnings (baseline-matched), 0 errors |
| `cd cli && npm run build`                       | ✅ clean        |
| `cd cli && npm test -- --run`                   | ✅ 454 pass / 2 skipped |
| `up.ts` LOC ≤ pre-slice                         | ✅ 1660 → 1296  |

## Threat model

Unchanged. No change to:

- `runPreflightChecks` (RBAC + provider check) invocation arguments.
- `az login` / `az account show` / `az account list` / `az account set`
  invocations.
- `az vm list-skus` SKU check semantics.
- Foundry / OpenAI URL validation (`isValidAzureHost`).
- Cached-context read (`loadContext()`); no write-side changes here
  (`saveContext()` lives in deploy and is untouched).
- Reserved env-prefix handling: not applicable to preflight (deploy section).

The function is a pure orchestration of existing helpers with the same
inputs and outputs. No new network endpoints, no new credentials, no new
prompts.

## Tracker

- §4.2 budget for `cli/src/commands/up.ts`: 800 (cap not yet hit).
- Sub-slices remaining: d.3 (Helm + AgentMesh deploy phases), d.4 (sandbox
  bring-up + summary). Subsequent sub-slices will continue trimming toward
  800.
- §0.3 success-gate item "no file grew past its Phase 2 cap" — touched
  files shrank (§4.3 "touched code pays its decomposition debt"); cap
  achievement deferred to d.3/d.4 closure of S15.d.

## Sign-offs

- Core ✅ — body moved verbatim; tests + tsc + lint + build green.
- Security ✅ — no threat-model surface change; no new dependencies; no
  new network or auth flows; existing `isValidAzureHost` URL validator
  preserved verbatim.
