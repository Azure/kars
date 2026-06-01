# kars Publishing Strategy

> **Status (2026-06-01):** Planning — onboarding to ESRP + MCR in progress.
> Nothing in this repo currently publishes to public registries.
> The "ship it" path is documented here so reviewers can validate
> the design before the first GA release.

This document describes how kars artefacts will be published to public
registries, with an explicit "keep it private until day-0" path and the
ESRP-signed Microsoft official publish path.

The pattern is **lifted directly from Microsoft AGT**
(microsoft/agent-governance-toolkit `.github/pipelines/esrp-publish.yml`) —
same ESRP Release task version, same connected-service contract, same
dry-run UX. AGT is the canonical reference for ESRP-signed multi-language
publishing inside Microsoft.

---

## What kars publishes

| Artefact type | Count | Registry (after GA) | Onboarding required |
|---|---|---|---|
| Container images | 5 | mcr.microsoft.com (long-term), karsacr.azurecr.io (interim) | MCR onboarding |
| npm packages | 4 | npmjs.com under `@kars` scope | Claim npm scope |
| Rust libraries | 2 | crates.io | None (free + signed via ESRP) |
| Rust binaries | 4 | Inside container images + GitHub Release tarballs | None |
| Helm chart | 1 | OCI artefact in MCR + GitHub Release | MCR onboarding |

### Container images (5)

| Image | Source | Runs as |
|---|---|---|
| `kars-controller` | `controller/Dockerfile` | Kubernetes operator |
| `kars-inference-router` | `inference-router/Dockerfile` | Per-sandbox proxy |
| `kars-a2a-gateway` | `a2a-gateway/Dockerfile` | Public-ingress edge |
| `kars-conformance-runner` | `sandbox-images/conformance-runner/Dockerfile` | Eval Job runner |
| `kars-sandbox-base` | `sandbox-images/openclaw/Dockerfile.base` | Sandbox userland |

### npm packages (4)

| Package | Source | Consumer |
|---|---|---|
| `@kars/cli` | `cli/` | end users (`npx @kars/cli up`) |
| `@kars/mesh` | `mesh-plugin/` | runtime adapters |
| `@kars/runtime-openclaw` | `runtimes/openclaw/` | sandbox image |
| `@kars/runtime-langgraph-ts` | `runtimes/langgraph-ts/` | sandbox image |

### Rust libraries (2 → crates.io)

| Crate | Source | Consumer |
|---|---|---|
| `kars-a2a-core` | `kars-a2a-core/` | A2A 1.0.0 SDK consumers |
| `kars-eval-corpus` | `eval-corpus/` | external eval pipelines |

Rust **binaries** (controller, inference-router, a2a-gateway, conformance-runner)
ship inside container images and as GitHub Release tarballs. They do NOT publish
to crates.io.

---

## Three-stage publishing flow

### Stage 1 — Private CI builds (today)

Every PR runs `.github/workflows/ci.yml`. On dev/main pushes,
`.github/workflows/image-cache-publish.yml` builds + pushes to
**`ghcr.io/azure/kars-*` (private)**. No public publishing happens.

### Stage 2 — Internal Release (today, behind the wall)

Tag `v0.1.0-internal.1` (or `v0.1.0-preview.1`) — or manual dispatch
via the Actions UI. `.github/workflows/release-internal.yml` runs:

- Compiles all Rust binaries (host glibc 2.35, runs on AL3 2.38)
- Builds + pushes all 5 container images to **`ghcr.io/azure/kars-*` (PRIVATE)**
- Cosign keyless signs every image (Sigstore Rekor public attestation)
- `npm pack` for all 4 packages → tarballs attached to GitHub Release
- `cargo package` for the 2 libraries → `.crate` files attached
- Generates SPDX SBOMs (5) + Trivy HIGH/CRITICAL reports (5)
- Creates a **GitHub Release** with `prerelease: true` + `make_latest: false`
  — visible only to org members because the repo is private

**This is a real release.** Versioned, tagged, signed, complete artefact set,
nothing on a public registry. Anyone with repo access can pull
`ghcr.io/azure/kars-controller:v0.1.0-internal.1` and run it.

### Stage 3 — Public Release via ESRP (after onboarding)

Triggered manually from ADO once #384 (ESRP) + #386 (ADO project) close:
`.github/pipelines/esrp-publish.yml`.

