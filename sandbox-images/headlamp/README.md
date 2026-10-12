# Headlamp on Azure Linux 3

Build from the repository root with `make image-headlamp` or:

```bash
docker build -f sandbox-images/headlamp/Dockerfile -t kars-headlamp:latest .
```

Where direct npm access is restricted, pass the existing approved registry with
`--build-arg NPM_CONFIG_REGISTRY="$(npm config get registry)"`. Lockfile integrity
checks remain enabled; registry configuration is confined to the build stage.

The recipe retains the existing CLI's Headlamp 0.41.0 version, pins its upstream
multi-platform image, and copies its shipped plugins into AL3 distroless.
The frontend is rebuilt from the same pinned source with targeted npm security
patches in `build-frontend.mjs`. Its 3 GiB Node heap replaces the upstream 8 GiB
heap request for bounded builders; product/version environment and Vite configuration
are preserved. The build emits rendered-module/package evidence, output hashes,
and a CycloneDX inventory, including the separately copied Monaco assets.
These describe actual build outputs, not every development dependency in the lock.
The backend is rebuilt from the same pinned source with Go 1.27.2
and explicit security dependency patches in `build-backend.sh`; it is not the
unmodified upstream binary. It rebuilds `tools/headlamp-plugin` from its lockfile
and ships its `package.json` alongside the bundle: Headlamp uses that metadata
to check compatibility, and disables a bundle without it;
no plugin download, package manager, shell or compiler runs in the final image.
The first AL3 wrapper's scan found 49 HIGH issues in the upstream Go backend,
not the AL3 packages. The patched ARM64 image subsequently passed the same
read-only offline frontend/plugin proof and a complete supported-package scan
with zero HIGH/CRITICAL findings, including unfixed advisories and no ignores.
The rebuilt ARM64 candidate subsequently passed the same offline proof, a native
AL3 Chromium check showing both Kars and Prometheus loaded, and fresh image and
217-package browser-inventory scans with zero HIGH/CRITICAL findings.
The bounded split build reused unchanged backend/plugin products; it was not a
new monolithic Dockerfile build.
The source-lockfile companion found 129 HIGH/CRITICAL advisory occurrences:
46 in the frontend, 18 in the Kars plugin and 65 in the shipped Prometheus
plugin, including development/build dependencies. The shipped Prometheus bytes
match its pinned 0.8.2 release. These are not 129 demonstrated runtime
vulnerabilities, but the frontend directly imports affected `js-yaml` and
`lodash` versions. The new frontend rebuild pins their fixes along with Vite,
fast-uri and nanoid. Its source lock still reports 29 advisory occurrences,
separate from the clean rendered-package inventory. The unchanged Kars and
Prometheus source locks report another 18 and 65 occurrences. Production-source
and byte-bound UMD import analysis shows their runtime imports are externalized
to Headlamp's `pluginLib`, rather than independently bundling the advisory-bearing
compiler/development dependencies. The host-provided libraries are covered by
the rebuilt frontend inventory. This does not erase source/build-tool advisories
or claim absence of other vulnerabilities.
AMD64 and installed login/RBAC acceptance also remain separate. Changing the base OS alone is
not a vulnerability fix for embedded dependencies or proof of installed behavior.

The Core chart owns the `headlamp` namespace, Deployment, ServiceAccount, Service
and NetworkPolicy. No third Helm release or post-install plugin copy is needed.
An existing independently managed Headlamp must remain external by setting
`headlamp.enabled=false`; do not force Helm to adopt it. The namespace is kept on
uninstall so unrelated namespace contents cannot be destroyed.

The local template-and-apply path carries the same explicit Core owner marker,
so legacy CLI steps cannot replace it with the upstream image or copy a second
plugin into its pod. Local builds/releases require and load `kars-headlamp:latest`;
the Kind overlay uses `imagePullPolicy: Never`. Cloud import/build and upgrade
paths select the corresponding image in the installation's registry.

The dashboard ServiceAccount has **no resource permissions**. In-cluster
configuration supplies the API address and CA, not ambient cluster-admin.
Operators sign in with their own Kubernetes credential; its existing RBAC
controls every request. Do not mint the unprivileged dashboard ServiceAccount's
token as an operator login or reuse a Bridge session token. Headlamp does not
inherit Bridge's Dex login automatically.

For local access, forward `service/headlamp` in namespace `headlamp` to port
3903. Set the Bridge chart's `headlamp.url=http://localhost:3903` to wire the
existing links in both web and BFF. For a remote installation, supply the
operator's real browser-reachable HTTPS endpoint instead. An empty URL preserves
the explicit not-linked state rather than exposing a cluster-internal URL.
