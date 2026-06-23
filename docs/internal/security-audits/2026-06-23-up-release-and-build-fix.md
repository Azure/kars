# Security Audit — `kars up --release` (import public GHCR images) + `--build` staging fix

PR: Azure/kars (branch `feat/up-release-and-build-fix`)

## Scope

Capability-path change in `cli/src/commands/up.ts` (the `kars up` image step):

1. **`--release [version]`** — new path that imports the **public, signed GHCR
   release images** (`ghcr.io/azure/*`) into the user's ACR via `az acr import`,
   instead of building from source or importing from a private source ACR. Bare
   `--release` uses `:latest`; `--release <tag>` pins. These are the same
   cosign-signed multi-arch images `kars dev --release` already runs. The change
   is confined to the image-acquisition step (precedence: `--release` →
   `--build` → source-ACR import); it bypasses no other phase (preflight, infra,
   Helm, AgentMesh, Entra) and honours `--skip-runtime-images` + resume/skip.

2. **`--build` staging fix** — `kars up --build` used the COPY-only
   `controller/Dockerfile` / `inference-router/Dockerfile`, which expect a
   pre-compiled `bin/<arch>/<binary>`, but never staged it. On a fresh non-Linux
   machine the build failed: `COPY bin/amd64/kars-controller: no such file or
   directory`. Now mirrors `kars push` / `kars dev`: on native linux/amd64 it
   `cargo build`s + stages the binaries; otherwise it uses the `*.multistage`
   Dockerfile that compiles Rust inside the (emulated) amd64 docker build.

## Threat model

### T1: Does importing from public GHCR introduce an untrusted image source? (NO)
The GHCR images are the **same artefacts** the release pipeline produces and
signs (cosign keyless + SBOM + SLSA provenance), already used by
`kars dev --release` and verified end-to-end on docker + AKS. `az acr import`
copies them into the user's **own** ACR under the same `:latest` names the Helm
chart and AgentMesh manifest already reference; AKS still pulls only from the
user's ACR. No new registry is trusted at runtime, and image **contents** are
unchanged. The mesh images are re-tagged `kars-agentmesh-*` → `agentmesh-*-agt`
to match the existing manifest (which `agentmesh_deploy.ts` rewrites to the
user's ACR). Required-image import failures hard-fail with a clear message
(no silent fallback to an unexpected image).

### T2: Does `--release` weaken any auth / isolation control? (NO)
`--release` only changes where images come from. Preflight, RBAC, Foundry /
Key Vault provisioning, NetworkPolicy, seccomp, Entra Agent ID setup, and the
Helm values are all unchanged. `az acr import` runs as the already-authenticated
caller against the user's ACR; pulling the *public* source needs no extra creds.

### T3: Does the `--build` multistage path change what's shipped? (NO)
`Dockerfile.multistage` compiles the **same Cargo workspace** as the COPY-only
path, just inside docker for cross-host correctness (it already backs
`kars push` / `kars dev` on Apple Silicon). The host-arch guard
(`linux && x64`) only selects build strategy; the resulting linux/amd64 image is
equivalent. This removes a fail-open footgun (a stale `bin/amd64/*` from a prior
unrelated build silently shipping into production) by compiling deterministically.

## What this audit does NOT cover

- Image **contents** (covered by the controller/router/sandbox build audits and
  the release-pipeline signing).
- The end-to-end AKS deploy with imported images — mechanically identical to the
  existing source-ACR import path; to be smoke-tested on a live subscription.

## Verdict

Accept. `--release` reuses the project's own signed public images through the
user's ACR with a hard-fail on missing required images and no change to any
other `up` phase; the `--build` fix makes source builds correct on all hosts.
Verified by the full 802-test suite, typecheck, and lint (0 errors), plus
anonymous-pull checks confirming the GHCR images are public.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
