# Security Audit — `kars upgrade` flow fixes + security-audit gate relocation (v0.1.21)

Date: 2026-06-27
Scope: `cli/src/commands/upgrade.ts`, `cli/src/commands/up.ts`, `cli/src/commands/up/fast_upgrade.ts`, `cli/src/lib/version.ts` (new), `.github/workflows/release-public-interim.yml`, `ci/security-audit-required.sh`, plus tests.
Gated paths: `cli/src/commands/upgrade.ts`, `cli/src/commands/up.ts`, `cli/src/commands/up/fast_upgrade.ts`.

## Summary

A field report surfaced multiple bugs in `kars upgrade`. Root-caused the whole
flow (target resolution → version detection → image import → Helm → rollout) and
fixed six issues, and relocated the security-audit gate to a tracked folder.

1. **`--to latest` / `--to stable` was broken (recovery blocker).** The literal
   string `"latest"` was passed into `compareVersions`, which treats an
   unparseable tag as *older* than the current version, so the command wrongly
   refused with "Cluster is NEWER … use --force to downgrade" — making it
   impossible to upgrade to the latest release. New `resolveTargetVersion()`
   resolves `latest`/`stable` (and the no-`--to` default) to the newest
   published GitHub release, validates an explicit `--to` as a real version tag,
   and surfaces a clear error otherwise.

2. **`helm upgrade` reset chart values to defaults + `:latest` made rollback a
   no-op (CRITICAL).** Neither `buildHelmUpgradeArgs` nor the `kars up --upgrade`
   fast path passed `--reuse-values`, and the chart referenced `:latest`
   everywhere. So (a) every unspecified value reverted to the chart default on
   each upgrade (wiping per-runtime images + Foundry config), and (b) because
   tags never changed, `helm upgrade --wait` couldn't catch a bad image and
   `helm rollback` was a `:latest`→`:latest` no-op. Fix: added `--reuse-values`
   to both paths AND pinned image tags to the target version
   (`controller/router/sandbox` + all 7 runtimes set explicitly to
   `<acr>/<repo>:<version>`), with the version-tagged image now a REQUIRED ACR
   import. Now a changed tag makes `--wait`/`--atomic` genuinely gate the upgrade
   and `helm rollback` restores the previous version's tag. Health verification
   was also strengthened to check ImagePullBackOff/CrashLoopBackOff and to FAIL
   the command (was a silent warning).

3. **Version detection reported `v0.1.0`.** The chart `appVersion` is a static
   `0.1.0` sentinel that is never bumped, and `kars up` never stamped the real
   `karsRelease` Helm value (only `kars upgrade` did) — so every freshly
   provisioned cluster mis-reported `v0.1.0`, which also tripped the false
   downgrade guard. Now `kars up` / `kars up --upgrade` stamp
   `karsRelease=v<cliVersion>` (the CLI package version == the release tag), and
   `detectCurrentVersion` treats the `0.1.0` sentinel as "unknown".

4. **AgentMesh relay + registry were never refreshed on upgrade.** They are
   standalone Deployments in the `agentmesh` namespace (applied from a manifest,
   not Helm-managed), so `helm upgrade` never touches them and `rolloutRestartAll`
   didn't restart them — leaving the mesh on the pre-upgrade image. Since the
   whole stack pins `:latest` + `imagePullPolicy: Always`, a rolling restart is
   the ONLY thing that pulls new images; `rolloutRestartAll` now also restarts
   the `agentmesh` namespace and waits for it.

5. **`kars-runtime-langgraph-ts` was never published.** It is a real, first-class
   runtime (CRD LangGraph TypeScript flavour, chart value, controller env,
   `sandbox-images/langgraph-ts/Dockerfile`, and the CLI build/import paths all
   reference it), but the release workflow's runtime matrix omitted it — so
   `ghcr.io/azure/kars-runtime-langgraph-ts:<version>` 404s. Added it to the
   release publish matrix.

6. **Security-audit gate relocated.** The launch hygiene change untracked
   `docs/internal/` (now `.gitignore`d), which left `ci/security-audit-required.sh`
   pointing at a folder no PR can cleanly add to. Repointed the gate at the
   **tracked** `docs/security-audits/` folder (with a README + template), so the
   audit record is committed normally with the PR.

