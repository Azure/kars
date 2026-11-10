<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Attributable Headlamp build-tool polyfill repair

H21's `plugin-lock-scan.json` reports **CVE-2025-14505, LOW**, in
`elliptic@6.6.1`, with no fixed version:

```text
@kinvolk/headlamp-plugin 0.13.1
  → vite-plugin-node-polyfills 0.23.0
    → node-stdlib-browser 1.3.1
      → crypto-browserify 3.12.1
        → browserify-sign 4.2.6 → elliptic 6.6.1
        → create-ecdh 4.0.4 → elliptic 6.6.1
```

The immutable tool's **only executable polyfill consumer** is
`config/vite.config.mjs`, explicitly including `process`, `buffer`, and `stream`.
`bin/headlamp-plugin.js` loads this configuration for both production `build` and
`start` (which calls `vite.build` with watch enabled, not a Vite development
server). The plugin's Vitest configuration also imports it. Storybook uses its
separate, unchanged Webpack configuration and retains the patched checker.
Crypto was not enabled by this consumer and is not implemented by this patch.

## One maintained installed path

`prepare.py` follows the existing checker archive recipe: verify the original
archive's pinned SHA-512, reject unsafe entries/unexpected consumer text, preserve
upstream file contents and modes, and write a sorted deterministic gzip/tar.
`upstream.json` identifies the immutable npm archive, original name/version,
original consumer hashes and verified upstream Apache-2.0 license. The original
npm archive does not contain a top-level license file, so the reviewed upstream
license is included as `UPSTREAM-LICENSE`; Kars additions carry `KARS-LICENSE`
(MIT). `karsPatch` and `KARS-PATCH.json` explicitly identify the modifications.
There is no fake package version, npm alias, scanner suppression or external fork.

The original `package.json`, `config/vite.config.mjs`, and the KEDA example's
`deploy/authentication.yaml` change. The actual broad polyfill dependency and its
shim resolver are removed. The tool instead
requests `@rollup/plugin-inject@5.0.5`, `buffer@6.0.3`, `process@0.11.10` and
`stream-browserify@3.0.0`. Its generated `config/kars-polyfills.mjs` is derived
from **the existing maintained `productionPolyfills` function in
`sandbox-images/headlamp/build-frontend.mjs`**, not a second hand-maintained
implementation. It preserves bare and `node:` aliases, free Buffer/process/global
injection, local bindings and real stream implementations. Unexpected crypto
imports fail explicitly instead of silently producing browser stubs.

The CLI, Storybook config, TypeScript loader/checker, other Vite plugins, UMD
externals, tests, templates and Go examples are retained. The existing npm
postinstall remains responsible for the two RabbitMQ example-module repairs.
Unchanged templates are upstream project-generation inputs, not additional
installed dependency roots or shipped application assets.

The root manifest installs `file:build-tool/headlamp-plugin-0.13.1-kars.tgz`;
Docker copies that archive before the existing `npm ci`. npm generated the new
lock with lockfile-only installation and no lifecycle scripts. No surviving
package version changed: **87** entries became unreachable solely within the old
polyfill dependency graph and were removed. Five surviving entries have only
npm-generated registry/integrity re-resolution metadata changes. The actual root lock no longer contains elliptic, crypto-browserify,
node-stdlib-browser, vite-plugin-node-polyfills, or deepmerge. Build-tool
`node_modules` and these archives are **not copied into the final AL3 image**.
The resulting plugin JavaScript is shipped and remains subject to browser checks.

## H23 packaged credential-example finding

The expanded archive scan identified `kubernetes-secret-yaml` at
`package/official-plugins/keda/test-files/deploy/authentication.yaml:2`; its redacted
matched-content SHA-256 was
`50a7dc2ac7056d9ed3524f9dcc4db9fee6ed4df654b86a9dd323ed3181b0404d`.
Inspection of the integrity-verified public npm source confirms documentation
placeholders in cluster-local RabbitMQ/Redis connection values and the Redis
scaler password, not evidence of a live deployment credential. Their values are
not reproduced here. Nevertheless, the example would install those defaults as
real Kubernetes Secrets if applied unchanged.

The recipe now verifies the original authentication file hash and removes only
its two Secret documents. Both upstream TriggerAuthentication documents, all
workloads, the publisher/consumer programs and their tests remain intact. The
packaged `deploy/README.md`, generated from `keda-example-credentials.md`, explains
provisioning the identical Secret names/keys from private, user-supplied environment
files with real `kubectl create secret` commands before applying the example.
There is no encoded replacement credential, disabled scanner rule, renamed Secret
kind, or invented authentication implementation. This archive is an example build
tool input, not a Secret automatically applied to the shipping Headlamp deployment.

