# Image versioning & release tagging

kars produces eight container images: the controller, the
inference router, the sandbox base + slim overlay, the AgentMesh relay
+ registry, and the five runtime adapter images
(`kars-runtime-{anthropic,langgraph,langgraph-ts,maf-python,openai-agents,pydantic-ai}`).

The build system can produce multiple tags, but the Kars deployment convention
uses one coherent floating channel:

| Channel | Tag form | Purpose |
|---|---|---|
| **Floating** | `:latest` | Track-the-tip channel for development clusters and CI; the controller's image-default constants point here so a `helm upgrade` always picks up the newest sandbox/runtime build. |
| **Build tag** | `:$(VERSION)-$(GIT_SHA)` | Build/release artifact and provenance lookup; not the default controller/runtime override strategy. |

Both tags are produced by every `make image-*` target. Operators choose
which channel to follow per environment by setting the corresponding
override env var on the controller (e.g. `OPENAI_AGENTS_RUNTIME_IMAGE`,
`LANGGRAPH_RUNTIME_IMAGE`, `LANGGRAPH_TS_RUNTIME_IMAGE`,
`ANTHROPIC_RUNTIME_IMAGE`, `MAF_RUNTIME_IMAGE`,
`PYDANTIC_AI_RUNTIME_IMAGE`, `INFERENCE_ROUTER_IMAGE`,
`SANDBOX_IMAGE`).

## Deployment convention

| Environment | Controller / router | Sandbox / runtimes | Why |
|---|---|---|---|
| Local dev / Kind | `:latest` | `:latest` | Loaded or pulled as one coherent build set. |
| Shared clusters | `:latest` | `:latest` | Avoid controller/router/runtime tag drift; use `imagePullPolicy: Always`. |
| Evidence and rollback | Resolve deployed tags to digests | Resolve deployed tags to digests | Record the actual image IDs in receipts, evidence, and release metadata. |

## Tagging a release

Releases are cut by bumping `cli/package.json` and pushing a git tag:

```bash
# 1. Bump version in cli/package.json (e.g. 0.1.18)
# 2. Commit + push to dev
# 3. After dev → main merge:
git tag v0.1.18
git push origin v0.1.18

# 4. Build + push every image with the pinned tag:
make images push push-runtimes  # uses VERSION from package.json + GIT_SHA
```

The source repository is public. Development and private-preview deployments
may use private registries; public release workflows may mirror signed images.

## Why `:latest` is also kept

- The controller's image-default constants (`controller/src/reconciler/runtime.rs`)
  fall back to `:latest` when no override env var is set. This is the
  zero-config developer-experience path — `kars up` against a
  freshly-built ACR Just Works without the operator computing a SHA.
- The Helm values explicitly default controller, router, and sandbox tags to
  `latest`; operators can supply another coherent tag set through values.
- Removing `:latest` would force operators to thread `IMAGE_TAG`
  through every dev workflow. Not worth it.

## Verifying a deployed image's provenance

```bash
# Resolve the floating tag to a digest:
docker buildx imagetools inspect $(REGISTRY)/kars-controller:latest

# Verify the Cosign signature on the digest (keyless OIDC):
cosign verify $(REGISTRY)/kars-controller@sha256:<digest> \
  --certificate-identity-regexp '^https://github.com/Azure/kars' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

See [`supply-chain.md`](./supply-chain.md) for the full Cosign /
SBOM / cargo-deny gates. The Cosign **admission** gate (verify on
`kubectl apply`) is tracked in the [roadmap](../roadmap.md) under
`cosign-admission`.
