# Security Audit — `kars sre install` + `kars headlamp --install` work out-of-the-box

PR: Azure/kars (branch `fix/sre-headlamp-ootb`)

## Scope

Capability-path changes in `cli/src/commands/sre.ts`, `cli/src/commands/headlamp.ts`,
and `cli/src/commands/up/headlamp_stack.ts`.

A follow-up to the `--release` OOTB work (PR #428). After
`kars dev --release --target local-k8s` ran cleanly end-to-end from an
npm-installed CLI, two more user-facing commands still hard-required a repo
checkout:

- `kars sre install` → "Could not resolve the kars repo root (looked for
  deploy/helm/kars)". It resolved the kars Helm chart via a repo-root walk.
- `kars headlamp --install` → `findRepoRoot()` threw, and the shared
  `up/headlamp_stack.ts` (also used by the AKS observability path) located the
  Headlamp plugin + monitoring manifests under `repoRoot`.

Fix — resolve those assets via the bundled-asset resolver
(`lib/repo-assets.ts`, added in #428):

- `sre.ts`: the chart is now `requireBundledAsset("deploy/helm/kars")`; the
  repo-only `resolveRepoRoot()` is deleted.
- `headlamp_stack.ts`: `installKarsPlugin` prefers the prebuilt **bundled**
  plugin and only falls back to a repo source build; `installPrometheus`
  resolves `deploy/monitoring` via the resolver. `repoRoot` is now optional.
- `headlamp.ts`: `repoRoot` is resolved best-effort (`findRepoRootOrNull`) and
  passed through as optional.

All of these assets are already bundled into the package by
`scripts/bundle-deploy-assets.mjs`.

## Threat model

### T1: New asset source / trust surface? (NO)
The Helm chart, monitoring manifests, and Headlamp plugin are the project's own
templates, already bundled (and audited) in #428 — this PR only routes two more
commands through the same resolver. No new registry, credential, or input.

### T2: Path resolution — traversal? (NO)
`requireBundledAsset` / `resolveBundledAsset` are called with fixed string
constants ("deploy/helm/kars", "deploy/monitoring",
"tools/headlamp-plugin/dist/main.js") — never user input.

### T3: Behaviour change to `kars sre install` / observability? (NO, EQUIVALENT)
Same chart / same manifests applied to the same cluster; only the **source path**
of the static files changes (repo → bundled package). The SRE controller, RBAC,
and the Headlamp/Grafana stack are byte-identical to before. The plugin install
remains best-effort (skips with a warning if unavailable), so it can never block.

## Verdict

Accept. Two more user-facing commands now run from an npm install with no repo
checkout, resolving the same bundled, provenance-attested assets through the
existing resolver. No new attacker-reachable input, no runtime security-control
change. Verified by 810 tests, typecheck, lint, and a build that re-bundles the
assets.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
