# Security Audit — `--release` works out-of-the-box (bundle deploy assets; no repo checkout)

PR: Azure/kars (branch `fix/release-ootb-bundle-assets`)

## Scope

Capability-path changes in `cli/src/commands/dev/local-k8s.ts`,
`cli/src/commands/up.ts`, and `cli/src/commands/up/agentmesh_deploy.ts`, plus a
new helper `cli/src/lib/repo-assets.ts`, a build-time bundler
`cli/scripts/bundle-deploy-assets.mjs`, and tests.

`kars dev --release --target local-k8s` (and `kars up --release`) **failed out
of the box** when the CLI was installed from npm with no repo checkout: the K8s
flows resolved the Helm chart, AgentMesh manifest, bicep templates, monitoring
manifests, and the Headlamp plugin from a repo-relative `repoRoot`, and a hard
"must be run from inside the kars repo checkout" guard fired even in `--release`
mode (which pulls published images and needs no source).

Fix — make all three `--release` use cases (docker / local-k8s / aks) run with
**no source tree**:

1. **Bundle** the static deploy assets into the published package: the build
   copies `deploy/helm/kars`, `deploy/bicep`, `deploy/agentmesh-agt.yaml`,
   `deploy/agentmesh-ingress.yaml`, `deploy/monitoring`, and the prebuilt
   `tools/headlamp-plugin/dist/main.js` (+ its `package.json`) into `dist/`.
2. **Resolve** those assets via `resolveBundledAsset()` / `requireBundledAsset()`
   — repo checkout first, then the bundled copy — instead of `repoRoot` joins.
3. **Gate** the repo-only build guard and the AGT-toolkit/Python-wheel bootstrap
   on `!releaseMode` in local-k8s (release pulls published images).
4. **Relocate temp writes** (`up.ts` role bicep, `agentmesh_deploy.ts`
   manifest/ingress) from `repoRoot/.tmp-*` to `os.tmpdir()` so they don't
   require a writable source tree (an npm global dir is typically read-only).
5. Make the local-k8s observability extras (Headlamp / plugin / Prometheus)
   **best-effort** so they can never block an otherwise-running agent.

docker `--release` was already OOTB-clean (build is `!releaseMode`-gated; the
seccomp profile loads from the bundled `dist/profiles`; the plugin is baked into
the published sandbox image) — verified, no change needed.

## Threat model

### T1: Do bundled deploy assets add a trust/tamper surface? (NO)
The bundled chart/bicep/manifests/plugin are the project's **own** templates,
copied verbatim from the repo at build time and versioned with the CLI. They are
declarative K8s/bicep YAML + a prebuilt Headlamp UI plugin — no secrets, no
credentials. The published npm package is provenance-attested (SLSA) and
`npm audit`-clean, so the bundle's integrity rides on the same signing as the
CLI itself. AKS/kind still pull container images from the user's own ACR / public
GHCR exactly as before.

### T2: Path resolution — any traversal risk? (NO)
`resolveBundledAsset(relPath)` is only ever called with **fixed string
constants** ("deploy/helm/kars", etc.) — never user input. It checks a repo root
(walked up via marker files) then a path under the compiled bundle dir; both are
derived from the process, not from attacker-controlled data.

### T3: Temp-file relocation to os.tmpdir() — safe? (IMPROVEMENT)
Writing patched manifests/role-bicep to `os.tmpdir()` with a unique
`Date.now()`-suffixed name (then unlinking) avoids writing into the install dir
and matches the existing pattern used elsewhere. Contents are the same
non-secret, ACR-substituted templates as before.

### T4: Best-effort observability — does skipping reduce security? (NO)
Headlamp/Grafana are operator-convenience dashboards, not a security control.
Wrapping their install in try/catch only prevents a non-security add-on from
aborting a correctly-sandboxed, NetworkPolicy-isolated agent. The controller,
router, mesh, seccomp, and NetworkPolicy posture are unchanged.

## What this audit does NOT cover
- Image contents (covered by the build/sign audits).
- The full live AKS `kars up --release` deploy (to be smoke-tested on a real
  subscription — mechanically identical to the existing source-ACR import path).

## Verdict

Accept. The change ships the project's own deploy templates inside the
provenance-attested package and resolves them deterministically, removes a
hard repo-checkout requirement from the no-build `--release` paths, and moves
temp writes out of the install tree — with no new attacker-reachable input and
no change to the runtime security posture. Verified by 810 tests (incl. new
resolver + bundle-completeness regression tests), an out-of-repo resolver check,
typecheck, lint, and a tarball-contents check.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
