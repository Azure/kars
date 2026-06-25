# Security Audit — `kars upgrade` (failsafe cluster upgrade to a published release)

Date: 2026-06-25
Scope:
- NEW `cli/src/commands/upgrade.ts` (+ wired into `cli/src/cli.ts`)
- NEW `cli/src/lib/release.ts` (+ `release.test.ts`) — shared release image plan,
  version comparison, latest-release discovery.

Gated path (CI `security-audit-required`): `cli/src/commands/upgrade.ts`.

## Summary

Adds `kars upgrade`, a foolproof/failsafe path to move an EXISTING kars cluster to
a published GitHub release. Unlike `kars up --upgrade` (Helm-only re-run that
assumes the ACR already holds the new images), `kars upgrade`:

1. Connects to the cached cluster and verifies the `kars` Helm release exists.
2. Resolves the target release (latest GitHub release, or `--to <tag>`) and the
   current deployed version (stamped Helm value, fallback chart appVersion).
3. Skips when already at target (unless `--force`); refuses silent downgrade.
4. `--dry-run`: prints the plan (images + steps) and makes NO changes.
5. Imports the target release images into the user's ACR — `:latest` (what the
   chart references) AND the immutable `:<tag>` (for pin/rollback). Required
   images fail closed; optional runtime adapters degrade.
6. `helm upgrade --atomic` — a failed upgrade auto-rolls-back the release, so the
   cluster never lands half-migrated. CRDs are templated (not in `crds/`), so the
   chart upgrade updates them.
7. Rolling-restarts the controller + every sandbox Deployment (the inference-router
   is a sidecar, rolled with its pod) to pick up the new `:latest`.
8. Verifies controller availability and reports old → new.
9. `--rollback`: `helm rollback` to the previous revision + restart + verify.

## T1: New capability / attack surface? (NO — same operations as `kars up`)
- Every action is one the operator already performs via `kars up`: `az acr import`
  of the PUBLIC signed GHCR images into THEIR ACR, `helm upgrade`, and `kubectl
  rollout restart`. No new principal, secret, credential, scope, or network path.
- Image provenance is unchanged: the same `ghcr.io/azure/*` cosign-signed images
  imported by `kars up --release`. `kars upgrade` only pins them by tag as well.
- No change to sandbox runtime privileges, egress, seccomp, NetworkPolicy, or the
  inference-router auth model (still Entra/IMDS, no keys).
- Latest-release discovery is an unauthenticated GET to the public GitHub releases
  API; failure returns null (operator must pass `--to`). No token used.

## T2: Security-control change? (NEUTRAL / IMPROVED)
- `--atomic` adds a safety control: a failed upgrade is auto-rolled-back rather
  than leaving a partially-upgraded control plane. `--rollback` gives an explicit
  revert. Both reduce the blast radius of a bad release.
- The version stamp (`--set karsRelease=<tag>`) is a non-templated Helm value used
  only for reporting; it grants nothing and is not read by any policy.
- Required-image fail-closed prevents importing a partial image set and then
  upgrading onto missing images.

## T3: Availability / fail-open risk? (REDUCED)
- `--dry-run` lets an operator preview with zero writes.
- Already-at-target and newer-than-target guards prevent needless or destructive
  re-deploys.
- `--atomic` + `--rollback` make a failed upgrade recoverable instead of wedging.
- Rollout restart is bounded by `rollout status --timeout`; failures degrade to a
  warning pointing at `kars status`, never a silent success.

## Verification
- CLI: `tsc --noEmit` clean, oxlint 0 errors, **830 tests pass** (+9 new
  `release.ts` tests: version parse/compare incl. prerelease ordering, image plan
  with `-agt` mesh tags + runtime toggle, atomic Helm args).
- `fetchLatestReleaseTag()` validated against the live GitHub API (returned
  `v0.1.16`).
- `kars upgrade --dry-run` validated end-to-end against the live `kars-aks`
  cluster: connected, found the Helm release, resolved target `v0.1.16`, and
  printed the 12-image plan with NO changes made.

## Known limitation (documented, follow-up)
- Current-version detection is reliable only after the first `kars upgrade` (which
  stamps `karsRelease`); before that it falls back to the chart's static
  appVersion. The upgrade itself is idempotent regardless.

## Verdict
Accept. `kars upgrade` performs the same operator-scoped operations as `kars up`
on the operator's own ACR/cluster, adds atomic-upgrade + rollback safety controls,
and introduces no new attack surface or credential. Net posture: improved.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
