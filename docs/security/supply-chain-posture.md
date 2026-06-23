# Supply-chain posture & OpenSSF Scorecard notes

This document records kars's supply-chain decisions and how we address — or
deliberately accept — each [OpenSSF Scorecard](https://scorecard.dev/viewer/?uri=github.com/Azure/kars)
finding. Tracking issue: **#410**.

## Addressed

| Check | What we did |
|---|---|
| **Token-Permissions** | Every workflow sets top-level `permissions: contents: read`; write scopes (`packages`, `id-token`, `attestations`, `contents`, `security-events`) are granted per-job only where used. |
| **Pinned-Dependencies — GitHub Actions** | All GitHub-owned + third-party actions pinned by commit SHA. |
| **Vulnerabilities — actionable** | `RUSTSEC-2026-0185` (quinn-proto) fixed by upgrade. |
| **Signing (images)** | Every container image is cosign keyless-signed + carries an SPDX SBOM + a GitHub build-provenance (SLSA) attestation. |

## Accepted with rationale

### Vulnerabilities (no upstream fix)
Triaged in [`osv-scanner.toml`](../../osv-scanner.toml), [`deny.toml`](../../deny.toml),
and [`.cargo/audit.toml`](../../.cargo/audit.toml). All remaining advisories are
**unmaintained or no-patch transitive** dependencies whose exploit path does not
apply to kars (build-time-only proc-macros; `rsa` not used for kars crypto;
js-yaml parsing only trusted in-image config). Each is documented inline with a
reason and re-checked when a fix ships.

### Binary-Artifacts — `vendor/sandbox-wheels/*.whl`
**Intentional.** ~130 Python wheels are vendored (and LFS-tracked) so the
`kars-sandbox-base` image builds **hermetically and offline** with no live PyPI
dependency at build time — a deliberate supply-chain *hardening* (vendored +
checksummed beats pulling from PyPI on every build). Wheels are refreshed via a
reviewed PR. This is a knowing trade-off against Scorecard's "no binaries in
source" heuristic.

### Pinned-Dependencies — container base images
Base images (`mcr.microsoft.com/azurelinux/...`) are referenced by tag, not
digest, **on purpose**: kars rebuilds images frequently so each build pulls the
latest **OS security patches** for the base. Digest-pinning would freeze the
base and silently miss CVE fixes until a manual bump. The intended end-state is
digest-pin **plus** Renovate digest auto-update (so we get both patches and
reproducibility) — tracked in #410.

### Pinned-Dependencies — pip / npm / go install commands
Reproducibility for these is provided by lockfiles (`Cargo.lock`,
`package-lock.json`) and the vendored wheels, which Scorecard's heuristic does
not credit. Hash-pinning is tracked in #410.

## Open / external

- **Signed-Releases (release *assets*)** — images are signed; signing the
  GitHub Release tarball assets is tracked in #410.
- **CII-Best-Practices badge** — application is a manual process at
  <https://www.bestpractices.dev/>; tracked in #410.
- **Some packages private** — `kars-controller` / `kars-inference-router` GHCR
  packages need a one-time org-admin "Public" toggle for the kind/AKS
  `--release` path (the docker path is already fully public). GitHub has no API
  for container-package visibility; this is UI-only.
