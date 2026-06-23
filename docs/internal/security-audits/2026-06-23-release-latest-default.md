# Security Audit — `kars dev --release` defaults to latest + public quick-start

PR: Azure/kars#411 (branch `feat/release-latest-default`)

## Scope

A follow-up to #408 (`docs/internal/security-audits/2026-06-23-cli-dev-release-published-images.md`).
The capability-introducing change is in `cli/src/commands/dev.ts`:

- `--release` becomes an **optional-value** flag. Bare `--release` now resolves
  to the latest published images (the `:latest` tag); `--release <tag>` still
  pins a specific release. `releaseImage(name, "latest")` → `ghcr.io/azure/<name>:latest`.
- The relay/registry pull drops the hardcoded `--platform linux/amd64` so the
  host-arch variant is pulled for multi-arch images (relay/registry became
  multi-arch in interim.10), falling back to amd64 (emulated) for older
  amd64-only pinned tags.

Non-capability changes in the same PR (not the subject of this audit, but
noted): the release workflow marks public-interim releases as the repo's
`latest` release; README/getting-started lead with the public, no-auth
`/releases/latest/download/` install + `kars dev --release`; 12 GitHub-owned
actions pinned by SHA (Scorecard Pinned-Dependencies).

## Threat model

### T1: `:latest` is mutable — does defaulting to latest enable image substitution? (MITIGATED)
`:latest` is pushed by the **same protected org CI** that pushes `:VERSION`,
to the same `ghcr.io/azure` packages, and every image (including the manifest
`:latest` points at) is **cosign keyless signed + GitHub build-provenance
attested + SBOM'd** — identical posture to the pinned-tag path audited in #408.
A mutable tag is a *reproducibility* tradeoff, not a trust downgrade: consumers
who need pinning use `--release <tag>` (documented). The default favours
"always get the latest security fixes" for the grandma/no-pin audience, which
is the safer default for unattended consumers.

### T2: Dropping `--platform linux/amd64` on the relay/registry pull (NOT A REGRESSION)
Previously the CLI force-pulled the amd64 relay/registry and ran them emulated
on arm64. Now `docker pull` (no `--platform`) selects the host-arch variant for
multi-arch images and still resolves amd64 for amd64-only tags. The image
**contents/signatures are unchanged**; only the arch of the pulled variant
changes. Native arm64 removes the Rosetta/QEMU emulation layer (a strict
reduction in moving parts), and the relay forwards only opaque encrypted
frames either way — no security control depends on the arch.

### T3: Public "for everyone" framing — does it expose anything new? (NO)
The runtime images referenced by the docker quick-start
(`openclaw-sandbox`, `kars-agentmesh-relay`, `kars-agentmesh-registry`) are
**already public** on `ghcr.io/azure` and already signed/attested. The docs
change is descriptive (point users at the existing public artefacts + the
stable `/releases/latest/` URL); it grants no new access and changes no
package visibility. `kars-controller`/`kars-inference-router` remain private —
the K8s (`--target local-k8s`) released path needs them public, called out in
the PR as a separate one-time maintainer action.

## What this audit does NOT cover

- Image **contents** (covered by the controller/router/sandbox build audits and
  the patched AGT SDK in microsoft/agent-governance-toolkit#3128).
- The ESRP-signed public path (MCR + npmjs + crates.io) that supersedes the
  interim GHCR release line — tracked in `docs/PUBLISHING.md`.

## Verdict

No new capability or trust boundary is introduced. The change makes the
already-signed published-image path easier to consume (latest-by-default,
native-arch) without weakening signing, provenance, sandbox isolation, or the
mesh trust model.

Signed-off-by: Pal Lakatos <plakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
