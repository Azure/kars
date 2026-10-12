# Prometheus on Azure Linux 3

This recipe retains Prometheus **3.11.3**, Docker and Swarm discovery, and both
the legacy React and current Mantine interfaces. The runtime is AL3 distroless,
runs as UID/GID 65534, and contains no Node.js, Go compiler, shell or package
manager. Build tools are confined to intermediate stages.

Build from the repository root with `make image-prometheus`. The result is
`$(REGISTRY)/kars-prometheus:latest`; this target does not deploy the image.
Mount writable storage at `/prometheus` when running with a read-only root.

The backend uses Go 1.27.2, frozen module resolutions, and two checksum-bound
upstream Moby split-client migration patches. The frontend retains the release
source and updates Nano ID 3, PostCSS 8 and React Router 7 within their existing
major versions. Other dependency resolution starts from the upstream lockfiles.
Router overrides also constrain hoisted peer-dependency copies used by
`use-query-params`; pinning only the application's direct dependency is insufficient.
The current workspace lock is refreshed for these four package names explicitly;
an ordinary lock-only install can retain older workspace resolutions.
Build output includes separate JavaScript package inventories and asset hashes
for both interfaces. CSS-only dependencies are recorded separately. Distributed
dependency license/notice files are retained in archives alongside the upstream
LICENSE and NOTICE.

Legacy static CommonJS assets are attributed using the upstream compiler's
`--stats` output. Embedded helper sources are matched against their distributing
package's published source map, without inventing dependency versions. Raw
compiler statistics are kept out of served assets. The Docker build context
allowlist excludes unrelated checkout files, caches and qualification artifacts.

Run packaging regressions with:

```sh
node --test sandbox-images/prometheus/build-frontend.test.mjs
```

Image construction is not deployment acceptance. Qualify the exact resulting
image on each supported architecture, scan its OS/Go and embedded browser
inventories, and exercise installed monitoring before promoting it. The
unmodified legacy upstream test suite has known failures; retain baseline and
patched test reports rather than presenting those failures as passing tests.
