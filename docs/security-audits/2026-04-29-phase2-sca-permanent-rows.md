# Phase 2 — S17.A: SCA permanent CI rows (npm audit)

**Date:** 2026-04-29
**Slice:** `phase2-sca-permanent-rows` (sub-slice S17.A of S17 conformance train)
**Author:** AzureClaw maintainers
**Sign-offs:** `@maintainer-1`, `@maintainer-2`

## Scope

§11.1 Phase 2 success-gate ("Security regression: trivy + cosign-verify
+ SCA → permanent CI rows") — partial closure for the **SCA**
half: add `npm audit --audit-level=high` to the existing CLI Build
and Mesh Plugin Build CI jobs. Both jobs already exist and run on
every PR; we promote them to also fail on `high` or `critical`
advisories in their dependency trees. This brings the JavaScript
side of the repo to parity with the Rust side, which has already
shipped `cargo audit --deny warnings` as a permanent CI row since
Phase 1.

- `.github/workflows/ci.yml` `cli-build` job: appended `npm audit
  --audit-level=high` after the existing `npm test` step.
- `.github/workflows/ci.yml` `mesh-plugin-build` job: same.

Both jobs share the same audit-level threshold (`high`) which
matches the cargo-audit job's `--deny warnings` posture (RUSTSEC's
default `warnings` corresponds roughly to npm's `high`).

## Out of scope

- **`cosign verify`** for base images, vendored binaries, or
  upstream releases. Cosign-verify is a separate supply-chain gate
  that requires either Sigstore-signed inputs (we don't pin to
  any) or a private root of trust (none provisioned). Tracked as
  a follow-up sub-slice **S17.B** once at least one direct
  dependency starts publishing Sigstore signatures.
- **Auto-PR for non-prod-affecting moderates.** Tools like
  Dependabot already do that; we don't reinvent it here.
- **Python sidecar SCA.** The AGT sidecar workflow exits early
  if `agt-sidecar/pyproject.toml` is absent (see ci.yml lines
  82-90). Once the sidecar lands, that job will need a `pip-audit`
  step. Out of scope for this slice — we don't ship the sidecar
  binary today.

## Hard-rule checklist (`docs/implementation-plan.md` §0.2)

| # | Rule | Status |
|---|------|--------|
| 1 | No fork; no upstream re-implementation | ✓ — uses standard `npm audit` |
| 8 | No custom-crypto / framing | ✓ — N/A |
| 9 | Audit doc with two sign-offs | ✓ — this doc |
| 10 | Verify, don't guess; cite sources | ✓ — `npm audit --audit-level=high` ran clean (`found 0 vulnerabilities`) on both `cli/` and `mesh-plugin/` before this PR; CI rows green from first push |

## Test coverage

- Local verification: `(cd cli && npm audit --audit-level=high)` and
  `(cd mesh-plugin && npm audit --audit-level=high)` both report
  `found 0 vulnerabilities` against today's `package-lock.json`.
- CI verification: this PR's own `CLI Build & Test` and `Mesh
  Plugin Build & Test` jobs exercise the new step end-to-end.

## Threat model

- **Failure mode delta.** Previously a high/critical advisory in a
  CLI / mesh-plugin transitive dep would silently land. Now CI
  rejects the PR until either the advisory is fixed (preferred)
  or the maintainer explicitly resolves it via `npm audit fix`,
  upstream patch, or `npm audit --omit=dev` if the advisory is
  scoped to a dev-only path that doesn't ship.
- **No new attack surface.** `npm audit` queries the public npm
  registry advisory database during CI; no new secrets or
  credentials added.
- **Threshold rationale.** `audit-level=high` (not `moderate`)
  matches the team's existing pattern of erring against
  high-noise gates that pin to advisories that don't affect the
  shipped surface area. We can tighten to `moderate` in a future
  slice when `cargo audit` parity tightens too.

## Existing implementation surveyed

- `.github/workflows/ci.yml:29-43` — existing `cargo-audit` job
  (Rust SCA reference).
- `.github/workflows/ci.yml:120-143` — existing `Security Scan`
  job running Trivy fs scan (already a permanent row).
- `.github/workflows/ci.yml:145-205` — existing `container-scan`
  job running Trivy on built images (also permanent, but skipped
  via admin-merge per Phase 2 standing approval until ACR base
  image cache stabilises).

## §14.6 / §15 impact

- §11.1 success-gate ("trivy + cosign-verify + SCA → permanent CI
  rows"): SCA half now closed for the JavaScript side. Trivy was
  already permanent; cosign-verify remains for S17.B.
- §15.2 #6 (CNCF Sandbox conformance): incremental progress; the
  K8s AI Conformance v1.35+ suite wiring is the larger remaining
  S17 deliverable.
