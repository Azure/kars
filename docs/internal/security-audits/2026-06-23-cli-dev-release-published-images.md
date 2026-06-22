# Security Audit — `kars dev --release` (run from published images)

PR: Azure/kars#408 (branch `release/published-binaries`)

## Scope

Adds a `--release <tag>` flag to `kars dev` (`cli/src/commands/dev.ts`) — a
capability-introducing path under `cli/src/commands/`. In release mode the
CLI:

- pulls `ghcr.io/azure/openclaw-sandbox:<tag>` instead of building the
  sandbox image locally,
- pulls `ghcr.io/azure/kars-agentmesh-{relay,registry}:<tag>` and tags them
  `agentmesh-{relay,registry}:dev` for the existing run path,
- **skips** the AGT auto-clone and every local image build,
- forces `--build` off and rejects `--build`/`--target local-k8s` combos.

It also adds the interim public release workflow
(`.github/workflows/release-public-interim.yml`) that produces those images.
This audit covers the **CLI capability change**; the image build/publish
supply-chain posture is covered inline below.

## Threat model

### T1: `--release` pulls a tampered/malicious image instead of the real one (MITIGATED)
The pulled images are the **same Dockerfiles and sources** as the local
build, pre-built in the org's protected CI. Every image is **cosign keyless
signed** (Sigstore Rekor) **and** carries a **GitHub build-provenance
attestation** (`gh attestation verify`) **plus an SPDX SBOM**. A consumer can
verify provenance before trusting an image. The release tag and the CLI
version are required to match (documented), reducing confusion-style
substitution.

### T2: `--release` weakens sandbox isolation vs the from-source build (NOT A REGRESSION)
The runtime security posture is identical: `openclaw-sandbox` is the *same*
image the from-source path builds and the controller launches — egress-guard
init (iptables, UID-1000 confinement), the UID 1000/1001 split, seccomp, and
the router L7 allow-list are all baked into the image, not added by the CLI.
`--release` changes only the image **source** (pull vs local build), never
the container's security context or the dev-mode trust boundary.

### T3: Supply-chain — compromised CI publishes a bad image under a release tag (MITIGATED, residual accepted)
The release workflow runs in the org's GitHub Actions with keyless OIDC
signing; signatures + provenance are recorded publicly in Rekor. A consumer
verifies with `cosign verify` / `gh attestation verify`. Residual risk
(a fully compromised org CI) is the same as for any pre-built artefact and is
the reason the **ESRP-signed** path (MCR + npmjs + crates.io) supersedes this
interim GHCR release later — tracked in `docs/PUBLISHING.md`.

### T4: `--release` skips the AGT clone — does it skip a security control? (NO)
The AGT clone only provided *source to build relay/registry locally*. In
release mode those are pulled as signed images instead. No governance,
policy, identity, or crypto control is bypassed; the relay forwards opaque
encrypted frames and the registry does prekey/discovery exactly as before.

## What this audit does NOT cover

- The **contents** of the published images (the controller/router/sandbox
  build posture is covered by their respective prior audits; the patched AGT
  SDK is covered by the upstream PR microsoft/agent-governance-toolkit#3128
  + its audit doc).
- The **AKS** `kars up --release` path — not implemented in this change
  (`--release` is docker-target only and errors clearly on `local-k8s`).
- ESRP-signed public distribution (future; supersedes the interim release).

## Test posture

- `cli` typecheck clean (`tsc --noEmit`); `oxlint` 0 errors; **798 CLI tests
  pass** (`vitest`), including `dev.test.ts`.
- `--release` flow validated as far as the authoring environment allows:
  loads cached creds, skips the rebuild prompt + AGT clone + local build, and
  resolves the correct `ghcr.io/azure/openclaw-sandbox:<tag>` ref before the
  pull. Full pull→run→agent-response e2e is pending verification in a
  working-`docker pull` environment (the authoring host had a local docker
  egress block); the exec-brief harness is wired via `KARS_RELEASE`.
- Release pipeline proven green end-to-end: `v0.1.0-interim.6` published with
  all images cosign-signed + attested + SBOM'd; the published CLI tarball
  carries `--release` (verified).

## Sign-offs

Signed-off-by: Pal Lakatos <plakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
