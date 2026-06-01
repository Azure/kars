# Pre-public release prep — keep everything private, then flip on day 1

**Date**: 2026-06-01
**Scope**: Concrete answers to three operator questions:
1. How do we **prepare the full release pipeline (npm + crates + Docker) and keep everything private** until public day?
2. How do we **onboard to Component Governance / Artemis**?
3. How does **Artemis fit into the CI for release**?

This supersedes the earlier 2026-06-01-first-release-plan.md briefing.

---

## TL;DR

| Question | Short answer |
|---|---|
| Keep npm private? | ✅ Yes — **GitHub Packages** (`https://npm.pkg.github.com`) for free org-internal staging. Flip to npmjs.com public on day 1. |
| Keep crates.io private? | ❌ Not possible on crates.io itself. ✅ Yes via **Azure Artifacts Cargo registry** (1ES-provisioned, internal-only) for staging. Use `cargo publish --dry-run` in CI for daily validation. |
| Keep Docker private? | ✅ **Already done** — `karsacr.azurecr.io` is private. Add a `ghcr.io/azure/kars/*` dry-run mirror (push as **internal-visibility package**) to validate the public-day registry too. |
| Onboard to Artemis? | File an internal request via the **Artemis portal** (`https://artemis.azurefd.net`) — needs Microsoft internal login + a 1ES sponsor. The portal walks through Component Governance enrollment for the repo. |
| Artemis in CI? | Today: GitHub Actions runs `cargo audit`, `cargo deny`, Dependabot, Trivy, Syft SBOM, cosign + notation. **All of this stays in GitHub Actions**. Artemis additionally consumes those signals via webhook + adds Microsoft-internal CVE / license / IP scans on a scheduled job. Release pipeline does NOT block on Artemis (it's an out-of-band compliance scanner), but **OSPO will not greenlight the public-visibility flip until the Artemis compliance dashboard for kars is green**. |

---

## Part 1 — Pre-public release pipeline (private staging across all three registries)

### 1.1 npm — staging via GitHub Packages, flip to npmjs.com on day 1

**Why GitHub Packages, not npmjs.com private packages**: GitHub Packages on `Azure/kars` is free, requires no separate org account, scopes visibility to `Azure` org members automatically, and the publish token is `${{ secrets.GITHUB_TOKEN }}` — no separate npm-token-rotation problem.

#### What to add to the repo

A new `.github/workflows/npm-publish.yml`:

```yaml
name: Publish npm packages

on:
  push:
    tags: ['v*']        # public-day publish
  workflow_dispatch:    # manual stage-publish to GHCR
    inputs:
      registry:
        type: choice
        options: [github-packages, npmjs-public]
        default: github-packages

permissions:
  contents: read
  packages: write       # GHCR push
  id-token: write       # npm --provenance attestation

jobs:
  publish:
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      matrix:
        package:
          - cli                       # @kars/cli
          - mesh-plugin               # @kars/mesh
          - runtimes/openclaw         # @kars/runtime-openclaw
          - runtimes/langgraph-ts     # @kars/runtime-langgraph-ts
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: '22'
          registry-url: ${{ inputs.registry == 'npmjs-public' && 'https://registry.npmjs.org' || 'https://npm.pkg.github.com' }}
          scope: '@kars'
      - name: Build
        working-directory: ${{ matrix.package }}
        run: npm ci && npm run build
      - name: Publish
        working-directory: ${{ matrix.package }}
        env:
          NODE_AUTH_TOKEN: ${{ inputs.registry == 'npmjs-public' && secrets.NPM_TOKEN || secrets.GITHUB_TOKEN }}
        run: |
          ACCESS=${{ inputs.registry == 'npmjs-public' && 'public' || 'restricted' }}
          npm publish --access $ACCESS --provenance
```

#### Pre-public usage

- Maintainer triggers the workflow manually via `workflow_dispatch` with `registry: github-packages`.
- Packages land at `https://github.com/orgs/Azure/packages?q=kars` — visible to anyone with `read:packages` on the org.
- Test the install path with a PAT: `npm install @kars/cli --registry https://npm.pkg.github.com`.

#### Public-day flip

- Create the `@kars` scope on npmjs.com (free, ~5 minutes for an org admin).
- Add `NPM_TOKEN` secret to the repo (publish-only, from `npm token create --read-only=false`).
- Tag `v0.1.0` → push → workflow auto-runs with `registry: npmjs-public` and `access: public`.

---

### 1.2 crates.io — Azure Artifacts staging + dry-run in CI, public publish on day 1

#### Today (zero cost)

Add a `cargo-publish-dry-run` job to `.github/workflows/release.yml`:

```yaml
  cargo-publish-dry-run:
    name: cargo publish --dry-run
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      matrix:
        crate:
          - kars-a2a-core
          - kars-eval-corpus
          - kars-a2a-gateway
          - kars-inference-router
          - kars-controller
          - kars-conformance-runner
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo publish --dry-run --package ${{ matrix.crate }} --allow-dirty
```

Validates `Cargo.toml` metadata, license headers, package size, README rendering on **every release-tag push**. Zero external exposure.

#### Pre-public staging (optional, when 1ES is available)

If you want a **real publish** to validate the full pipeline before crates.io:

1. Request an **Azure Artifacts Cargo feed** via 1ES (`https://1es.dev/azure-artifacts`). Org-internal visibility only.
2. Add `[registries.azure-internal]` entry to `~/.cargo/config.toml` on the runner.
3. Add a `cargo-publish-azure-artifacts` job mirroring the dry-run above but with `--registry azure-internal`.
4. Validate: from another internal machine, `cargo install --registry azure-internal kars-inference-router` and confirm it works.

This step is **optional** — `cargo publish --dry-run` catches 95% of issues without provisioning anything.

#### Public-day flip

The crates.io publish itself:

```yaml
  cargo-publish:
    if: startsWith(github.ref, 'refs/tags/v')
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: |
          # Topological order: libs first, then binaries
          cargo publish --package kars-eval-corpus --token ${{ secrets.CRATES_IO_TOKEN }}
          cargo publish --package kars-a2a-core --token ${{ secrets.CRATES_IO_TOKEN }}
          # Wait for index propagation
          sleep 30
          cargo publish --package kars-conformance-runner --token ${{ secrets.CRATES_IO_TOKEN }}
          cargo publish --package kars-a2a-gateway --token ${{ secrets.CRATES_IO_TOKEN }}
          cargo publish --package kars-inference-router --token ${{ secrets.CRATES_IO_TOKEN }}
          cargo publish --package kars-controller --token ${{ secrets.CRATES_IO_TOKEN }}
```

The `CRATES_IO_TOKEN` is generated from <https://crates.io/me> (needs an authenticated account; typically a maintainer-owned bot account).

---

### 1.3 Docker — already private; add internal-visibility GHCR mirror

#### Today

- `karsacr.azurecr.io/*` — private ACR, accessible only to AKS clusters via Workload Identity. ✅ already correct for staging.

#### Pre-public

Add a `ghcr-mirror` job to `.github/workflows/image-sign-sbom.yml` that pushes the same images to `ghcr.io/azure/kars/*` with **package visibility set to `internal`** (visible to Azure org members only). This validates that GHCR auth + push works *before* you flip the package to public on day 1.

```yaml
      - name: Push GHCR mirror (internal visibility)
        run: |
          docker tag ${{ env.REGISTRY }}/${{ matrix.image }}:${{ env.VERSION }} \
                     ghcr.io/azure/kars/${{ matrix.image }}:${{ env.VERSION }}
          echo ${{ secrets.GITHUB_TOKEN }} | docker login ghcr.io -u $ --password-stdin
          docker push ghcr.io/azure/kars/${{ matrix.image }}:${{ env.VERSION }}
```

By default a fresh GHCR package has visibility "private" — flip to "internal" via the org Packages settings once the first image lands.

#### Public-day flip

- File an MCR namespace request via Container Engineering (4–6 week lead time). On approval, push to `mcr.microsoft.com/kars/*` alongside the existing publish.
- Flip GHCR package visibility from `internal` → `public` for each of the 13 images (one-time GUI click per package).
- Keep `karsacr.azurecr.io/*` for AKS dev-cluster pulls (no change).

---

## Part 2 — Artemis onboarding

### What it is (recap)

**Component Governance** = Microsoft's internal SCA/SBOM/license-clearance pipeline. **Artemis** = the engineering system / portal that runs CG scans on a schedule and surfaces results.

### The onboarding sequence

1. **Confirm a 1ES sponsor.** For repos in `Azure/*`, the C+AI 1ES tooling team is the default sponsor. Reach out via your manager / OSPO contact for the right alias.

2. **Visit the Artemis portal** — `https://artemis.azurefd.net` (requires internal Microsoft login).

3. **Click "Onboard New Project"** and supply:
   - **Project name**: `kars` (or `Agent Reference Stack for Kubernetes`)
   - **GitHub repo URL**: `https://github.com/Azure/kars`
   - **Default branch**: `main`
   - **Orchestration platform**: choose **GitHub Actions** (NOT Azure Pipelines — our CI is in GitHub already)
   - **Languages**: Rust, TypeScript (kars has both)

4. **Authorize the Artemis GitHub App** on the `Azure/kars` repo. This grants Artemis read-only access to the repo + lets it consume the workflow run artefacts (SBOMs, dependency manifests).

5. **Add `cgmanifest.json` to repo root** (optional but recommended) — declares the "I'm aware of these dependencies and here's their provenance" subset that supplements auto-discovery. Stub for kars:

   ```json
   {
     "$schema": "https://json.schemastore.org/component-detection-manifest.json",
     "version": 1,
     "registrations": []
   }
   ```

   (Empty `registrations[]` is fine — we have no vendored sources after the Phase 5.2 AGT-only migration. The file's presence signals to Artemis that auto-discovery is authoritative.)

6. **Wait 1–2 weeks** for Artemis to wire the scheduled scan + start reporting. The first scan will show every dependency's CVE + license status.

7. **Resolve findings** as they appear in the Artemis dashboard. Most will be "OK" because cargo-deny / Dependabot / Trivy already gate on the same data. Anything Artemis flags that our in-repo tools missed is a real CG-specific finding (usually internal-only CVE info or license edge cases the public databases don't carry).

### What changes in the repo

- **`cgmanifest.json`** at root (~30 lines)
- **`.github/workflows/artemis-notify.yml`** (optional) — a workflow that posts release-tag events to Artemis so the compliance dashboard tracks per-release status

That's it. No other workflow changes are required because Artemis discovers dependencies from the repo + the existing SBOM artefacts produced by `image-sign-sbom.yml`.

---

## Part 3 — How Artemis fits into the release CI

### Common misconception

Artemis is **not** an in-pipeline step that blocks the build (the way `cargo audit` or `cosign verify` do). It's an **out-of-band scheduled scanner** that reads your repo + your build artefacts and produces a compliance dashboard. Your release pipeline runs whether or not Artemis is green — but **OSPO will refuse to flip repo visibility to public** until the Artemis dashboard for kars shows zero open Critical / High findings + zero open license exceptions.

### Release-flow diagram

```
Today's CI (kars):
   Push tag v0.1.0 →
     release.yml triggers →
       ci.yml (cargo build, cargo test, cargo audit, cargo deny, npm test, npm audit, helm-lint, container-scan)
       cargo-publish --dry-run (NEW from §1.2)
       cargo-publish-azure-artifacts (NEW from §1.2 — staging only)
       npm publish to github-packages (NEW from §1.1 — staging only)
       image-sign-sbom.yml (build images, cosign sign, syft SBOM, push to ACR)
       ghcr-mirror (NEW from §1.3 — staging only)
       create-release.yml (GitHub Release with changelog)

Artemis (out of band):
   Scheduled scan (nightly + on every push to main) →
     Read repo manifests (Cargo.toml, package.json, Dockerfiles)
     Read SBOM artefacts from latest image-sign-sbom.yml run
     Cross-reference with Microsoft CVE + license + IP databases
     Update Artemis dashboard
     Open work items for findings

Public-release day (manual):
   1. Verify Artemis dashboard is green (no open Critical/High)
   2. Re-tag v0.1.0 → triggers workflows ABOVE again, this time with:
        - npm publish workflow_dispatch with registry: npmjs-public
        - cargo-publish (real, not dry-run) to crates.io
        - GHCR package visibility flipped from internal → public
        - MCR push to mcr.microsoft.com/kars/* (after Container Engineering provisions the namespace)
   3. Flip repo visibility private → public
   4. Cut public GitHub Release
```

### What's required in CI vs. what's nice-to-have

| Required for public release | Status |
|---|---|
| `cargo audit` in CI | ✅ already in `.github/workflows/ci.yml:44` |
| `cargo deny` in CI | ✅ via `deny.toml` |
| Dependabot enabled | ✅ `.github/dependabot.yml` (cargo + docker + github-actions) |
| Trivy container scan | ✅ in `ci.yml` |
| Syft CycloneDX SBOM per image | ✅ in `image-sign-sbom.yml` |
| cosign keyless OIDC image signatures | ✅ in `image-sign-sbom.yml` |
| Notation signatures via Azure KV | ✅ in `image-sign-sbom.yml` |
| SLSA build provenance | ✅ via BuildKit `provenance: true` |
| **`cgmanifest.json`** at root | ❌ to add (§2.5 above) |
| **`cargo publish --dry-run` matrix** | ❌ to add (§1.2 above) |
| **npm publish staging via GHCR** | ❌ to add (§1.1 above) |
| **GHCR image mirror (internal visibility)** | ❌ to add (§1.3 above) |

### What's NOT in CI (Artemis is out of band)

- The Artemis scan itself runs on Microsoft-internal infrastructure; no GitHub Actions step.
- The Artemis dashboard is read manually pre-release — there's no green-light webhook back into the repo.
- Failures are tracked as work items in Artemis, not as PR check failures.

---

## Final recommended sequence

| # | Action | Effort | Pre-req for public release? |
|---|---|---|---|
| 1 | Add `.github/workflows/npm-publish.yml` with GHCR staging support | ~1 hr | Yes — proves the pipeline before day 1 |
| 2 | Add `cargo-publish --dry-run` matrix job to `release.yml` | ~30 min | Yes — catches metadata errors early |
| 3 | Add `ghcr-mirror` job to `image-sign-sbom.yml` with internal visibility | ~1 hr | Yes — validates GHCR auth |
| 4 | Run each new workflow once, manually via `workflow_dispatch` | ~30 min | Validates the pipelines |
| 5 | Add `cgmanifest.json` stub to repo root | ~15 min | Helps Artemis once enrolled |
| 6 | File Artemis onboarding via 1ES tooling team / Artemis portal | 30 min + 1–2 wk lead | **Yes — OSPO blocks public release without it** |
| 7 | File MCR namespace request via Container Engineering | 30 min + 4–6 wk lead | Optional — only if you want mcr.microsoft.com publishing |
| 8 | Claim `@kars` npm scope on npmjs.com (org-admin task) | ~10 min | Yes for npm public publish |
| 9 | Generate `CRATES_IO_TOKEN` from maintainer-owned crates.io account | ~10 min | Yes for crates.io publish |
| 10 | Wait for Artemis dashboard to show zero open Critical/High findings | passive | **Yes** |
| 11 | On the public-release day: flip repo visibility + run release workflows with public targets | ~1 hr active | The actual flip |

### What this costs

- **Zero $$ until public day.** GHCR is free for org-internal packages. Azure Artifacts is included in 1ES. crates.io and npmjs.com are free.
- **~4 hours total** of CI authoring (#1, #2, #3).
- **~3 hours** of process + waiting (#5–#10).
- **1 hour** for the actual public-day flip (#11).
