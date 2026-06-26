# kars — Supply-Chain Hardening

This document describes the kars build, sign, and verify pipeline
for container images and Rust dependencies. It is the operator's
reference for what `cargo deny`, `cosign verify`, and the CI rows
actually check, and how to extend them.

> **See also:** [Supply-chain posture & OpenSSF Scorecard notes](../security/supply-chain-posture.md)
> records how kars addresses (or deliberately accepts) each Scorecard finding —
> the *policy* counterpart to this *pipeline* reference.

## At a glance

| Layer                | Tool                  | Where it runs                          | Required |
|----------------------|-----------------------|----------------------------------------|----------|
| Rust advisories      | `cargo audit`         | `.github/workflows/ci.yml` (advisory)  | No (continue-on-error) |
| Rust supply-chain    | `cargo deny`          | `.github/workflows/ci.yml`             | **Yes**  |
| Node advisories      | `npm audit`           | `cli-build`, `runtime-openclaw-build`, `mesh-plugin-build` jobs | **Yes** (>= high) |
| Filesystem CVE       | `trivy fs`            | `security-scan` job                    | **Yes**  |
| Image CVE            | `trivy image`         | `container-scan` job                   | **Yes**  |
| Image signature      | `cosign verify`       | `cosign-verify` job (PR dry-run)       | **Yes** (recipe pinned) |
| SLSA build provenance | BuildKit (`provenance: true` on `docker/build-push-action`) | `image-sign-sbom.yml` | **Yes** (tag push) |
| SBOM (SPDX)          | `syft` (anchore/sbom-action) | `image-sign-sbom.yml`           | **Yes** (tag push) |
| Image signing        | `cosign` keyless OIDC | `image-sign-sbom.yml`                  | **Yes** (tag push) |
| Dockerfile lint      | `hadolint`            | `dockerfile-lint` job                  | **Yes**  |

> **About SLSA.** On tagged release builds, `docker/build-push-action` runs with `provenance: true`, which makes BuildKit emit a SLSA-format build-provenance attestation alongside the image (attached to the registry as an OCI attestation manifest). Combined with the cosign keyless signature and the SPDX SBOM, every signed image has a verifiable record of *what* was built, *how* it was built, and *with what dependencies*. Verification recipes are below.

## `deny.toml`

`deny.toml` lives at the workspace root. It pins four sections:

- `[advisories]` — RUSTSEC database is checked at every PR. The
  `ignore` list contains advisories with documented rationale; do
  **not** add IDs here without a comment explaining why the call
  site is non-attacker-observable. Current ignores:
  - `RUSTSEC-2024-0370` (proc-macro-error unmaintained) — transitive
    via `sigstore` → `json-syntax` → `locspan-derive`. Build-time
    only, no runtime exposure.
  - `RUSTSEC-2023-0071` (rsa Marvin timing attack) — pulled by
    `jsonwebtoken` and `sigstore`. Neither call site does
    attacker-observable RSA decryption with secret keys.
- `[licenses]` — allow-list of SPDX identifiers. Adding a new
  third-party dependency that introduces a license outside the
  allow-list will fail CI.
- `[bans]` — banned crates and version-skew detection.
- `[sources]` — forbids non-crates.io git sources by default.

## Image signing

kars images are signed in two flows:

- **Tag releases (`v*`)** — signed with Notation against an Azure
  Key Vault key (`.github/workflows/image-sign-sbom.yml`). SBOMs are
  generated with `syft` and uploaded as build artifacts.
- **`dev` / `main` push** — built and pushed to GHCR by
  `image-cache-publish.yml`. SBOM generation is recommended as a
  follow-up; signing on every push is intentionally deferred (signs
  add cost on hot rebuild paths).

### Verifying signatures (keyless OIDC)

The `cosign-verify` CI job pins the verification recipe. To verify
locally against a tag-release image:

```bash
cosign verify \
  --certificate-identity-regexp "^https://github.com/Azure/kars/" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  ghcr.io/azure/kars-controller@sha256:<digest>
```

The `cosign-verify` job runs in PR mode as a **dry-run** because
PR-rebuilt images are not always signed. The verification command is
echoed into the job summary so reviewers can replay it against a
specific digest.

## Image-tag convention

The kars repo convention is `:latest` as the *default*
operator-image tag (controller, inference-router, sandbox base). This
is intentional: the controller's image-tag drift across v11–v25
caused hard-to-debug incidents, and `:latest` plus
`imagePullPolicy: Always` keeps the cluster on the most recently
published digest.

In production, operators are expected to override the tag at install
time with a digest pin:

```bash
helm install kars deploy/helm/kars \
  --set controller.image.tag="@sha256:<digest>"
```

The CNCF AI Conformance suite under `tests/cncf-conformance/` enforces
the minimum bar: every `image:` reference must declare an explicit
tag or digest. Implicit `:latest` (no tag at all) is forbidden. The
exact criterion identifier tracks the upstream conformance release the
suite is pinned to — see the suite's README for the current mapping.

## SBOM

Tag-release SBOMs are built with `syft` and attached as build
artifacts. To attach SBOMs to GHCR images on every `dev`/`main`
push, wire `anchore/sbom-action` into `image-cache-publish.yml` and
follow up with `oras attach --artifact-type application/spdx+json`
to publish the SPDX JSON as an OCI artifact alongside the image.

## CNCF conformance

`tests/cncf-conformance` runs as a workspace test crate on every PR.
It writes `CONFORMANCE-REPORT.md` and fails the build on any
non-passing criterion. See `docs/operations/branch-protection.md` for
which jobs are required checks.
