# kars Publishing Strategy

> **Status (2026-06-01):** Legal review approved · ESRP onboarding done ·
> ADO pipeline integration in progress · MCR / npm scope / crates.io
> publishing wires up next. Until then the official supported install
> path is **build-from-source after cloning the repo** (see
> [main README](../README.md#try-it-in-five-minutes)). Internal
> wall-off releases (private GHCR + private GitHub Releases) are
> available to Microsoft / Azure-org members via
> [`install.sh`](../install.sh).

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

Triggered manually from ADO once the ESRP cert + ADO pipeline wiring are in place:
`.github/pipelines/esrp-publish.yml`.

- Downloads `.tgz` + `.crate` artefacts from the Stage 2 GitHub Release
- npm → npmjs.com under `@kars` scope, signed by Microsoft ESRP
- crates.io → Microsoft-signed publish via `EsrpRelease@11`
- Container images → MCR (after MCR namespace onboarding completes)

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

Status: **ESRP onboarded ✓ · everything else pending.**

- [x] **ESRP onboarding** — https://aka.ms/esrp-onboarding (done)
- [ ] **Microsoft DL for project contact** — file via idweb to create
  `kars@microsoft.com` (or whichever alias the team picks). Needed
  as:
  - The `author` field in published npm/PyPI packages
  - The `ESRP_OWNERS` / `ESRP_APPROVERS` distribution lists below
  - The public contact point in `SUPPORT.md`

  Until the DL exists, package metadata uses `Microsoft Corporation`
  with no email (PyPI / npm both accept this).
- [ ] **MCR onboarding** — https://aka.ms/mcr-onboarding
  - Request namespace `kars/*`
  - Map images: `controller`, `inference-router`, `a2a-gateway`,
    `conformance-runner`, `sandbox-base`
- [ ] **ADO project** — request kars space + service connection `kars`
  (Azure RM, Managed Identity)
- [ ] **npm scope** — claim `@kars` scope, assign Microsoft team as owner
- [ ] **crates.io** — register team account (no per-crate registration needed
  beyond the first publish via ESRP)
- [ ] **PyPI Package Owners group** — N/A; kars has no Python packages
  bound for PyPI today (the runtime adapters under `runtimes/*/` are
  Python but distribute through container images)
- [ ] **Anaconda** — N/A; kars has no conda packages

### Required ADO pipeline variables

Sourced from ADO pipeline variables (mark as secret):

- `ESRP_KEYVAULT_NAME` — Azure Key Vault containing the release cert
- `ESRP_RELEASE_CERT_IDENTIFIER` — cert name for `EsrpRelease@11`
- `ESRP_CLIENT_ID` — Managed Identity client ID
- `ESRP_OWNERS` — distribution list owning the published packages
  (e.g., `kars@microsoft.com` once the DL exists — see Onboarding above)
- `ESRP_APPROVERS` — distribution list authorised to approve releases
  (can be the same DL or a separate `kars-leads@microsoft.com`)

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
| **ADO project** | (whichever Microsoft ADO project ends up hosting kars CI/CD) | Set up by the team that owns kars publishing. |

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

---

## Consuming AGT from upstream `main` (temporary)

kars depends on **Microsoft AGT** (the Agent Governance Toolkit) for both the
Rust mesh primitives and the TypeScript SDK. We need fixes that landed in
upstream `main` (PR [#2090](https://github.com/microsoft/agent-governance-toolkit/pull/2090),
[#2719](https://github.com/microsoft/agent-governance-toolkit/pull/2719), and
CI-stabilisation follow-ups) but are NOT yet in any AGT release.

While we wait for an AGT release that contains the fix, kars consumes AGT
from a pinned commit (`bae5de3` on `microsoft/agent-governance-toolkit`).
This is wired in two places:

### Rust — `[patch.crates-io]` in `Cargo.toml`

```toml
agentmesh = "4.0.0"          # floor — what we declare we want
agentmesh-mcp = "4.0.0"

[patch.crates-io]
agentmesh = { git = "https://github.com/microsoft/agent-governance-toolkit", rev = "bae5de3...", package = "agentmesh" }
agentmesh-mcp = { git = "https://github.com/microsoft/agent-governance-toolkit", rev = "bae5de3...", package = "agentmesh-mcp" }
```

When upstream cuts a release containing the pinned commit, drop the
`[patch.crates-io]` block and Cargo automatically resolves from crates.io.

### npm — vendored tarball in `vendor/agt/`

The AGT TypeScript SDK has no `prepare` script, so installing from a git
ref directly doesn't trigger the TypeScript build. We instead:

1. Clone AGT at the pinned SHA
2. Run `npm install && npm run build && npm pack`
3. Commit the produced `.tgz` to `vendor/agt/`
4. Reference it via `"@microsoft/agent-governance-sdk": "file:../vendor/agt/microsoft-agent-governance-sdk-4.0.0-agt-<sha>.tgz"`

When upstream cuts an npm release containing the fix, flip back to the
published version and delete `vendor/agt/`.

### CI guard — `ci/check-agt-released.sh`

Runs daily via `.github/workflows/check-agt-released.yml`. Walks the AGT
release tags via the GitHub API, checks whether any of them contains the
pinned commit, and opens a tracking issue (labelled `release`,
`onboarding`) the moment a fixed release appears. Closes idempotently if
already open. This bounds the time we sit on the git pin.

### Updating the pin to a newer AGT commit

If you need a newer AGT main than what's pinned today:

```bash
# 1. Pick the new commit SHA from microsoft/agent-governance-toolkit main
NEW_SHA=$(gh api repos/microsoft/agent-governance-toolkit/commits/main --jq .sha)
SHORT=$(echo "$NEW_SHA" | cut -c1-7)

# 2. Update Cargo.toml [patch.crates-io] rev fields
sed -i.bak "s|bae5de3[0-9a-f]*|$NEW_SHA|g" Cargo.toml && rm Cargo.toml.bak
cargo update -p agentmesh -p agentmesh-mcp

# 3. Rebuild + repack the TS SDK
git clone --depth 1 https://github.com/microsoft/agent-governance-toolkit /tmp/agt
( cd /tmp/agt && git fetch --depth 1 origin "$NEW_SHA" && git checkout "$NEW_SHA" )
( cd /tmp/agt/agent-governance-typescript && npm install && npm run build && npm pack )
rm vendor/agt/microsoft-agent-governance-sdk-*.tgz
cp /tmp/agt/agent-governance-typescript/microsoft-agent-governance-sdk-*.tgz \
   "vendor/agt/microsoft-agent-governance-sdk-4.0.0-agt-${SHORT}.tgz"

# 4. Update package.json file: paths
for f in mesh-plugin/package.json runtimes/openclaw/package.json; do
  sed -i.bak "s|microsoft-agent-governance-sdk-4.0.0-agt-[0-9a-f]*.tgz|microsoft-agent-governance-sdk-4.0.0-agt-${SHORT}.tgz|g" "$f"
  rm "${f}.bak"
done

# 5. Reinstall + checksum
( cd mesh-plugin && npm install )
( cd runtimes/openclaw && npm install )
( cd vendor/agt && shasum -a 256 *.tgz > SHA256SUMS )
```
