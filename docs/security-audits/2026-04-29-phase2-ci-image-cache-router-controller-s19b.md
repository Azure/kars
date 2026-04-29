# Phase 2 — S19.b CI Image Cache (Router + Controller)

**Date:** 2026-04-29
**Branch:** `phase2-ci-image-cache-router-controller`
**Slice:** S19.b (follow-up to S19)

## Scope

1. Generalise `.github/workflows/sandbox-base-publish.yml` →
   `.github/workflows/image-cache-publish.yml`. Three-image matrix:
   `sandbox-base`, `inference-router`, `controller`. Each branch is gated on
   its own path filter so we don't rebuild all three on every commit.
2. Update `container-scan` in `.github/workflows/ci.yml` to also pull the
   inference router image from GHCR (with local-build fallback), matching
   the pattern already in place for the sandbox base image.
3. Fix the channel-dep sanity check in `sandbox-images/openclaw/Dockerfile.base`
   that S19 introduced. The check was incorrect — channel deps in OpenClaw
   2026.4.26 don't live where the check was looking. Replace with "trust
   openclaw doctor's exit code".

## Rationale

S19 (PR #108) cut PR-time CI cost for the sandbox base image. Inference
router and controller have similar issues:

- Inference router is the second-biggest single-step cost in `container-scan`
  (Rust + cargo build of an Axum app + cross-compile target).
- Controller is similar.

Both rebuild from scratch on every PR even when they haven't changed. Same
GHCR push/pull pattern fixes both.

The Dockerfile.base sanity check fix is needed because the S19 check was
returning false negatives — the `/usr/local/lib/node_modules/openclaw/node_modules/`
location I asserted on doesn't actually contain channel deps in OpenClaw
2026.4.26 (channel deps live under `dist/extensions/<channel>/node_modules/`
and are surfaced via the `link_pkg` symlink block at line 79-95). Doctor's
own exit code is the right source of truth.

## Security considerations

### Image cache publish workflow (matrix extension)

- Same security model as the original S19 workflow:
  - Authentication via `GITHUB_TOKEN` (`packages: write`).
  - Inference router image contains only the compiled Rust binary +
    distroless base. No secrets, no Azure-specific configuration.
  - Controller image is the same shape as inference router.
  - Each package should be marked **private** in GHCR settings to preserve
    current exposure surface.

### Container-scan job

- Adds `packages: read` permission (already present from S19).
- Logs into GHCR before the per-image conditional build/pull steps.
- Falls back to local build if no cached image exists.

### Dockerfile.base check change

- Removes the channel-dep filesystem assertion. This is a strict reduction
  in build-time validation but the previous assertion was producing false
  negatives, so it offered no real protection.
- Adds `set -o pipefail` so the `openclaw doctor … | tail -40` pipe
  surfaces a non-zero exit code from doctor instead of hiding it behind
  tail's success.
- Drops `|| true` mask. Real doctor failures will now fail the build.

## Verification

- This **is** the verification PR: container-scan must go green.
- The first push will trigger image-cache-publish.yml on the matrix to
  prepare the GHCR cache for subsequent PRs.

## Files touched

- `.github/workflows/image-cache-publish.yml` (renamed from `sandbox-base-publish.yml`, generalised)
- `.github/workflows/ci.yml` (container-scan job: add router pull, add login, restructure)
- `sandbox-images/openclaw/Dockerfile.base` (drop false-negative sanity check)
- `CHANGELOG.md`
- `docs/security-audits/2026-04-29-phase2-ci-image-cache-router-controller-s19b.md` (this file)
