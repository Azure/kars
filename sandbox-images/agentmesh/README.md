<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# AL3 AgentMesh servers

The relay and registry run the unchanged Microsoft AGT modules from the exact
`vendor/agt/pin.json` revision. They share the maintained AGT 5 owner-wheel and
native ACS preparation used by the agent runtimes. This is runtime packaging,
not a second mesh provider or a vendored server implementation.

The AGT 5 `agentmesh-platform` distribution is a deprecated, dependency-only
redirect with no `server` extra. Its upstream Dockerfile still installs that
extra on Debian. Kars instead selects the verified **core owner wheel with its
server extra** and matching native ACS wheel. Python dependency constraints are
shared with Hermes; unused constraints do not install Hermes or its adapters.

Build from the repository root after the normal pinned AGT wheel preparation:

```bash
docker build -f sandbox-images/agentmesh/Dockerfile \
  --build-arg COMPONENT=registry -t agentmesh-registry-agt:latest .
docker build -f sandbox-images/agentmesh/Dockerfile \
  --build-arg COMPONENT=relay -t agentmesh-relay-agt:latest .
```

The source-build paths in `kars dev`, `kars push` and `kars up --build`
prepare these wheels and use this same recipe. `up --build` builds both mesh
components even with `--skip-runtime-images`; mesh failures are fatal rather
than falling back to an older imported image. Local Kind `--no-build` reuses
existing images without a source checkout. Release builds download the shared
verified wheel artifact for each native architecture.
Core Helm defaults use the same `ghcr.io/azure/kars-agentmesh-{registry,relay}`
release repositories, including when per-component image settings are omitted.
Explicit registry mirrors and immutable image overrides remain supported; saved
image values are not silently migrated.

The final base is official Azure Linux 3 distroless Python 3.12. Servers run as
UID/GID 1000 with no shell, package installer, runtime download or source overlay.
Uvicorn is the main process and handles termination directly. `HOST`, `PORT`,
`LOG_LEVEL`, registry/relay configuration and the upstream `/health` endpoints
are unchanged. The component must be `registry` or `relay`; there is no fallback.

Image/native, fresh-install, identity and encrypted cross-harness acceptance
must be recorded separately. Do not replace live registry state or call a
successful image build an accepted customer collaboration.
