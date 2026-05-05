# Image versioning & release tagging

AzureClaw produces eight container images: the controller, the
inference router, the sandbox base + slim overlay, the AgentMesh relay
+ registry, and the five runtime adapter images
(`azureclaw-runtime-{anthropic,langgraph,langgraph-ts,maf-python,openai-agents,pydantic-ai}`).

The build system supports two parallel tag channels for every image:

| Channel | Tag form | Purpose |
|---|---|---|
| **Floating** | `:latest` | Track-the-tip channel for development clusters and CI; the controller's image-default constants point here so a `helm upgrade` always picks up the newest sandbox/runtime build. |
| **Pinned** | `:$(VERSION)-$(GIT_SHA)` | Immutable per-build tag for production rollouts, audit trails, and Cosign signature provenance. `VERSION` is read from `cli/package.json`; `GIT_SHA` is the abbreviated commit hash. |

Both tags are produced by every `make image-*` target. Operators choose
which channel to follow per environment by setting the corresponding
override env var on the controller (e.g. `OPENAI_AGENTS_RUNTIME_IMAGE`,
`LANGGRAPH_RUNTIME_IMAGE`, `LANGGRAPH_TS_RUNTIME_IMAGE`,
`ANTHROPIC_RUNTIME_IMAGE`, `MAF_RUNTIME_IMAGE`,
`PYDANTIC_AI_RUNTIME_IMAGE`, `INFERENCE_ROUTER_IMAGE`,
`SANDBOX_IMAGE`).

## Recommended channels per environment

| Environment | Controller / router | Sandbox / runtimes | Why |
|---|---|---|---|
| Local dev / Kind | `:latest` | `:latest` | Fastest iteration. |
| Shared dev / staging | `:$(VERSION)-$(GIT_SHA)` | `:latest` | Pin the control plane (rare changes); float the data plane (frequent rebuilds). |
| Production | `:$(VERSION)-$(GIT_SHA)` for everything | same | Immutable rollouts, signature-pinnable, easy rollback. |

## Tagging a release

Releases are cut by bumping `cli/package.json` and pushing a git tag:

```bash
# 1. Bump version in cli/package.json (e.g. 1.0.0)
# 2. Commit + push to dev
# 3. After dev → main merge:
git tag v1.0.0
git push origin v1.0.0

# 4. Build + push every image with the pinned tag:
make images push push-runtimes  # uses VERSION from package.json + GIT_SHA
```

> **Repo policy:** images are pushed to a **private** ACR. The
> upstream OSS repo is and stays private. Public mirroring is done via
> a separate (non-default) workflow that the maintainers run manually.

## Why `:latest` is also kept

- The controller's image-default constants (`controller/src/reconciler/runtime.rs`)
  fall back to `:latest` when no override env var is set. This is the
  zero-config developer-experience path — `azureclaw up` against a
  freshly-built ACR Just Works without the operator computing a SHA.
- Every Helm chart override (`controller.image.tag` etc.) silently
  defaults to the chart's own version when omitted; explicit `:latest`
  via env override is the documented escape hatch.
- Removing `:latest` would force operators to thread `IMAGE_TAG`
  through every dev workflow. Not worth it.

## Verifying a deployed image's provenance

```bash
# Resolve the floating tag to a digest:
docker buildx imagetools inspect $(REGISTRY)/azureclaw-controller:latest

# Verify the Cosign signature on the digest (keyless OIDC):
cosign verify $(REGISTRY)/azureclaw-controller@sha256:<digest> \
  --certificate-identity-regexp '^https://github.com/Azure/azureclaw' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

See [`supply-chain.md`](./supply-chain.md) for the full Cosign /
SBOM / cargo-deny gates. The Cosign **admission** gate (verify on
`kubectl apply`) is tracked as a v1.1 deliverable — see the v1.1 milestone (`
cosign-admission` in the release plan.
