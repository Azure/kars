<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Attributable Webpack checker consumer patch

This is the single installed checker path for the Headlamp plugin's Storybook
Webpack builder. It repairs **GHSA-gcqx-w48m-w2h4 / CVE-2026-93753 (LOW)** without
removing Storybook, disabling TypeScript checking, or suppressing scanning:

```text
@kinvolk/headlamp-plugin 0.13.1
  -> @storybook/react-webpack5 9.1.20
    -> @storybook/builder-webpack5 9.1.20
      -> fork-ts-checker-webpack-plugin 8.0.0
        -> deepmerge 4.3.1 (removed)
```

The checker's sole `deepmerge` consumer combines cosmiconfig options with explicit
constructor options. Its Webpack hooks, schema validation, TypeScript workers,
worker pools and public entry point are unchanged. Upgrading to checker 9.1.0
would still retain `deepmerge`; replacing the whole checker/framework is not
necessary. Vite plugin builds and the Headlamp CLI's separate `tsc` command are
also retained.

## Source, attribution and actual dependency change

- `upstream.json` pins the original npm package name, version, canonical archive
  URL and SHA-512 integrity. The archive is the unmodified upstream **8.0.0**
  release, not a new upstream or published fork release.
- `prepare.py` verifies that immutable archive before changing anything. It
  preserves 166 upstream files byte-for-byte, including the original MIT license
  and attribution. Only `package.json` and the import/call in `lib/plugin.js`
  change; the adapter, `KARS-PATCH.json` and `KARS-LICENSE` are added.
- `merge-options.cjs` uses the maintained, ordinary (prototype-safe)
  `deepmergeCustom` API from **deepmerge-ts 8.0.2**, under BSD-3-Clause. That real
  dependency is installed with its own upstream license. No `deepmerge` code or
  package has been renamed, aliased, hidden, or assigned a fictitious version.
- The checker's actual dependency on `deepmerge` is removed from its manifest;
  `deepmerge-ts` is added at an exact version. Its Node floor becomes 16.9.0,
  matching the replacement library; the image builds with Node 22.
- The generated tarball keeps the original checker package name/version and adds
  explicit `karsPatch` metadata with upstream provenance and the adapter's SHA-256.
  Its distinct integrity is recorded by npm in the lockfile.
- The root's direct local-file dependency plus the builder's `$` override select
  one checker installation. `deepmerge` is absent from the generated lock graph.
  No unrelated locked dependency versions were refreshed.

The artifact is `fork-ts-checker-webpack-plugin-8.0.0-kars.tgz`. It is a small,
reproducible local package, **not** downloaded from a fork registry. Docker copies
it before `npm ci`, using the same lock and package as ordinary development/CI.
No runtime postinstall checker rewrite or second fallback implementation exists.
The package is a build/development dependency: the final shipping image copies
only the built plugin output and root plugin manifest, not this tool's
`node_modules`, archive, or checker workers. Build dependencies remain subject to
all-severity scanning, including dev dependencies.

## Preserved merge behavior

Explicit options override external options; nested records merge and arrays
concatenate. Array holes, recursive cloning, explicit `undefined`, null, false,
zero, empty strings, own enumerable symbols, and Date/RegExp/function/React-element
references retain the old behavior. Other object/class/map/set values retain the
old enumerable-property cloning behavior. Inherited/non-enumerable keys are not
copied. The adapter is only for this two-object constructor call, not a replacement
implementation of the public `deepmerge` API.

Intentional security correction: own `__proto__`, `constructor`, `toString` and
similar keys remain inert own data properties rather than invoking prototype
setters or being dropped because of inherited names. Neither input, result nor
global prototypes are modified. Both explicit and discovered options still pass
the existing schema validation. Callers must not turn inert data into unsafe
assignments elsewhere; this patch changes only the checker's actual merge consumer.

## Reproduce and verify

From the repository root, with Python 3 and network access to the original public
npm archive (no compilation or npm installation is involved):

```sh
python3 -B tools/headlamp-plugin/checker/prepare.py
python3 -B tools/headlamp-plugin/checker/prepare.py --check
cd tools/headlamp-plugin
npm update fork-ts-checker-webpack-plugin --package-lock-only --ignore-scripts --no-audit --no-fund
```

`--upstream <archive-path>` accepts an already downloaded mirror/source archive;
it still must match the pinned upstream SHA-512. `--check` compares the entire
reproduced archive, not only metadata. Files are sorted and tar/gzip timestamps,
ownership and gzip filename are normalized. Integrity/source/dependency drift is
fatal and requires review. Review lock changes: only the root declaration,
checker entry, removed `deepmerge` entry and added `deepmerge-ts` entry should
change. Commit the source, recipe and generated archive together.

