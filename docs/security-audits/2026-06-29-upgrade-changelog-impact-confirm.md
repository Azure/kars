# Security Audit — `kars upgrade` changelog + impact table + confirm (additive UX)

Date: 2026-06-29
Scope: `cli/src/commands/upgrade.ts`, `cli/src/lib/release.ts`, `cli/src/lib/release.test.ts`.
Gated paths: `cli/src/commands/upgrade.ts`.

## Summary

Re-lands the additive, non-conflicting parts of the stale PR #457 on top of the
current (v0.1.21) hardened `kars upgrade` flow, **without touching the repaired
write path** (image import → atomic Helm upgrade → mesh-first rolling restart →
health-gated success → `--atomic`/rollback). Four read-only UX additions:

1. **Changelog summary** — before the confirm, prints the annotated tag messages
   for the releases between current and target (`fetchRecentReleases`,
   `releasesBetween`, `fetchTagMessage`, `summarizeChangelog`).
2. **Impact table** — reads the live cluster (`kubectl get deployment …`) and
   lists the controller + sandboxes that will be rolling-restarted, with
   readiness and running image.
3. **Y/N confirmation** — an interactive prompt before any write. Auto-proceeds
   under `--yes` or a non-TTY stdin, so existing automation is unaffected.
4. **Pre-flight node-readiness gate** — `kubectl get nodes`; hard-blocks (with
   guidance, no changes made) only when **every** node is NotReady, where the
   upgrade would otherwise burn minutes and time out.

Plus a **version-detection fallback**: when the controller runs `:latest` and no
`karsRelease` stamp exists (a cluster from before the stamp), match the running
image digest against published release digests to recover the real "Current:"
version. It is inserted **only as a new fallback step** — it never overrides the
existing image-tag or stamped-value detection.

## T1: New capability / attack surface? (NO)
- No new endpoint, route, privilege, credential, or write path. All additions
  are **read-only**: `kubectl get` (nodes/deployments) and anonymous public
  GitHub / GHCR REST reads (release notes, tag messages, public manifest
  digests). No tokens, no auth material, no mutating calls.
- The mutating upgrade sequence (`az acr import`, `helm upgrade --atomic`,
  rollout restarts, rollback) is **unchanged**. A human confirm now *gates* it;
  nothing new *performs* it.

## T2: Security-control change? (STRENGTHENED)
- Adds a confirmation gate and a fail-fast pre-flight check in front of the
  existing controls; removes none. The v0.1.21 post-upgrade health gate,
  value preservation (`--reuse-values`), and `--atomic` rollback are untouched.
- Version detection is more accurate (digest fallback) but strictly additive —
  it can only turn a previous "unknown" into a real version, never change a
  correct answer, so the "cluster is NEWER than target" downgrade guard behaves
  identically or better.

## T3: Availability / fail-open risk? (REDUCED)
- Every new call is best-effort and never throws: GitHub/GHCR reads fall back to
  "no notes"/"unknown"; `kubectl` read failures print a note and continue;
  unreadable nodes do **not** hard-block (only an all-NotReady cluster does, and
  that is a true pre-existing outage where the upgrade would fail anyway).
- Non-TTY / `--yes` auto-proceed preserves existing scripted/CI behaviour, so the
  new prompt cannot wedge automation.

## Verification
- CLI typecheck (`tsc --noEmit`) clean; `oxlint` 0 errors (no new warnings in the
  changed files); `npm run build` clean.
- `vitest`: 888 pass / 2 skipped (49 files). `release.test.ts` gains 6 tests —
  `releasesBetween` (3) and `summarizeChangelog` (3) — covering the changelog
  selection + parsing logic.
- The hardened write path, health gate, and rollback in `upgrade.ts` are
  byte-for-byte unchanged (additions only).

## Verdict
Accept. Read-only UX + an extra confirm/pre-flight in front of an unchanged,
already-hardened write path; more accurate version reporting via a strictly
additive fallback. No security control weakened.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
