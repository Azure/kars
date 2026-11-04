<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

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
The backend is rebuilt from the same pinned source with Go 1.27.3
and explicit security dependency patches in `build-backend.sh`; it is not the
unmodified upstream binary. The build sets upstream's `kubeconfig.Version` to
the pinned 0.41.0 release and executes `-version` to verify it before acceptance;
omitting that linker value otherwise reports `Headlamp unknown`. It rebuilds `tools/headlamp-plugin` from its lockfile
and ships its `package.json` alongside the bundle: Headlamp uses that metadata
to check compatibility, and disables a bundle without it;
no plugin download, package manager, shell or compiler runs in the final image.
The metadata and bundle are copied with explicit read-only permissions for the
non-root runtime (0444 for metadata; 0555 for the bundle tree, including directory
traversal). Retained-product packaging must preserve this: a restrictive build
umask can otherwise make `package.json` or `main.js` unreadable and hide every Kars
route. A loaded metadata entry alone does not prove that the bundle is readable.
Check the served metadata and bundle in a fresh browser context; cached metadata
can conceal this failure.
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
The October 28 Dex scan exposed new OpenSSL, JOSE and Go x509 advisories that
also require refreshing shared Headlamp build inputs. The recipe now pins the
verified Go 1.27.3 builder and AL3 base, and requests JOSE 4.1.6. The fresh
candidate scan also found CVE-2026-63209 in the embedded klauspost/compress
1.18.0; the backend now requests its 1.18.7 fix. Headlamp's existing x/net 0.60.0
request is unchanged. The rebuilt ARM64 candidate passed native version checks,
upstream backend tests, base/frontend/plugin continuity checks, and fresh final-image
and browser-inventory scans with zero HIGH/CRITICAL findings and no ignores.
This qualifies that image, not installed SSO. AMD64 and installed login/RBAC
acceptance remain separate. Changing the base OS alone is
not a vulnerability fix for embedded dependencies or proof of installed behavior.

## Writable frontend lifecycle

Upstream creates a new `.headlamp*` temporary frontend on every server startup
without removing earlier copies. Container restarts retain the pod's temporary
volume: one observed copy was about 25 MiB, and accumulated copies exceeded its
64 MiB limit. Increasing that limit would only postpone the failure.

`build-backend.sh` now applies an exact-match, fail-closed patch to the pinned
upstream function and includes `static-cache.go` in that same backend binary.
The image explicitly sets `HEADLAMP_STATIC_CACHE_NAME=kars-headlamp-static`.
This single directory name is resolved beneath Go's `os.TempDir()` (so a supplied
`TMPDIR` is honored), not a host path, cluster name or address. The server requires
that setting; there is no fallback to the unbounded upstream copy path.

The server creates its private directory with mode 0700, verifies ownership,
rejects a symlink or non-private directory, and replaces only its `frontend`
child before copying. A restart after SIGKILL or an incomplete copy therefore
retains at most one frontend tree, without relying on shutdown hooks. Copy
errors remove partial output and do not change the configured source directory.
Cleanup is directory-handle-confined and does not follow stale symlinks; source
and cache overlap is rejected. The shipped frontend remains immutable, while
the copied frontend remains writable for Headlamp's existing runtime rewriting.
Other temporary files, including legacy `.headlamp*` copies, are not deleted.
Upgrading an affected pod therefore requires pod replacement to release those
legacy copies; restarting its container alone does not migrate them.

The existing deployment supplies one Headlamp process per pod temporary volume.
Independent servers sharing a temporary filesystem must use distinct cache names
(or distinct `TMPDIR` directories); simultaneous servers must not share a cache.
The read-only root filesystem, non-root user, distroless runtime, upstream pins,
plugin metadata permissions and temporary-volume limit are unchanged.

Run the dependency-free native lifecycle/patch checks from the repository root:

```bash
sh sandbox-images/headlamp/test-static-cache.sh
```

Optionally set `HEADLAMP_UPSTREAM_SOURCE` to an already available pinned
`backend/cmd/headlamp.go` to check the exact source match and patched Go syntax.
The script keeps its scratch files and Go cache inside this project, removes
them on exit, disables dependency downloads and uses only the installed Go toolchain.
Without the source path, that one source-integration test is explicitly skipped.
The image build runs the patch checks against the extracted pinned source and
includes lifecycle tests in its existing upstream backend test invocation.

Local Go 1.27.2 checks cover repeated startup without shutdown cleanup, incomplete
copies, stale symlinks, copy failure/recovery, read-only source preservation,
unsafe configuration, source/cache overlap and pinned-source patch validation.
These are not deployed acceptance: the repaired backend/image still needs its
batched build, upstream test suite and image qualification, followed by a fresh
Kind pod and actual login/plugin/frontend journeys with repeated container
restarts and temporary-volume usage checks. No new image or cluster rollout was
performed for this repair.

## Installation and access

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
token as an operator login or reuse a Bridge session token.

### Automatic entry from Bridge

With a browser-SSO-capable IdP shared by the two applications, configure Headlamp's
own OIDC client and set `BRIDGE_HEADLAMP_SSO_ENABLED=true` through Bridge
`web.extraEnv`. Keep `headlamp.url` as the browser-reachable **base** URL, not an
OIDC callback or a URL containing credentials. Leave the flag unset for external
unpatched dashboards and token-only installations; their existing links remain
unchanged.

Bridge then opens this packaged Headlamp's `/oidc?cluster=main&return=/` flow
(or the selected cluster-relative Kars view). The backend binds the return route
to the existing single-use OAuth state and redirects there only after the normal
code exchange and ID-token verification have set Headlamp's own auth cookie.
PKCE, issuer/audience verification and Kubernetes user RBAC remain in effect.
Return routes cannot be external URLs, traversals, queries or fragments. Existing
popup sign-in still uses the upstream `/auth` callback UI when no return route
is supplied. No Bridge session or provider token is passed in a launch URL.

Dex requires native browser sessions and explicit client sharing for this to
avoid another password prompt. Dex 2.45.1 cannot provide that behavior; the
maintained 2.46.1 recipe adds the upstream capability. Enable
`DEX_SESSIONS_ENABLED=true`, configure `sessions.ssoSharedWithDefault: none`, and
list only Headlamp's client ID under the Bridge client's `ssoSharedWith`.
Sharing is directional, and the two clients retain separate audiences and
callback URLs. A legitimate fresh IdP login establishes the browser session;
existing Bridge application cookies cannot retroactively create one. Expired
or revoked IdP sessions can still require authentication. Build/runtime and
installed browser acceptance are separate from these source changes.

For local access, forward `service/headlamp` in namespace `headlamp` to port
3903. Set the Bridge chart's `headlamp.url=http://localhost:3903` to wire the
existing links in both web and BFF. For a remote installation, supply the
operator's real browser-reachable HTTPS endpoint instead. An empty URL preserves
the explicit not-linked state rather than exposing a cluster-internal URL.