- Downloads `.tgz` + `.crate` artefacts from the Stage 2 GitHub Release
- npm → npmjs.com under `@kars` scope, signed by Microsoft ESRP
- crates.io → Microsoft-signed publish via `EsrpRelease@11`
- Container images → MCR (after #385 onboarding)

The ADO pipeline has a `dryRun=true` parameter for ADO-side validation that
matches what Stage 2 already did in GHA.

---

## Why ESRP, and what does it sign?

ESRP (Engineering System Release Process) is the only approved publishing
path for Microsoft-signed OSS releases. It:

1. **Authenticode-signs** all binaries with the Microsoft CA
2. Embeds a tamper-proof signature inside each `.tgz`, `.crate`, container
   manifest, etc.
3. Records the release in Microsoft's audit ledger
4. Pushes to the public registry on Microsoft's behalf
5. Shows **"Microsoft Corporation"** as the publisher on npmjs.com, crates.io, etc.

Without ESRP: packages would publish from a personal account. **Microsoft
SDL forbids this for any project that uses the `@microsoft`, `microsoft-`,
or Azure branding.** We use the `@kars` npm scope (not `@microsoft`) to
leave non-ESRP options open, but the kars team intends to go through ESRP
anyway for supply-chain provenance.

> **GitHub Packages is not an approved general-purpose registry.** Per
> AGT's `docs/PUBLISHING.md`, GHCR is allowed only for interim/nightly
> builds and engineering assets — not official releases.

---

## Onboarding checklist (one-time)

Status: **all items still pending.**

- [ ] **MCR onboarding** — https://aka.ms/mcr-onboarding
  - Request namespace `kars/*`
  - Map images: `controller`, `inference-router`, `a2a-gateway`,
    `conformance-runner`, `sandbox-base`
- [ ] **ESRP onboarding** — https://aka.ms/esrp-onboarding
  - Get ESRP Release certificate in Azure Key Vault
  - Note the `ESRP_*` pipeline variables list below
- [ ] **ADO project** — request kars space + service connection `kars`
  (Azure RM, Managed Identity)
- [ ] **npm scope** — claim `@kars` scope, assign Microsoft team as owner
- [ ] **crates.io** — register team account (no per-crate registration needed
  beyond the first publish via ESRP)
- [ ] **PyPI Package Owners group** — N/A; kars has no Python packages
- [ ] **Anaconda** — N/A; kars has no conda packages

### Required ADO pipeline variables

Sourced from ADO pipeline variables (mark as secret):

- `ESRP_KEYVAULT_NAME` — Azure Key Vault containing the release cert
- `ESRP_RELEASE_CERT_IDENTIFIER` — cert name for `EsrpRelease@11`
- `ESRP_CLIENT_ID` — Managed Identity client ID
- `ESRP_OWNERS` — distribution list (e.g., `kars-team@microsoft.com`)
- `ESRP_APPROVERS` — distribution list (e.g., `kars-leads@microsoft.com`)

Hardcoded in pipeline:

- `MICROSOFT_TENANT_ID = 975f013f-7f24-47e8-a7d3-abc4752bf346`
  (PME tenant where ESRP certs live; do NOT add as ADO variable — causes
  cyclical reference warning, per AGT's experience.)

### ESRP onboarding form answers

When filling out the ESRP onboarding request (https://aka.ms/esrp-onboarding):

| Field | Value | Why |
|---|---|---|
| **Integration Technology** | **VSTS Build** | This is the ADO YAML task path (`EsrpRelease@11`). NOT "ESRP Client" — that's for standalone build services calling ESRP REST APIs directly without ADO. NOT "CloudBuild" — that's only for projects using Microsoft's internal CloudBuild. AGT uses VSTS Build and ships every release this way. |
| **Build system** | Azure DevOps Pipelines | Our `.github/pipelines/esrp-publish.yml` is an ADO YAML pipeline. |
| **Signing scenarios** | Sign + Release | Sign = Authenticode for binaries. Release = package distribution to npm / crates.io / MCR. |
| **Repository** | `Azure/kars` (GitHub) | GitHub-hosted source, ADO-hosted pipeline that pulls from it. |
| **ADO project** | (TBD per #386) | Whichever Microsoft ADO project ends up hosting kars CI/CD. |

The form note that says "Release customers should select ESRP Client unless
you will only be submitting via the ESRP Release UI" is misleading — the
`EsrpRelease@11` ADO task IS the supported VSTS-Build release path. AGT has
shipped 50+ releases this way without picking "ESRP Client".

---

## Compatibility commitments

- **kars container images run on AL3 distroless glibc 2.38.** CI build host
  is pinned to **ubuntu-22.04 (glibc 2.35)** for backward-compat. See
  `.github/workflows/ci.yml` → `build-rust` and the 4 Dockerfiles.
- **Rust toolchain pinned** to `1.88.0` workspace-wide. ESRP pipeline
  installs the exact pinned version with a SHA256-verified rustup-init.
- **Node.js** consumers expected on **20.x or 22.x**. ESRP pipeline uses 22.
