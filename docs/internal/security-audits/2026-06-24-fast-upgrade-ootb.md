# Security Audit — `kars up --upgrade` resolves the chart from the bundled package

PR: Azure/kars (branch `fix/fast-upgrade-ootb`)

## Scope

Capability-path change in `cli/src/commands/up/fast_upgrade.ts`.

Final follow-up to the `--release` OOTB work (#428, #432). `kars up --upgrade`
(fast Helm upgrade of an existing AKS deployment) located the kars Helm chart via
a cwd/CLI-relative walk and errored "Helm chart not found. Run from the kars repo
directory." when run from an npm-installed CLI with no checkout.

Fix: resolve the chart with `requireBundledAsset("deploy/helm/kars")` (repo-or-
bundled), removing the bespoke chart-finding walk. The chart is already bundled
into the package by `scripts/bundle-deploy-assets.mjs`.

## Threat model

### T1: New asset source / input? (NO)
Same chart already bundled + audited in #428; this only routes one more command
through the existing resolver, called with a fixed string constant
("deploy/helm/kars") — no user input, no new registry/credential.

### T2: Behaviour change? (NO, EQUIVALENT)
`helm upgrade` runs against the same chart with the same cached-context args; only
the chart's source path changes (repo walk → bundled package).

## Verdict

Accept. `kars up --upgrade` now works from an npm install with no repo checkout,
resolving the same bundled, provenance-attested chart through the existing
resolver. No new attacker-reachable input, no runtime security-control change.
Verified by typecheck + lint (0 warnings) + the OOTB resolver checks.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