With the plugin's dependencies already installed:

```sh
node --test tools/headlamp-plugin/patch-build-tool.test.mjs
```

The existing Node test entry point imports `checks.node.mjs`; the npm test script
and Vitest exclusion remain unchanged. The `.node.mjs` name prevents Vitest from
also treating these as component tests. Normal tests verify the installed checker
and Storybook resolve to the same patched package. They exercise the real checker
constructor/cosmiconfig/schema, not a mock; they do not spawn compiler workers.

For resource-constrained verification against stale installed dependencies, the
explicit `KARS_CHECKER_MERGE_ARCHIVE` environment variable can point to an already
downloaded **real deepmerge-ts 8.0.2 archive**. Tests verify its lock integrity and
extract it beside the patched checker in a repository-local disposable fixture;
they never modify the user's `node_modules`. This verifies packaged behavior,
**not** the user's stale installed graph. Fixtures are removed after testing.

Local verification for this repair: **17 Node tests passed, zero skipped** using
that archive mode; deterministic reproduction, invalid-upstream rejection,
preservation of all 166 untouched upstream files, and the four-entry lock delta
also passed. The combined frontend/build-tool command passed **26 tests**, zero
skipped:

```sh
KARS_CHECKER_MERGE_ARCHIVE=tools/headlamp-plugin/.checker-verification/deepmerge-ts.tgz node --test sandbox-images/headlamp/build-frontend.test.mjs tools/headlamp-plugin/patch-build-tool.test.mjs
```

That repository-local downloaded archive was removed after verification; normal
hosted tests use the installed, locked dependency instead. One earlier test
assertion expected the wrong schema-error wording;
it was corrected to assert the actual memory-limit validation error. The full
component suite was not rerun or claimed passing with stale installed Vitest.

## Installed Webpack/TypeScript compiler regression

After the hosted plugin's clean `npm ci`, run this separate command from the
repository root:

```sh
node --test tools/headlamp-plugin/checker/compile.node.mjs
```

`compile.node.mjs` is deliberately not discovered by Vitest and is not added to
the existing lightweight Node test entry point. The shared CI owner wires the
command into the hosted Headlamp job. It requires the actual installed patched
checker and merger and asserts that Storybook resolves that same checker. There
is no archive-mode fallback, mock checker, skipped prerequisite, or install step.

The test creates a tiny strict TypeScript project under the plugin directory and
runs three sequential fresh Webpack compilers using Storybook's installed Webpack,
real `ts-loader` and the checker's real TypeScript workers:

1. A valid numeric assignment must produce a bundle with no warnings, errors or
   checker issues.
2. A string assigned to that number must fail compilation. The checker worker
   must report **TS2322** against the exact `index.ts` fixture, and that same
   diagnostic must appear in Webpack's compilation errors. A loader-only error
   cannot satisfy the assertion.
3. Rewriting the same source to the valid assignment must compile successfully
   again, without stale checker errors.

All checker diagnostic categories are enabled, `async: false` makes compilation
wait for the worker, and the issues hook observes and returns diagnostics
unchanged. Loader type checking is not disabled. Every compilation must invoke
the real checker completion hook, including successful builds. Compiler handles
close in `finally`, and the test removes its fixture and emitted bundles afterward.
An expected invalid *compilation* is asserted inside the test; the overall command
returns zero only when the valid/invalid/recovered sequence passes.

Local syntax validation passed. Execution remains pending on hosted clean
installation: the current local `node_modules` has an unpatched checker and no
`deepmerge-ts`. The prerequisite assertion fails before any fixture, compiler or
worker starts; local compilation and dependency installation were not attempted.

## Required hosted qualification

This is a source/package repair, not completed image or customer acceptance.
The parent-owned hosted pipeline still needs to prove:

1. Clean Node 22 `npm ci` installs the local checker and maintained merger, with
   normal Node tests confirming Storybook resolves that same checker.
2. The real plugin build and component suite pass. Run the Headlamp CLI TypeScript
   check, and a real Webpack/Storybook build with `typescript.check: true`; include
   a deliberately invalid TypeScript input and require a reported diagnostic/nonzero
   result so an accidentally bypassed checker cannot pass qualification.
3. All-dev/all-severity lock/filesystem/image scans inventory the installed npm
   graph, show no `deepmerge`, and report no replacement advisories. Inspect the
   local-file checker and its real dependency, not only image runtime layers.
4. The actual Headlamp image builds and the existing browser/customer journeys
   remain functional. No local image builds, containers, full installs or compiler
   builds were used to claim these gates.