The successor retains **1,611 upstream files byte-and-mode identical**, changes
three originals, adds five attributed files and removes no files. The two retained
authentication documents are unchanged apart from document separators. The npm
lockfile-only refresh changes **only the tool archive integrity**: no dependency
names, versions or requirements change. `karsPatch.exampleAuthentication` records
the patched manifest and provisioning-guide hashes. The frozen H23 source/archive
is not modified; only the working successor package is regenerated.

Focused successor validation:

```sh
node --test tools/headlamp-plugin/build-tool/checks.node.mjs sandbox-images/headlamp/build-frontend.test.mjs
python3 -B tools/headlamp-plugin/build-tool/prepare.py --check --upstream tools/headlamp-plugin/.credential-verification/upstream.tgz
```

**17 tests passed, zero skipped**, including actual YAML parsing, retained resource
names/keys, real Vite imports and existing polyfill tests. Deterministic reproduction,
complete upstream-file comparison, integrity-only lock delta, and provisioning
command shell syntax also passed. Verification downloads are cleaned after use;
plain `prepare.py --check` downloads and verifies the same immutable public source.
The installed compiler/Storybook tests, fresh full archive scan and live KEDA
credential/deployment journey remain hosted/customer qualification gates.

## Reproduce and verify

From the repository root:

```sh
python3 -B tools/headlamp-plugin/build-tool/prepare.py --check
node --test tools/headlamp-plugin/build-tool/checks.node.mjs
node --test sandbox-images/headlamp/build-frontend.test.mjs tools/headlamp-plugin/patch-build-tool.test.mjs
node --test tools/headlamp-plugin/checker/compile.node.mjs
```

`prepare.py` without `--check` regenerates the archive. `--upstream PATH` accepts a
previously downloaded original archive but never bypasses its SHA-512 check.
Regenerate the archive and use npm lockfile-only resolution when its source
changes, including changes to the canonical frontend polyfill function. Do not
hand-edit the archive or lock metadata. Remove this local recipe when a reviewed
upstream release provides the same dependency repair and passes the same gates.

The eight-test Node suite is imported by `patch-build-tool.test.mjs`, so the
existing hosted `npm test` executes it without adding another Vitest-discovered
test. It checks archive/lock integrity, provenance, retained real commands,
canonical source equivalence, direct dependency versions, all nested installed
paths, Docker ordering, and **imports the actual patched Vite configuration**.
The canonical frontend tests exercise real Rollup injection and actual package
exports. The existing real compiler regression remains unchanged.

Local combined validation passed **32 tests, zero skipped** using the existing
installed dependencies and a verified, separately extracted deepmerge-ts archive:

```sh
KARS_CHECKER_MERGE_ARCHIVE=tools/headlamp-plugin/.polyfill-verification/deepmerge-ts.tgz node --test sandbox-images/headlamp/build-frontend.test.mjs tools/headlamp-plugin/patch-build-tool.test.mjs
```

The repository-local verification downloads/fixtures were subsequently cleaned.
This archive-mode run is **not** acceptance of the stale local installed graph,
full Vitest suite, TypeScript workers, Storybook build or shipping image. An initial
new test incorrectly expected the upstream Storybook range `^9.1.19`; the verified
archive retains `^9.1.17`. The assertion was corrected and the combined run passed.

## Required hosted proof

Run clean `npm ci`, the existing complete plugin tests, actual plugin Vite build
and watch flow, the unchanged real checker compiler regression (valid → TS2322 →
recovery), and Storybook compilation. The existing Docker build now also builds
Headlamp against Helm 3.21.4 and runs upstream signing/verification tests; see the
[backend repair](../../../sandbox-images/headlamp/README.md#h21-openpgp-and-plugin-polyfill-follow-up).
Re-run all-severity image, frontend lock/inventory and plugin lock scans with dev
dependencies included. Fresh browser validation must exercise the plugin,
Buffer/process/stream paths and real Helm operations. No passing image scan or
browser acceptance is claimed from the lightweight local tests.

No shared CI edits are made here. Existing image build and npm-test commands cover
the new build/test integration. The parent may additionally run the reproducibility
command above and retain the backend's new `headlamp.deps.txt` alongside exported
`headlamp.go.mod`/`headlamp.go.sum` and binary build information.
