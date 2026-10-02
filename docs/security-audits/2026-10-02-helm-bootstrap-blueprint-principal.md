<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit — Helm bootstrap and typed blueprint principals (beta preparation)

Date: 2026-10-02
Scope: the principal validation and opt-in schema bootstrap in
[Azure/kars#573](https://github.com/Azure/kars/pull/573), including the helper,
chart, network policies, packaging, image recipe, tests and documentation,
plus the Bridge dependency and BFF lint repairs described below.
Original gated paths: `cli/src/commands/mesh/agent_id_setup.ts`,
`deploy/helm/kars/files/schema-hook.mjs`,
`deploy/helm/kars/files/schema-hook.NOTICE`.

Source base: `3a6c81564cf2a8a5a201b0cbe63e000738d70133` (`kars-bridge`).
Source head for requested review: `301dea274cbf6f89ce9dbef2f1316f2672ec4e22`
(principal commit `7e5a6255bb8a60eae92dd576df6df68be6130cee`, bootstrap commit
`8a1ef14708550af9f09e65403be03b4bebfa101d`, bundle reproducibility/license
repair `b3e4d531`, and Bridge dependency/BFF lint repair `301dea27`). This is the
exact executable source range described here; the subsequent commit updating
this record is documentation-only. Later executable changes require renewed
review. Unpublished Helm/runtime identity integration is excluded.

**Review status:** prepared by GitHub Copilot for human review. Neither an author
security sign-off nor an independent human review has been obtained. This is not
a completed two-person audit, maintainer delegation, or permission to bypass CI.

## Summary

Principal setup now validates the blueprint application and principal identity,
uses typed Graph routes, and refuses ambiguous or incompatible responses instead
of accepting an arbitrary ordinary service principal.

The disabled-by-default Core `schemaHook` stages CRDs from the exact stored
pending Helm release. It removes the separate customer schema command for the
supported original install. The chart packages the executable in an immutable
ConfigMap and uses a digest-pinned toolchain image. It does not install Bridge,
activate Entra identity, or qualify an agent journey.

## T1: New capability / attack surface? (YES)

- Enabling the hook adds eight pre-install resources: a ServiceAccount,
  ClusterRole/Binding, Role/Binding, immutable program ConfigMap, NetworkPolicy
  and Job. These are privileged installation resources, not sandbox authority.
- CRD `create` permission is cluster-wide: Kubernetes RBAC cannot restrict create
  by resource name. The program restricts writes to CRD specifications in the
  pinned pending manifest. This is an application control, not an RBAC boundary
  against a compromised image or executable. CRD `get/patch` is name-restricted
  to the chart's 21-CRD inventory.
- Helm history needs Secret `get/list` throughout the release namespace. The
  helper can therefore read unrelated secrets placed there. A dedicated Core
  namespace and trusted installation inputs are requirements, not optional
  mitigations. The helper also reads discovery, admission-policy information,
  its namespace and the named controller Deployment.
- The chart, bundled program, selected image digest and API destination values
  are trusted installer inputs. Digest syntax does not authenticate a publisher
  or prove that an image has been qualified. No published qualified helper image
  is provided by this change.
- The entrypoint uses only projected namespace/token/CA files and an explicit
  temporary kubeconfig. It invokes Helm/kubectl with `execFile`, a reduced
  environment, bounded output and timeouts, rather than a shell. Normal failure
  output deliberately omits manifests, subprocess output and credentials.

## T2: Security-control change? (YES)

- Graph lookup escapes the OData filter, requests up to two entries, and rejects
  multiple results or pagination. Returned app IDs and any expected object ID
  must match. Explicit incompatible OData types are rejected. Missing type
  metadata requires an exact-object typed read; creation uses the typed
  BlueprintPrincipal endpoint. There is no ordinary-principal fallback added by
  this change, and no tenant policy or consent bypass.
- The chart independently rejects simultaneous Entra-sidecar activation. It
  permits only original revision-one installs with the inspected outer Helm
  4.1.3, Kubernetes 1.35, a reviewed image digest, explicit IPv4 API destinations,
  and the policy prototype disabled. Broader library upgrade checks do not make
  chart upgrades or replacement installs supported.
- The coordinator pins release history and manifest digests, checks controller
  safety before writes, and repeats discovery/schema checks before success. It
  does not rerender the release, create customer custom resources, force
  ownership, reconstruct private authority, or issue Helm lifecycle commands.
  These rechecks are not an atomic transaction with all Kubernetes writers;
  release storage, RBAC and concurrent installer access remain trusted controls.
- Namespace-wide deny is separated from the existing operator allow policy.
  The operator selector excludes `schema-hook`, preventing the helper from
  inheriting chart-provided DNS, mesh and IMDS allowances. The helper policy
  denies ingress and allows only configured API `/32` address/port pairs.
  Other customer policies remain additive; effective CNI enforcement and
  pre/post-DNAT destinations still need installed qualification.
- The Job runs nonroot with RuntimeDefault seccomp, a read-only root filesystem,
  no privilege escalation, dropped capabilities, bounded temporary storage and
  CPU/memory limits. Its projected token requests a 600-second lifetime; the
  actual cluster token policy and retained RBAC still matter.

## T3: Availability / fail-open risk? (INCREASED)

- Opting in adds an installation dependency on exact versions, API routing,
  discovery/admission readiness, release-history visibility and the helper
  image. Unsupported inputs are refused instead of silently broadening access.
- The helper defaults to a 120-second budget; subprocesses are bounded by the
  remaining time and output limit. The Job has a 180-second active deadline and
  no restart/backoff retry. These reduce runaway execution but do not establish
  native behavior until the image and cluster path have been exercised.
- An install can fail after some CRDs have been published. Failure does not
  restore the former schema or authorize force, automatic write retries,
  uninstall, release replacement, or deletion of retained customer data.
- Each attempt has fresh helper names. Job, credentials, RBAC, program and
  policy are retained on success and failure. This preserves evidence and
  isolation while a helper may still be running, but also retains privilege.
  An operator must verify termination before retiring that exact attempt's
  resources. A failed Helm watch alone is not proof of termination, and release
  uninstall does not automatically remove hook resources.

## Verification

### Repaired bundle and scoped chart

The first PR CI run found stale generated code and missing copyright coverage.
Local dependencies contained YAML 2.9.0 while the committed lockfile specifies
2.8.3. A locally repeatable build with the former did not prove lockfile/CI
reproducibility. The repair rejects mismatched or absent YAML lock metadata before
writing output, regenerates against 2.8.3, adds source headers after minification,
and records the upstream NOTICE as legal attribution rather than Microsoft-owned
source. No dependency manifest, lockfile or security gate was weakened.

The following checks passed in a disposable source export containing the published
CLI/chart plus exactly the repair changes, excluding unfinished identity work.
Dependencies were installed there, not in the shared development installation.
Commands are from `cli/` unless stated otherwise:

- `npm ci --ignore-scripts --no-audit --no-fund` — passed with the committed
  lockfile. Lifecycle scripts were disabled; this was not a dependency audit.
- `./node_modules/.bin/tsc` — passed; compiled the CLI, including the executable
  entrypoint required by the subprocess tests.
- `npm run build:schema-hook` — passed with locked YAML 2.8.3.
- `npm run check:schema-hook` — passed against that generated program and NOTICE.
- `npm test -- src/lib/core-helm-schema-hook.test.ts src/schema-hook.test.ts src/schema-hook-executor.test.ts src/schema-hook-process.test.ts src/schema-hook-image.test.ts src/lib/schema-hook-bundle.test.ts src/testing/schema-hook-chart.test.ts --reporter=json --outputFile=<temporary-results.json>`
  — passed, 122 tests in seven files, zero failures. Coverage includes generated
  program execution, refusal paths, dependency drift, header preservation and
  chart policy/rendering. These are not installed cluster acceptance tests.
- `./node_modules/.bin/oxlint scripts/bundle-schema-hook.mjs src/lib/schema-hook-bundle.test.ts src/testing/schema-hook-chart.test.ts`
  — passed with zero warnings/errors.
- From the export root,
  `python3 -m unittest discover -s ci/tests -p copyright_headers_test.py`
  — passed, 34 tests. The same suite in the mixed development tree had failed on
  an unpublished AuthConfig template; that excluded work was not changed to hide
  the failure.
- From the export root,
  `python3 ci/copyright_headers.py check ci/copyright-coverage.json cli/scripts/bundle-schema-hook.mjs cli/src/lib/schema-hook-bundle.test.ts cli/src/testing/schema-hook-chart.test.ts cli/schema-hook/Dockerfile.dockerignore cli/schema-hook/README.md deploy/helm/kars/templates/schema-hook.yaml deploy/helm/kars/files/schema-hook.mjs deploy/helm/kars/files/schema-hook.NOTICE`
  — passed: nine paths covered, no missing headers or errors.
- From the export root, `helm lint deploy/helm/kars` — passed.
- From the export root, `helm package deploy/helm/kars --destination <temporary-directory>`
  — passed.
- From the development repository, `git diff --cached --check` — passed before
  committing the eight-file repair.

Only after these checks passed were the generated program and NOTICE copied back.
The disposable source, dependency installation and package were removed. The
shared development installation remains unchanged; its `check:schema-hook` now
correctly refuses YAML 2.9.0. Local generation/tests used Node 24.9.0 with the
configured Node 22 output target. On published head `c3086df1`, the
[public CLI Build & Test job](https://github.com/Azure/kars/actions/runs/36996841261/job/110805477244)
passed on Node 22, including locked installation, typecheck, lint, the committed
bundle freshness check, build, tests and CLI dependency audit.

### Focused Bridge CI repair

Commit `301dea27` changes only the web manifest/lockfile, gateway lockfile and
two BFF Clippy expressions:

- Next.js, its platform packages and ESLint configuration: 16.3.6.
- DOMPurify: 3.4.16; brace-expansion: 1.1.21 and 5.0.12.
- Axios: 1.20.0; ip-address: 10.7.2, within existing gateway parent ranges.
  The Teams SDK and gateway manifest are unchanged.
- Both BFF policy-readiness checks pass the same closure without a needless
  borrow. Their generation, provider, deployment and Ready predicates are
  unchanged; no authorization check is removed.

npm generated both lockfiles. Existing versions retain their previous registry
URLs and integrity metadata; the published baseline already uses both npmjs and
Microsoft public-mirror URLs. This is not a registry migration. No security gate,
severity threshold or CI command was weakened.

Checks below passed on Node 24.9.0. Install/build/test checks ran in a disposable
export of committed source with only the three changed dependency files overlaid,
excluding unfinished identity work and shared dependencies. Both lockfiles stayed
byte-identical throughout installation and checks; the disposable tree was removed.

- From the repository root,
  `npm --prefix bridge/web audit --package-lock-only --ignore-scripts --audit-level=high`
  and
  `npm --prefix bridge/teams-gateway audit --package-lock-only --ignore-scripts --audit-level=high`
  — each passed with zero vulnerabilities through the configured npm mirror.
- In each component,
  `npm ci --ignore-scripts --no-audit --no-fund --fetch-retries=1 --fetch-timeout=30000`
  — passed. Lifecycle scripts were disabled.
- In `bridge/web`, `npm run lint` — passed with zero errors and eight existing
  warnings in unchanged application files; `./node_modules/.bin/tsc --noEmit`
  — passed; `node --experimental-strip-types --test tests/*.test.mjs`
  — passed, 94 tests across nine files, including proxy routing and attribution.
- In `bridge/teams-gateway`, `npm run lint` — passed without warnings;
  `npm run build` — passed;
  `npm test -- tests/gateway.test.ts tests/kubernetes.test.ts` — passed, 31 tests.
  The bounded fake API server and Helm rendering are not native installation proof.
- `git diff --cached --check` — passed before the four-file commit.

The exact `ci/npm-audit-bulk.mjs` public-npm gate could not obtain advisory data
locally: its bounded requests failed with `ENOTCONN`, including a retry using
Node's supported environment-proxy flag. This is a transport failure, not a
passing bulk audit. Direct registry regeneration also failed and did not replace
source. Existing public CI must validate the new source's exact bulk audits,
Linux/Node 22 build and image checks, and BFF Clippy. No local Rust compilation,
image build, native cluster execution or full gateway suite was run for this repair.

### Earlier evidence and remaining qualification

Before CI exposed the dependency mismatch, `npm run build`, `npm run typecheck`,
focused lint and the eight-file selector set including
`src/commands/mesh/agent_id_setup_principal.test.ts` passed (203 tests). Those
results remain historical source-test evidence, not qualification of the repaired
locked bundle. The principal source was not changed in the repair. The latest
scoped run above did not repeat full deployment-asset packaging.

Not performed for this source: image build/runtime qualification, native Helm
install/cleanup lifecycle, effective CNI enforcement, Kind/AKS acceptance, Rust
runtime parity, tenant provisioning or real Entra token issuance. Full `make test`
and `make lint` were not run. Frozen or earlier worker evidence does not qualify
this source.

The initial PR CI head `8a1ef147` also failed:

- `security-audit-required`: no new signed audit record. This unsigned record
  does not satisfy the required two-person approval.
- Web dependency audit and the Web build/lint job's runtime audit step: five
  blocking web findings, including a critical Next.js advisory.
- Teams-gateway dependency audit and the add-on job's runtime audit step: seven
  blocking gateway findings involving Axios.
- BFF Clippy: `needless_borrows_for_generic_args` at
  `bridge/bff/src/kars/cluster/orchestrator.rs:248` and `:301`.

The focused repair now updates those dependency inputs and the two BFF expressions.
Configured-registry audits are clean, but the new public bulk-audit, BFF and image
results remain pending. The reports are not dismissed or assessed for
application-specific exploitability here. Consult the live PR for follow-up CI
results; local repair validation does not establish that the PR is green.

## Verdict

**Not approved for release; human source review pending.** The default-disabled
preview has explicit source-level safeguards, but native qualification, unresolved
CI findings and the required human approvals are not complete. This record does
not assert that passing local tests establish a safe installation or beta.

Author sign-off: not obtained. Independent human reviewer sign-off: not obtained.
No `Signed-off-by` identities are recorded on anyone else's behalf. Reviewers must
explicitly approve the stated source range, resolve or disposition the findings,
and add their own sign-offs before this can be treated as the required audit.
