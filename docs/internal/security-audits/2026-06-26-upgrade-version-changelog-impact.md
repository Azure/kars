# Security Audit — `kars upgrade` pre-flight UX: accurate version detection, changelog, impact table, confirm

Date: 2026-06-26
Scope:
- `cli/src/commands/upgrade.ts` — image-digest version detection, changelog summary,
  cluster impact table, Y/N confirmation.
- `cli/src/lib/release.ts` (+ `release.test.ts`) — `fetchRecentReleases`,
  `releasesBetween`, `fetchTagMessage`, `ghcrManifestDigests`.

Gated path (CI `security-audit-required`): `cli/src/commands/upgrade.ts`.

## Summary

UX hardening for `kars upgrade`, all **read-only** until the existing confirmed
write path. No new privileges or mutations are introduced.

1. **Accurate current-version detection.** Previously fell back to the chart's
   static `appVersion` (showed `v0.1.0` on any older cluster). Now resolves the
   real version, most-reliable first: (a) the `karsRelease` Helm value stamped by a
   prior upgrade; (b) **image-digest match** — read the controller pod's running
   image digest (`kubectl get pods … imageID`) and match it against the digests of
   recent published `kars-controller` release tags via the **public** GHCR registry
   (`az acr import` preserves content-addressed digests, so ACR==GHCR); (c) the
   static appVersion as last resort.

2. **Changelog summary.** Before the confirm, fetches recent releases (public
   GitHub API) and prints the annotated **tag messages** (the real feature
   changelog) for the versions between current and target.

3. **Impact table.** Reads the live cluster (controller + all sandbox Deployments
   across namespaces) and prints what will be rolling-restarted, with namespace,
   readiness, and running image — the blast radius — before the confirm.

4. **Y/N confirmation.** Interactive prompt before any write; auto-proceeds under
   `--yes` or a non-TTY (CI). `--dry-run` still previews and exits.

## T1: New capability / attack surface? (NO)
- All additions are reads: `kubectl get` (pods/deployments), unauthenticated GETs
  to `api.github.com` (public releases/tags) and `ghcr.io` (public image
  manifests via an anonymous pull token). No new write, no new credential, no new
  cluster permission. The only network egress targets are public Microsoft/GitHub
  endpoints already used by `kars up --release`.
- The confirmation gate **reduces** capability (a write now requires explicit
  consent in interactive mode).

## T2: Security-control change? (NEUTRAL / IMPROVED)
- The actual upgrade write path (image import → `helm upgrade --atomic` → rolling
  restart → verify, with `--rollback`) is unchanged from the merged `kars upgrade`.
- Adds a human confirmation step before mutation — a net safety improvement.
- GHCR/GitHub responses are only parsed for digests/tag strings; nothing from them
  is executed or used to construct privileged operations. Digest comparison is an
  exact `sha256:…` string match.

## T3: Availability / fail-open risk? (REDUCED)
- Every new read is best-effort with graceful fallback: digest detection failing →
  fall through to appVersion; GitHub/GHCR unreachable → empty changelog note;
  cluster read failing → "could not read workloads" note. None abort the upgrade
  or block on the network.
- Accurate version + visible blast radius + confirm make a production upgrade
  safer and less surprising.

## Verification
- CLI `tsc --noEmit` clean, oxlint 0 errors, **843 tests pass** (incl. new
  `releasesBetween` cases; version compare/image-plan unchanged).
- Validated live against `kars-aks`: digest helpers collect 5 manifest digests per
  tag; `releasesBetween("v0.1.15","v0.1.18")` → v0.1.16/17/18; changelog renders
  real bullets from tag messages; impact table renders controller + sandbox with
  namespace/readiness/image; `--dry-run` shows changelog + impact + plan with no
  changes.

## Verdict
Accept. Read-only pre-flight UX (accurate version, changelog, impact table) plus a
confirmation gate in front of the existing, unchanged upgrade write path. No new
attack surface; net safety improvement.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
