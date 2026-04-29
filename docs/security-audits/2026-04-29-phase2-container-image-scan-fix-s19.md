# Phase 2 — S19 Container Image Scan Fix

**Date:** 2026-04-29
**Branch:** `phase2-container-image-scan-fix`
**Slice:** S19 (sandbox base image build/pull resilience)

## Scope

Two changes:

1. `sandbox-images/openclaw/Dockerfile.base` — replace the brittle "≥1 staged
   node_modules dir under `/opt/openclaw-stage`" assertion with a positive
   sanity check on the four channel deps we ship, and stop masking real
   `openclaw doctor` errors with `|| true`.

2. New `.github/workflows/sandbox-base-publish.yml` + updated `container-scan`
   step in `.github/workflows/ci.yml` — publish the sandbox base image to
   GHCR on `dev`/`main` pushes, and have CI pull from GHCR (using
   `GITHUB_TOKEN`) before falling back to local rebuild.

## Root cause analysis

In OpenClaw 2026.4.26 the bundled-plugin runtime dependencies (telegram, discord,
slack, feishu, etc.) are resolved as direct dependencies of the `openclaw` npm
package itself, so `npm install -g openclaw` already populates
`/usr/local/lib/node_modules/openclaw/node_modules/`. When
`openclaw doctor --fix --non-interactive --yes` runs after that, its
`maybeRepairBundledPluginRuntimeDeps` flow finds `missing.length === 0` and
returns early without creating a `<stage>/openclaw-<version>-<hash>/` version
directory. The previous build-time assertion treated 0 staged dirs as a hard
failure, masking the actual cause behind a misleading error message and
`|| true`.

Confirmed from the upstream sources I extracted from `openclaw@2026.4.26` on
npm (`bundled-runtime-root-DEMD7-O_.js`,
`doctor-bundled-plugin-runtime-deps-CiQxW0ig.js`,
`effective-plugin-ids-KWubC1um.js`).

## Security considerations

### Dockerfile.base

- Dropped `|| true` on `openclaw doctor` — real failures will now fail the
  build instead of being silently swallowed. This is a strict improvement in
  the build-time integrity posture.
- Sanity check uses `[ -e ]` against four well-known channel package paths
  inside `/usr/local/lib/node_modules/openclaw/node_modules/`. No new
  network or registry surface; purely a filesystem assertion against the
  already-installed openclaw tree.
- `chmod -R a+rX /opt/openclaw-stage` retained so the runtime user (UID 1000)
  can read whatever doctor *did* stage.

### GHCR publish workflow

- Image is built from the same `Dockerfile.base` we ship. Contains only
  publicly-available OSS (Mariner base, openclaw npm, Node.js, ripgrep, gh,
  himalaya, op CLI, Python wheels from `vendor/sandbox-wheels/`). No secrets,
  no AzureClaw runtime/router/controller code (those are added in later
  multi-stage builds layered on top of this base).
- Authentication uses the workflow-provided `GITHUB_TOKEN` (`packages: write`).
  No long-lived credentials added to the repository.
- Package visibility should be **private** in GHCR settings to preserve the
  current exposure surface. CI in this repo can still pull a private GHCR
  image using `GITHUB_TOKEN` (`packages: read`), so making it private does
  not break the CI pull path.

### container-scan workflow update

- Adds `packages: read` permission to the existing job (least-privilege add).
- Logs into GHCR with `GITHUB_TOKEN` only when the base image is unchanged
  (i.e. only on the pull path, not the rebuild path).
- Falls back to ACR pull, then to local rebuild — preserves the ability to
  scan local rebuilds if neither registry has the image (e.g. forks).

## Verification

- `git diff sandbox-images/openclaw/Dockerfile.base` reviewed manually.
- CI must pass `Container Image Scan` for this PR — that **is** the
  verification, since this is the first PR where Container Image Scan needs
  to actually go green.

## CI scope

No changes to the gating contract. Container Image Scan was already a job in
`ci.yml` — this PR only fixes its build behavior and adds a faster path via
GHCR caching.

## Risks

- Sanity check could become a false negative if upstream openclaw drops one
  of the four channel deps (`grammy`, `@discordjs/opus`, `@slack/bolt`,
  `@larksuiteoapi/node-sdk`). That would be a real channel regression we'd
  want to catch at build time, not silently ship.
- GHCR publish workflow needs `packages: write` on `dev`/`main` pushes. This
  is a new permission grant on those branches' workflow runs, but is scoped
  to the `azureclaw-sandbox-base` package only.

## Files touched

- `sandbox-images/openclaw/Dockerfile.base`
- `.github/workflows/ci.yml` (container-scan job)
- `.github/workflows/sandbox-base-publish.yml` (new)
- `CHANGELOG.md`
- `docs/security-audits/2026-04-29-phase2-container-image-scan-fix-s19.md` (this file)
