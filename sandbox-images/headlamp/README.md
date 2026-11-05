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
patches in `build-frontend.mjs`. Vite 6.4.5 replaces the vulnerable 6.4.3 build
pin, and fast-uri 3.1.9 replaces 3.1.7 to address URI scheme/host normalization
and relative-reference advisories. These source pins require a fresh hosted image
build and all-severity scan; prior clean image evidence does not cover the update.
Run `node --test sandbox-images/headlamp/build-frontend.test.mjs` to check the
actual preparation command, preserved upstream settings and pinned product identity.
Its 3 GiB Node heap replaces the upstream 8 GiB heap request for bounded builders;
product/version environment and the Vite configuration outside the scoped polyfill
replacement are preserved. The build emits rendered-module/package evidence, output hashes,
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

## Scoped production frontend dependencies (2026-11-05)

[deepmerge GHSA-gcqx-w48m-w2h4](https://github.com/advisories/GHSA-gcqx-w48m-w2h4)
and [elliptic GHSA-848j-6mx2-7j84](https://github.com/advisories/GHSA-848j-6mx2-7j84)
have no published patched releases. The repair removes their unused dependency
paths from the actual production build instead of suppressing findings, renaming
vulnerable packages or claiming an unpublished fix.

Inspection of the pinned
[frontend manifest](https://github.com/kubernetes-sigs/headlamp/blob/066c5be9deb14b12bcef3485b531c64a84a2a219/frontend/package.json),
[source lock](https://github.com/kubernetes-sigs/headlamp/blob/066c5be9deb14b12bcef3485b531c64a84a2a219/frontend/package-lock.json),
[Vite configuration](https://github.com/kubernetes-sigs/headlamp/blob/066c5be9deb14b12bcef3485b531c64a84a2a219/frontend/vite.config.ts)
and published package code established these paths:

- Storybook → fork-ts-checker 8 → deepmerge in the reported regenerated lock;
  the pinned source lock additionally has RSBuild SVGR → deepmerge and
  RSBuild SVGR → SVGR SVGO → deepmerge. Vite's SVGR 4.3.0 instead declares
  SVGR core and JSX, not the SVGO plugin. Production SVG conversion is retained.
- Vite node-polyfills → node-stdlib-browser → crypto-browserify →
  create-ecdh/browserify-sign → elliptic. RSBuild's node-polyfill plugin independently
  declares crypto-browserify. The production Vite configuration enables only
  process, buffer and stream; selecting these aliases did not prevent installation
  of the unused crypto dependency tree.

`prepare` now removes `devDependencies` from the generated build manifest, replaces
all upstream scripts with the single bounded Kars production build command, and
removes the extracted `.storybook` and `rsbuild.config.ts` configurations. The
packaged product does not provide those development workflows: its maintained build
already called Vite directly, with `npm ci --omit=dev`. This change removes the
unused roots from lock resolution too. Apart from the lint-only roots removed in the
hosted follow-up below, other regular dependencies, optional platform packages,
existing security pins and unrelated Vite settings are retained. It does
not alter the upstream repository or claim its original development lock is fixed.

The broad Vite polyfill plugin is removed, not retained as an alternate path.
An exact-match, fail-closed patch removes its import and invocation from the pinned
Vite configuration. The build supplies `productionPolyfills` using direct
`buffer@6.0.3`, `process@0.11.10`, `stream-browserify@3.0.0` and
`@rollup/plugin-inject@5.0.5`. Small ESM adapters retain default/named Buffer and
process exports, free Buffer/process/global injection, and plain and `node:` aliases
for the three selected builtins. The upstream `global: 'globalThis'` definition is
unchanged. No crypto implementation is fabricated: an attempted `crypto` or
`node:crypto` import fails the build rather than receiving an empty browser module.
The alternative Rollup polyfill package was rejected because it stubs crypto.

Before invoking Vite, `build` verifies the regenerated npm v3 lock matches the
prepared dependency roots, has no root development dependencies, and contains
neither deepmerge nor elliptic (including nested paths and named aliases). The
read-only `verify-lock` command exposes the same check for hosted qualification;
it does not replace scanning for every other advisory or alter lock contents.

### Hosted qualification and scanning handoff

The Dockerfile retains the **actual regenerated** `package.json`, `package-lock.json`
and patched `vite.config.ts` in `/usr/share/kars/headlamp/frontend-build/`, mode 0444.
`frontend-evidence.json` records their SHA-256 hashes in `buildInputs`. No installed
frontend `node_modules` is shipped; bundled code and the lock evidence are distinct.
The retained lock intentionally exposes build dependencies to scanning as well.

The hosted image-qualification job must extract that entire directory together
with `/usr/share/kars/headlamp/frontend-evidence.json`, verify the three hashes,
run the existing all-severity source-lock scanner on the canonical
`frontend-build/package-lock.json`, and retain the unfiltered report and inputs as
artifacts even on failure. Keep final-image, rendered-inventory and plugin scans;
no existing scan is removed or downgraded. If a job also inventories the untouched
upstream development lock, retain its results as upstream-source evidence rather
than misidentifying that lock as the packaged production build. The shared CI
workflow is not modified by this change.

Local tests exercise manifest/configuration preparation, identity/drift rejection,
idempotence, lock rejection, exact alias wiring and crypto rejection. With existing
`tools/headlamp-plugin` dependencies, they also exercise real Rollup injection and
Buffer/process/stream behavior without bundling. These two integration checks skip
explicitly if those dependencies are absent; tests never install them. All fixtures
stay in this directory and are removed afterward.

Still required on GitHub-hosted runners: fresh lock resolution and installation,
full frontend/image build on supported architectures, all-severity lock/image/
rendered-inventory scans, and fresh browser checks covering SVGs, Monaco, terminal/
stream consumers and loaded plugins. The polyfill integration has not been bundled
or exercised in a browser locally. Preserve failed evidence; the source repair and
lightweight tests alone are not image or installed acceptance.

## Hosted all-severity follow-up (2026-11-05)

[CI 42020001347, Headlamp job 127220711620](https://github.com/Azure/kars/actions/runs/42020001347/job/127220711620)
built the real AL3 image at `c6a255af`, including the scoped polyfills, and verified
its exported build-input hashes. Its retained scans **failed**: 13 image findings
(6 MEDIUM, 4 LOW, 3 UNKNOWN), 31 frontend-lock findings (19 HIGH, 11 MEDIUM,
1 LOW), 1 rendered-inventory MEDIUM finding, and 22 plugin-source findings
(6 CRITICAL, 14 HIGH, 2 MEDIUM). YAML overlaps the image/lock/inventory reports;
these are occurrences, not 67 distinct advisories. Neither deepmerge nor elliptic
appeared in the frontend findings. The following repairs need a new hosted run;
they do not change that failed evidence into a successful qualification.

### Frontend: retain production consumers, remove only unused lint roots

The exported Vite configuration invokes React/Babel, SVGR and static-copy plugins,
not ESLint. The rendered module evidence contains no ESLint packages. Traversing
the actual regenerated lock, including dependency/optional/peer edges, shows that
removing these ten unused roots eliminates all installed brace-expansion, flatted
and minimatch packages, including the affected minimatch 9 path:

- `@headlamp-k8s/eslint-config`, `@typescript-eslint/eslint-plugin`, `eslint`,
  `eslint-config-prettier`, `eslint-plugin-import`, `eslint-plugin-jsx-a11y`,
  `eslint-plugin-react`, `eslint-plugin-react-hooks`,
  `eslint-plugin-simple-import-sort`, `eslint-plugin-unused-imports`.
- The removed paths include ESLint → file-entry-cache → flat-cache → flatted,
  ESLint/config/plugins → minimatch 3 → brace-expansion 1, and TypeScript ESLint →
  typescript-estree → minimatch 9 → brace-expansion 2. There is no forced minimatch
  9-to-11 API migration and no continued installation of an unused linter.
- Generated `eslintConfig`, `prettier`, `husky` and `lint-staged` configuration is
  removed with its unsupported consumers. The obsolete upstream `axe-core` and
  `domain-browser` `npm:dry-uninstall` overrides are removed, not used as fixes.
  Repository linting, source files and the real Vite consumers are unchanged.

| Reported package/version | Advisory IDs (CVE-2026 unless stated) | Repair |
| --- | --- | --- |
| `@babel/core` 7.28.3 | 49356 | 7.29.6; retained React/SVGR JSX compilation |
| `brace-expansion` 1.1.12 / 2.0.2 | 102276, 102278, 13149, 14257, 69152, 102277, 33750 | Remove unused lint dependency paths |
| `browserslist` 4.25.3 | 73088, 73089 | 4.28.7; retain Babel target configuration |
| `flatted` 3.3.1 | 32141, 33228 | Remove unused ESLint cache path |
| `minimatch` 9.0.9 | 118764 | Remove unused TypeScript ESLint path |
| `picomatch` 2.3.1 / 4.0.2 | 33671, 33672 | Same-major 2.3.2 / 4.0.4; retain Vite/Rollup/file matching |
| `postcss` 8.5.3 | 45623, 73646, 41305, 69153 | 8.5.23; retain Vite CSS processing |
| `source-map-js` 1.2.1 | 93749 | 1.2.2; retain PostCSS source maps |
| `yaml` 1.10.2 / 2.7.0 | 33532 | Same-major 1.10.3 for cosmiconfig; 2.8.3 for the app |

The build asserts that patched `yaml@2.8.3`, as well as `js-yaml@4.3.2`, is present
in rendered package evidence. Every remaining lock entry is still scanned.
Published npm manifests confirm the selected patch versions support the Node 22
builder; local tests do not substitute for resolving and building the new lock.

### Backend: update the dependencies of real Helm and telemetry features

Headlamp's `pkg/helm/{charts,release,repository}.go` uses Helm repository/search,
chart loading, install, upgrade, rollback and uninstall APIs. Helm retains its
containerd dependency. `cmd/headlamp.go` initializes `pkg/telemetry` and uses its
middleware/spans. These are real consumers, not dependencies that can be removed
because they appeared in a scan. `build-backend.sh` now requests:

| Embedded package/version | Finding | Minimum selected requirement |
| --- | --- | --- |
| containerd 1.7.33 | CVE-2026-53493, CVE-2026-53495, CVE-2026-95838 | 1.7.36 |
| OTLP trace, gRPC and HTTP exporters 1.42.0; OTel SDK 1.44.0 | CVE-2026-81870; HTTP also CVE-2026-39882 | All four modules 1.45.0 |
| x/crypto 0.57.0 | GO-2026-5932 (UNKNOWN) | 0.58.1-0.20261009181718-2f1f834330b2 |
| x/net 0.60.0 | CVE-2026-78661, CVE-2026-97045 (UNKNOWN) | 0.62.0 |
| Helm 3.18.5 | CVE-2026-35206 | 3.20.2 |

These module versions were verified against the public Go proxy. The crypto
pseudo-version is the published advisory fix, not a fabricated release. Their
Go requirements fit the existing 1.27.3 builder. Helm's upgrade also raises the
Kubernetes client module floor from 0.33 to 0.35: that compatibility change must
pass the hosted build/tests, not be inferred from version ordering. The maintained
backend test command now includes `./pkg/helm ./pkg/telemetry` alongside its
existing packages. Go's normal minimum-version selection and checksum verification
remain enabled; no replacement module or compatibility stub is introduced.

### Plugin build tool: patch actual retained Go examples

The 22 plugin-source findings were eleven RabbitMQ advisories in **each** of
`@kinvolk/headlamp-plugin/official-plugins/keda/test-files/{consume,publish}/go.mod`:
CVE-2026-77403, -77404, -77405, -77406, -77407, -77408, -77410, -77411, -77412,
-79921 and -107386. Both modules request `github.com/rabbitmq/amqp091-go@v1.3.4`.
They are development examples inside npm build-tool 0.13.1, not shipped Go binaries
or dependencies of the Kars browser bundle. Inspection of published build-tool
0.14.0 confirmed the same vulnerable requirements, so upgrading that npm package
alone would not repair them.

`tools/headlamp-plugin/patch-build-tool.mjs` is the single npm `postinstall` path
used by hosted installs and the Docker build. It retains both example programs,
changes their real Go requirements to AMQP 1.14.0, raises their Go directive to
AMQP's minimum 1.20, and updates `go.sum` using the published module/go.mod hashes
from `sum.golang.org` (including its goleak test dependency). A subsequent example
build therefore resolves the patched library; this is not a renamed package,
changed scanner label, deleted fixture, or ignored advisory. The actual Dial,
channel, queue, publish/consume and delivery-acknowledgement APIs used by these
examples remain exported with compatible signatures in 1.14.0.

The patch validates the locked npm package version and both original modules
before any write, preserves other dependencies and all Go source, and supports
idempotent execution. Tests cover upstream drift, checksums, no partial writes,
install wiring and copies of the actual installed examples. The Docker context
copies the helper before `npm ci`; no runtime installer is added. Remove this
patch when a reviewed upstream build-tool release fixes both example modules.

### Coverage gap and remaining qualification

The retained plugin report contains **only the two Go targets**, not the npm lock.
Its Trivy filesystem invocation lacked `--include-dev-deps`, while all plugin npm
roots are `devDependencies`. Therefore that report is not proof that the plugin
build dependency graph is clean. The shared CI owner must add
`--include-dev-deps` to **filesystem** scans (both frontend-lock and plugin-lock),
retaining `--severity UNKNOWN,LOW,MEDIUM,HIGH,CRITICAL`, `--ignore-unfixed=false`,
`--list-all-pkgs`, failure exit codes and artifact retention. Confirm the next
plugin report actually contains `package-lock.json`; any newly exposed findings
still require repair. In the existing scan loop, build the additional arguments as
`dependency_args=(); if [ "$mode" = fs ]; then dependency_args+=(--include-dev-deps); fi`
and pass `"${dependency_args[@]}"` to Trivy. Fail if either lock report lacks its npm
inventory, for example:

```bash
jq -e 'any(.Results[]?; .Type == "npm" and ((.Target // "") | endswith("package-lock.json")) and ((.Packages // []) | length > 0))' "$HEADLAMP_EVIDENCE_DIR/plugin-lock-scan.json"
```

No shared CI or audit document is edited here.

Focused local checks, using existing dependencies and no installs/builds/containers:

```bash
node --test sandbox-images/headlamp/build-frontend.test.mjs tools/headlamp-plugin/patch-build-tool.test.mjs
sh -n sandbox-images/headlamp/build-backend.sh
cd cli && ./node_modules/.bin/vitest run src/testing/helm-headlamp.test.ts -t 'patches frontend dependencies|reuses pinned upstream assets'
```

Results: **15/15 Node tests passed, zero skipped**; shell syntax passed; **2/2
selected CLI tests passed** (64 unrelated tests intentionally unselected). Scoped
`git diff --check` also passed. No full dependency installation, Go compilation or
image build was run locally.

The CLI fixture now supplies the real Vite preparation contract, checks pruning,
pins, preserved configuration and idempotence, and keeps its files inside the
repository. Production validation is not relaxed. Required hosted gates remain:
fresh npm lock resolution/installation; native backend/frontend/plugin image
builds and upstream tests; compilation of the retained patched Go examples;
exported-input/hash validation; unfiltered all-severity image, frontend lock,
rendered inventory and **development-inclusive** plugin lock/source scans; and
fresh browser checks for login, Helm, YAML editing, Monaco, SVGs, terminal streams
and loaded plugins. This source repair is not a clean-scan or browser-acceptance claim.

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
