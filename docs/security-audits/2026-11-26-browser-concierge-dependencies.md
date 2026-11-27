<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit - Concierge dependency successor (Azure/kars#638)

Date: 2026-11-26
Source: `6b6831148955e3d3a28dda0c4ac229d48fef3e9e`
Base: `754110953bf9cccbb7545a71782c38a59087ea2f`
PR: <https://github.com/Azure/kars/pull/638>

## Scope

The maintainer explicitly selected **"Include minimal Azure dependency repairs"**
after the concierge-only CI repairs passed and the remaining Bridge and
Headlamp dependency failures were confirmed. This source retains the concierge
implementation and its original immutable review record. It adds only the
dependency repair, compatibility test and directly related documentation:

- Bridge BFF: kube/client/core/derive/runtime 4.2.1, quinn-proto 0.11.19,
  quinn-udp 0.5.16 and url 2.5.9, with their required transitive changes.
  kube's upstream minimum requires Rust 1.89 instead of 1.88; the existing
  Kubernetes schema version and rustls feature selection are retained.
- Headlamp: i18next 26.3.4 in the production frontend and plugin tooling;
  OpenTelemetry SDK and OTLP exporters aligned at 1.47.0.
  The bundle evidence must include the patched i18next package.
- The Headlamp source/version, Go builder, AL3 distroless base, runtime UID
  and immutable upstream plugin input remain unchanged.

No private-repository repairs are imported. Chrono removal, general package
refreshes, scan exclusions, severity changes and new runtime installation
paths are outside scope. The approval-only successor changes only its own
record and existing gate/test/README plumbing.

## T1: New attack surface? (NO)

No new route, service, listener, credential store or installation capability
is added beyond the separately recorded concierge implementation. Dependency
code changes remain supply-chain inputs and require fresh build qualification.
The plugin lock retains a canonical public tarball URL and SHA-512 integrity
computed by npm from the verified archive, not a machine-specific mirror URL.

## T2: Security-control change? (YES)

The package updates address the scanner findings retained in the first record:
RUSTSEC-2026-0600, RUSTSEC-2026-0524, RUSTSEC-2026-0523,
RUSTSEC-2026-0538, RUSTSEC-2026-0592, CVE-2026-63402 and
CVE-2026-128085. Cluster trust and authorization are not relaxed. The kube
update introduces its maintained platform-verifier dependencies, so fresh
cluster TLS/auth validation remains necessary.

All-severity Headlamp image, frontend lock/inventory and plugin lock scans
remain enabled. Existing source hashes, output evidence and read-only AL3
packaging are preserved. No advisory is suppressed by this repair.

## T3: Availability or fail-open risk? (YES)

Moving i18next across major versions and kube within 4.x can expose
compatibility regressions. The retained React adapter's peer range accepts
the patched i18next; a real adapter test checks singular/plural translation,
language changes, interpolation and fallback. Hosted image compilation,
upstream backend tests and Bridge's unchanged TLS/auth/cluster gates remain
required. Successful local checks are not installed customer acceptance.

## Verification at the source

From the repository root:

- `cargo audit --file bridge/bff/Cargo.lock`: **passed**, zero vulnerabilities;
  the existing allowed Chrono unmaintained warning remains visible.
- `cargo fmt --manifest-path bridge/bff/Cargo.toml --all -- --check`: **passed**.
- `node --test sandbox-images/headlamp/build-frontend.test.mjs`: **passed**,
  10 tests, no skips, including the real React/i18next compatibility check.
- `npm test --prefix tools/headlamp-plugin`: **passed**, 25 build-tool checks
  and 84 component tests in five suites.
- `npm run build --prefix tools/headlamp-plugin`: **passed**.
- `bash sandbox-images/headlamp/test-static-cache.sh`: **passed**.
- `sh -n sandbox-images/headlamp/build-backend.sh`: **passed**.
- `python3 -B -m unittest discover -s ci/tests -p headlamp_build_evidence_test.py`:
  **passed**, 15 tests.
- `trivy fs --scanners vuln --include-dev-deps --skip-dirs tools/headlamp-plugin/node_modules --exit-code 1 --format json --output <evidence>/patched-plugin-lock-scan.json tools/headlamp-plugin`:
  **passed**, npm lock inventory present and zero findings across all severities.
  The successful scan used an empty temporary `DOCKER_CONFIG` for anonymous
  public database access because the local Desktop credential helper was absent.
- `git diff --check`: **passed**.

The default local npm mirror returned a missing-tarball error; direct npm
registry access also failed. Installation from the approved feed advertised
by package metadata succeeded with integrity checks enabled. Those environment
failures do not count as passing validation.

Fresh hosted compilation, all image scans and cluster qualification were
pending when this record was authorized. No local image build, deployment or
installed acceptance was performed. Beta acceptance remains 0/2.
The earlier automated concierge security review is not a second human review
or a new independent review of this dependency successor.

After authorization, hosted run 45843600780 failed the CLI Headlamp manifest
fixture (the expected dependency map omitted the new i18next pin) and two
upstream Headlamp tracing tests (the middleware span was named `GET`, not
`test-service`). Its separate, unchanged root Rust lock also failed its audits.
Bridge's updated dependency audit passed in run 45843600716. These failures
remain unresolved at this source and are not waived.

The approval-only gate regression command
`PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p security_audit_gate_test.py`
**passed**, 364 tests, including historical-record immutability and rejection
of post-approval dependency/recipe changes.

## Verdict and authorization

Accept the exact source under the maintainer-authorized second-human exception,
not as a declaration of merge readiness or beta readiness.

The maintainer was asked:

> May I record your sole-maintainer security sign-off for updated PR638 source
> 6b6831148955e3d3a28dda0c4ac229d48fef3e9e against Azure base 75411095,
> now including the Bridge and Headlamp dependency repairs? Local audits and
> compatibility checks pass; hosted build/image/TLS checks remain required.
> Your previous source-bound approval will stay unchanged. This waives only
> the second-human requirement--not technical gates or merge approval.

The maintainer selected **"Authorize updated source-bound sign-off for PR638"**.
Pal Lakatos-Toth is the sole human approver; Copilot implemented the repair.
The previous concierge record remains immutable. Missing or altered source,
base, ancestry, PR context, historical record or successor authorization fails
the existing gate. Further implementation changes invalidate this approval.

Beta-approval-exception: Azure/kars#638; source=6b6831148955e3d3a28dda0c4ac229d48fef3e9e; base=754110953bf9cccbb7545a71782c38a59087ea2f

Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