7. (Flagged, not fixed) **`a2aGateway`** default image is
   `karsacr.azurecr.io/kars-a2a-gateway` and the image is never imported into the
   user's ACR; it is `enabled: false` by default with no CLI wiring to enable it,
   so it cannot bite a default deployment. Tracked for a follow-up.

## Deferred architecture follow-ups (filed as issues)
Two independent expert reviews (K8s/Helm + system architecture) of this change
identified deeper foundational items, deferred to their own issues so this fix
stays focused and shippable:
- **#470** — pin images by digest (not tag) for reproducible, signature-verified
  deploys (supply-chain posture; needs security-architecture sign-off).
- **#471** — unify the duplicated image lists into one `KARS_IMAGES` source of
  truth + a CI parity test against GHCR (the class of bug behind langgraph-ts).
- **#472** — recovery affordances: in-cluster context ConfigMap (cross-machine
  upgrade/rollback), pending-release unstick, `--repair`, `--allow-downgrade`.

## T1: New capability / attack surface? (NO)
- No new role, endpoint, privilege, or network path. All changes are control-flow
  in the operator-run `kars upgrade` / `kars up` CLI (Helm args, kubectl rollout
  targets, GitHub-release tag resolution), one CI matrix entry, and a CI-gate
  path change. No change to what runs in a sandbox or what an agent can reach.

## T2: Security-control change? (NEUTRAL / hardening)
- `--reuse-values` *preserves* the security-relevant values the original
  `kars up` set (workload-identity client id, key-vault name, mesh provider,
  network policy) instead of silently resetting them — a correctness + safety
  improvement. The explicit `--set` still pins the image repos to the user's own
  ACR (no external registry is introduced).
- `resolveTargetVersion` only accepts `latest`/`stable` or a strict
  `v?MAJOR.MINOR.PATCH[-pre]` tag (`parseVersionTag`); arbitrary `--to` strings
  are rejected, so nothing attacker-influenced flows into the image source.
- The `agentmesh` rollout restart targets a fixed namespace/`--all` with no
  user-supplied input. `karsRelease` is stamped from the CLI's own
  `package.json` version (not external input).
- The security-audit gate itself is *unchanged in strength*: it still requires a
  two-signoff audit doc for capability PRs; only the folder moved from an
  (untrackable) gitignored path to a tracked one — so the control is now
  enforceable again rather than silently un-satisfiable.

## T3: Availability / fail-open risk? (REDUCED)
- Fixes a hard `kars upgrade` failure mode (can't reach latest; runtime/Foundry
  config wiped; stale mesh). `--atomic` is retained so a failed Helm upgrade
  still auto-rolls-back; `kars upgrade --rollback` reverts to the prior Helm
  revision. The `agentmesh` rollout + health checks are best-effort
  (`.catch(() => {})`), so they never wedge the command.
- Known limitation documented: `--reuse-values` does not pick up *new* chart
  defaults added in a later chart version (Helm semantics); the critical image
  values are always re-`--set`, and a future move to `--reset-then-reuse-values`
  (Helm ≥3.14) can tighten this.

## Verification
- CLI: `tsc --noEmit` + oxlint clean. Full vitest suite green including new
  coverage for the upgrade health/safety/recovery surface:
  `resolveTargetVersion`, `buildHelmUpgradeArgs` (`--reuse-values`, ACR image
  overrides, `karsRelease` stamp, `--atomic`, conditional Foundry endpoint),
  `detectCurrentVersion` (honest "unknown" so a broken cluster can re-upgrade),
  `rolloutRestartAll` (controller + sandboxes + agentmesh), `verifyHealth`, the
  `--rollback` arg shape, and `cliVersion`/`cliReleaseTag`.
- All `ci/*.sh` gates pass locally (loc, no-stubs, no-custom-crypto,
  no-null-provider-prod, a2a-module-isolation, copyright-headers,
  security-audit-required against the new folder).
- Release workflow YAML validated; langgraph-ts builds the same way the existing
  source path already builds it.

## Verdict
Accept. Fixes the `kars upgrade` recovery blocker and a critical value-reset
bug, makes version detection honest, refreshes the mesh on upgrade, closes the
langgraph-ts publish gap, and makes the security-audit gate enforceable again.
No security control is weakened.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
