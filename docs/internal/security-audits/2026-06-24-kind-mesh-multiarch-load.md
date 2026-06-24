# Security Audit — fix multi-arch AgentMesh image load into kind (`kars dev --release --target local-k8s`)

PR: Azure/kars (branch `fix/kind-mesh-multiarch-load`)

## Scope

Capability-path change in `cli/src/commands/dev/local-k8s.ts` (the kind/local-k8s
dev flow only).

`kars dev --release --target local-k8s` failed at step 7/14 ("Deploying
agentmesh-agt") with:

```
ERROR: failed to load image: ctr ... images import --all-platforms ... content
digest <...> not found
local-k8s dev failed: kind load docker-image 'agentmesh-relay-agt:dev' --name kars-dev
```

Root cause: the AgentMesh relay/registry images are **multi-arch**, and
`deployAgentMesh()` loaded them with a **bare `kind load docker-image`**, which
runs `ctr import --all-platforms` against the containerd image store — that fails
because only the host-platform blobs are present locally. The controller / router
/ sandbox images already loaded fine because they go through `loadImageIntoKind()`,
which falls back to `<runtime> save | ctr import -` (host platform only). The mesh
load was the last code path still using the bare loader.

Fix:
1. Route the relay/registry load through the existing `loadImageIntoKind()` helper
   (same robust path the other images use).
2. Quiet the primary `kind load docker-image` attempt inside `loadImageIntoKind`
   (`stdio: "pipe"`), since it prints a scary-but-recovered "content digest not
   found" ERROR for multi-arch images before the fallback succeeds.

Only `--target local-k8s` (kind) is affected: `kars dev --release` (docker) runs
the pulled image directly and `kars up` (AKS) pulls multi-arch images from ACR —
neither uses `kind load`.

## Threat model

### T1: Does the alternate load path change WHAT runs? (NO)
`loadImageIntoKind()` loads the **same** local image tag into the kind node's
containerd, just via `docker save | ctr import -` instead of `kind load
docker-image` when the latter can't satisfy `--all-platforms`. The image bytes,
digests, and the host-arch variant selected are identical; only the transport
into the node changes. The image was already pulled from the signed public GHCR
release in the prior step.

### T2: Does quieting the primary attempt hide a real failure? (NO)
`loadImageIntoKind()` always **verifies** the image is present on the node after
the primary attempt and runs the `ctr import` fallback if not; success is decided
by that verification, not by the (now-piped) output of the best-effort first try.
A genuine load failure still surfaces (the fallback throws).

### T3: Blast radius (LOCAL DEV ONLY)
The change is confined to the kind-based local dev loop. No production / AKS path,
no controller, router, mesh-crypto, NetworkPolicy, or seccomp behaviour changes.

## Verdict

Accept. The fix makes the kind mesh-image load use the same verified, fallback-
backed loader as every other image, with no change to image contents or any
runtime security control. Confined to `kars dev --target local-k8s`. Verified by
build + dev tests + lint; the failing real-world `kars dev --release --target
local-k8s` step is the reproduction.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
