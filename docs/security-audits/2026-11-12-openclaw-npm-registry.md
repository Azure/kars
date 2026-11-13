<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit — OpenClaw canonical npm registry locks (beta)

Date: 2026-11-12
PR: https://github.com/Azure/kars/pull/622
Reviewed implementation source: `28ca292c01bbd4310af90563fc2496281fce7141`
Reviewed integration base: `af5ff64796898060f8f9d4a9dc853013f1d51981`
Scope: `mesh-plugin/package-lock.json`, `runtimes/openclaw/package-lock.json`, `runtimes/tests/test_openclaw_npm_sources.py`, `runtimes/openclaw/README.md`.
Gated paths: `mesh-plugin/package-lock.json`, `runtimes/openclaw/package-lock.json`.

## Summary

Beta.5 run [43221049337](https://github.com/Azure/kars/actions/runs/43221049337) failed in both native OpenClaw builds during cli-builder `npm ci` in `/build/mesh-plugin`: npm refused a mirror-hosted Vitest archive with `EALLOWREMOTE`. The run ended with 50 successful jobs, two failed builds and three skipped jobs. The previously repaired Node SDK byte comparison passed on both architectures and remains unchanged.

The base's npm 12.2.0 defaults to `allow-remote=none`. The two locks recorded public 1ES mirror URLs while the release used the canonical npm registry, so npm classified those tarballs as unrelated remote sources. The repair replaces only the exact `https://ms-feed-25.pkgs.visualstudio.com/1es-public/_packaging/npm-public/npm/registry/` prefix with `https://registry.npmjs.org/`: 66 resolved URLs in the shared mesh lock and 65 in the OpenClaw runtime lock. Canonical URLs reuse npm's maintained configured-registry substitution, including build-secret registries; no build-time rewrite or remote-source exception is added.

## T1: New capability / attack surface? (NO)

- No runtime feature, endpoint, privilege, credential flow or dependency version is added. OpenClaw 2026.9.8, Node 24, Hermes and AL3 are unchanged.
- Structural comparisons establish URL-only lock deltas. Versions, integrity hashes (including existing SHA1 entries), dependency graphs and vendored AGT `file:` references are unchanged.
- A focused Python suite tests both actual adapter locks and real npm installation behavior with isolated loopback package fixtures. Documentation explains the canonical-source convention.

## T2: Security-control change? (SOURCE RESTRICTIONS PRESERVED; SCOPED MAINTAINER EXCEPTION)

- `allow-remote=none`, `ignore-scripts`, integrity verification, canonical vendored SDK SHA256 and staged-byte checks, and immutable image packaging remain in force. Dockerfiles, manifests, base-image pins and workflows are unchanged.
- Tests demonstrate that canonical scoped and unscoped packages install through the configured registry, whereas the original mirror URL, unrelated remote tarballs, explicit remote dependencies and same-origin URLs outside the configured registry path are rejected. Mismatched integrity fails; root and dependency lifecycle scripts do not execute; lock bytes remain unchanged.
- The approval follow-up adds only these two exact lockfile paths to the existing capability gate, which previously omitted them. PR622 uses the ordinary new-record requirement, not PR621's workflow-only voluntary-record handling. The dedicated exception binds this record to the exact implementation source/base, signer, ancestry and PR/integration context. All six preceding approval cases remain unchanged.
- This is the user's expressly authorized maintainer sign-off, not an independent human review or independent security audit. It replaces only the second-human sign-off requirement for this immutable scope. Required technical checks and scans remain mandatory.

## T3: Availability / fail-open risk? (REDUCED)

- The install failure is repaired without permitting arbitrary remote package fetching or disabling lifecycle or integrity safeguards. Invalid sources and bytes remain rejected.
- Existing failed attempts remain evidence. Loopback tests do not establish full production dependency fetching, architecture-native builds, a completed signed release, working install commands or fresh independent Kind acceptance.
- The local npm behavior tests used upstream npm/cli v12.2.0 source and its bundled dependencies after registry access failed. Local Node 24.9.0 emitted npm12's unsupported-engine warning; all five tests ran successfully. Release Node 24.21.0 is unchanged. No claim is made that the local host matches the release builders.

## Verification

- `KARS_NPM_CLI=<isolated npm/cli v12.2.0>/bin/npm-cli.js PYTHONDONTWRITEBYTECODE=1 python3 -m unittest runtimes.tests.test_openclaw_npm_sources -v` — passed: five tests locally and from prepared implementation source; independently rerun by the parent with all five passing. The canonical-lock check fails against both original locks as expected.
- `cd cli && TMPDIR=<task-owned scratch directory> npm test -- src/testing/sandbox-hardening.test.ts -t 'sandbox Dockerfile — pinned mesh packaging'` — passed: ten tests; eleven unrelated tests excluded. The parent independently reran the same selected ten tests successfully.
- From prepared approval source: `TMPDIR=<task-owned scratch directory> PYTHONPATH=<checkout>/ci/tests PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p security_audit_gate_test.py -v` — passed: 176 tests across all seven immutable approval scopes, including cross-record/PR rejection, lock-only gating, both lockfile drift cases, source/base/ancestry checks and PR621 workflow behavior. Git fixtures were isolated from the checkout.
- The two new lock-only approval tests run against the unmodified published gate — failed as expected: both original gate calls incorrectly returned success without an audit record.
- `bash -n ci/security-audit-required.sh` — passed on prepared source.
- Per-path `git diff --no-index --check -- <published-source-file> <prepared-source-file>` (using `/dev/null` for this new record) — passed for all four approval paths.
- No full build, dependency install, container, cluster or provider execution was performed for this approval follow-up. Hosted technical checks, scans, native image rebuilds, release signing/source validation and independent fresh-Kind customer acceptance remain required.

## Verdict

Accept this exact-source repair under the recorded maintainer decision, subject to all required technical checks. At `2026-11-12T16:02:42.061459+00:00`, the user selected **“Publish, sign off, merge after checks, and release; allow the temporary review-only exception if needed”** for the four-file repair and its necessary source-bound approval plumbing. Authorized implementation patch SHA256: `f255cf80b9de22781732cbac062d065b3188734e463f3e25ffa0fb32a6991e6c`.

The approval-only follow-up may change only this record, `ci/security-audit-required.sh`, `ci/tests/security_audit_gate_test.py` and `docs/security-audits/README.md`. The existing gate enforces exact base, approved-source ancestry, no implementation drift and the Azure/kars PR622 merge or kars-bridge integration context. No preceding PR's approval is repointed, edited or reused.

If normal merging refuses for review requirements, the user's permission allows the publisher a temporary review-only exception for this PR, with full protection restored immediately. It does not waive any required technical check, scan, signature or source verification. This record does not itself change branch protection, publish, merge or release anything. The single-maintainer exception does not establish independent security review or beta readiness; fresh independent Kind acceptance remains outstanding.

Beta-approval-exception: Azure/kars#622; source=28ca292c01bbd4310af90563fc2496281fce7141; base=af5ff64796898060f8f9d4a9dc853013f1d51981
Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
