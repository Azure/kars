<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit — Interactive local Kind onboarding

Date: 2026-11-10
PR: https://github.com/Azure/kars/pull/607
Reviewed implementation source: `a4333b355a03648cf3769faf4eca26e988700aeb`
Reviewed integration base: `97789571866ca2c181f84455a845b957427cd991`
Scope: the full PR diff, including CLI onboarding and lifecycle helpers, packaged asset resolution, local Dex and network-policy Helm templates, Kind funding/Headlamp templates, dependency manifests, native admission qualification, the materialized-Task credential refresh repair, and their tests/documentation.
Gated paths: `cli/src/commands/dev.ts`, `cli/src/commands/dev/local-k8s.ts`, `cli/src/commands/dev/kind-beta*.ts` production modules, `bridge/deploy/helm/kars-bridge/templates/idp.yaml`, `bridge/deploy/helm/kars-bridge/templates/networkpolicy.yaml`. Additional security-control scope: `controller/src/credential_grants/sources.rs` and `controller/src/credential_grants/sources/bundle.rs`.

## Summary

Bare `kars dev --release --target local-k8s --beta-config` adds an interactive full Core+Bridge installation on a new managed Kind cluster, with fresh local Dex credentials and explicit inference configuration. Explicit-file external OIDC and no-flag standalone Core paths remain supported. This is new capability scope; neither the PR593 nor PR606 source-bound approval covers it.

The subsequent six-file repair reuses descriptor-bound private-file reads for the local ignore file, stores fixed UI failure categories instead of network-derived diagnostics, makes test RBAC matching exact, and observes admission-grant propagation using dry-run requests before unchanged single-shot forbidden-write assertions. No admission policy was modified.

The latest six-file controller repair addresses ordinary Task execution-status updates being treated as lost credential authority during private enrollment. It reuses the existing materialized-Task transition and live-caller checks for the final bundle target read, rather than relaxing the private-enrollment template guard or ignoring projection errors. Only resourceVersion/managed-fields and already-supported execution progress can differ; the exact bundle anchor, metadata, generation, spec, governance status, grant and source revisions remain protected. Arbitrary Task, Team and bare Sandbox callers retain exact target revision checks.

## T1: New capability / attack surface? (YES)

- New local identity provider, private identity material, loopback endpoints, API-server OIDC integration, DNS mappings and optional host CA trust. Host-trust consent defaults to no and is explicit/reversible; this remains a meaningful host trust decision, not an invisible prerequisite.
- Local email identity maps to the configured Bridge administrator. Headlamp uses a separate confidential OIDC client and read-only Kubernetes roles without Secret access, writes or wildcards. Separate clients do not establish browser-session SSO.
- Docker/Podman control is privileged relative to the local machine. Supported endpoints are pinned and ambient runtime contexts cleared; fresh cluster ownership and immutable node identity constrain lifecycle operations.
- Adds bcrypt for local password hashes. Lockfile integrity was checked; passwords have a 12-character minimum and a 72-byte UTF-8 maximum, with bcrypt cost 12.

## T2: Security-control change? (YES)

- Local Dex uses maintained restricted-PSA Helm resources, owned private storage, TLS, namespace-scoped caller policies and deny-all egress. Exact gateway/32 rules, hostname rewrite and API-server mapping support the local route without a generic network-policy exception.
- Separate OIDC and budget TLS identities are issued; CA private keys are removed after issuance. Certificates are hostname-constrained and expire after 365 days. No TLS verification bypass was introduced.
- Lifecycle receipts use private directories/files, bounded descriptor-based reads, fresh ownership and immutable node identity. Cleanup refuses foreign resources and active setup. Existing unsafe ignore files fail before cluster operations.
- Release setup requires packaged assets rather than silently sourcing the dirty checkout; image and chart bindings remain required.
- An empty price catalogue is allowed only for the existing exact Kind beta record-only/unbounded-token policy. Actual usage and settlement remain recorded; production finite-policy enforcement is not disabled. This is not a provider-capacity guarantee.
- Native readiness uses dry-run PATCH only. Intended policy denial is required within 30 seconds; unrelated denials and unexpected errors do not qualify. Actual forbidden writes remain single-shot and must return 422; Secret invariance assertions remain.

## T3: Availability / fail-open risk? (INCREASED)

- A fresh local IdP adds startup, certificate, DNS, API-server and NodePort dependencies. Failures may leave owned recoverable setup state; successful unit tests do not prove the complete lifecycle on a user's host.
- Host trust and cluster cleanup are ownership-sensitive operations. Explicit consent, private receipts and exact node matching reduce accidental scope, but installed macOS/Podman/VM behavior still requires fresh-system acceptance.
- Unbounded non-expiring Kind token allowance deliberately removes a Kars spending ceiling for beta workflows. Provider limits, real accounting, tenant/network controls and host limits remain; provider expenditure is not capped by this beta policy.
- Missing admission enforcement fails qualification rather than being converted to a pass. Current successful cold-install-2 evidence needed no retry (zero allowed dry-runs); it does not prove the exact cause of the earlier uninstrumented failure.

## Verification

Latest controller repair, from repository root, using `CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`:
- `cargo test --locked --package kars-controller credential_grants::sources` — passed, 34 tests. The new regression first failed on the published implementation with `Credential target changed before bundle write`, then passed with the repair. Tests reject changed Task/consumer authority, bundle anchors, grants and sources, and preserve exact revision checks for callers without a verified materialized Task consumer.
- `cargo test --locked --package kars-controller reconciler::credential_source` — passed, 28 tests, including real authority-loss pause/revocation and cross-runtime projection behavior.
- `cargo clippy --locked --package kars-controller --all-targets -- -D warnings` — failed with nine `nonminimal_bool` diagnostics on local Rust 1.95.0, all in unchanged files byte-identical to published head `d4bef7f38905b2a7669ff699ae95ae6b589f612d`. No suppression or unrelated rewrite was applied.
- Broader `cargo test --locked --package kars-controller credential_` attempt was stopped while the unrelated `observer_api_discovery_errors_and_malformed_contracts_never_mean_absent` test remained running. It reported 172 passes and no failures before stopping; this is not a full-suite pass. The two focused suites above completed on the final repair.
- Scoped `git diff --check` — passed.

Previous onboarding repair, from `cli/`:
- `npm test -- src/commands/dev/kind-beta-local.test.ts src/commands/dev/kind-beta.test.ts src/testing/helm-headlamp.test.ts src/testing/helm-inference-budget-empty-catalog.test.ts` — passed, 238 tests.
- `npm run typecheck` — passed.
- `npx --no-install oxlint src/commands/dev/kind-beta.ts src/commands/dev/kind-beta.test.ts src/commands/dev/kind-beta-local.test.ts` — passed, zero warnings/errors.

From repository root:
- `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s bridge/tests/native-credentials -p 'test_*.py'` — passed, 273 tests, including six new admission-observation regressions.

Hosted CI evidence:
- All three native cold-install API/admission checks passed at implementation source `650141e5` in run `42816406025`. Cold-install-2 artifact records all four case groups passed and actual forbidden-key/wrong-UID writes rejected with 422. That prerequisite artifact explicitly does not qualify runtime, NetworkPolicy enforcement, bearer issuance or combined active SRE behavior.
- JavaScript/TypeScript CodeQL analysis `2134971454` reports zero results on synthetic merge `9ba968239ebfb104db4dce6c0ab010a145d29f8e`. The three previous alerts are marked fixed on this PR, not dismissed. The prior alert-instance commit remains the old detection commit and must not be confused with the new analysis commit.
- At renewed approval preparation, published head `d4bef7f38905b2a7669ff699ae95ae6b589f612d` had 57 passing and two failing checks. Native runtime run `42819186255` failed `private-bff-observer-and-fresh-privacy-rpc` with `consumer-template-drift` during Restoring; its combined acceptance gate also failed. The retained artifact reports `target_changed` credential degradation and changed projection version with unchanged runtime executable settings and Deployment UID. The local regression demonstrates a concrete triggering bug, not a complete trace of the failed hosted read. Repaired hosted acceptance has not yet run. No all-green claim is made.

Earlier initial-publication build/typecheck and 419 focused tests passed; historical Helm upgrade regression repair passed 75 tests. Broad local `make test` and `make lint` were not run. Direct implementation review is not independent human review; a previous independent-review attempt was interrupted and returned no result.

Retained failures: initial wrong-working-directory npm invocation, historical Helm upgrade failures, prior native cold-install-2 unexpected successful PATCH, three CodeQL findings, and failed beta.1 release. The two failed-release images are anonymously readable but their signature manifests are absent; a successful sibling has a signature manifest. This does not establish the cause of GHCR write_package denials. Signing remains mandatory.

No local Docker restoration, containers, fresh cluster, inference requests or host trust changes were executed. Live local-Dex startup, complete login/denials, networking, downloaded signed-release installation and the user's independent fresh-cluster journey remain unqualified.

## Approval-plumbing verification

- `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p security_audit_gate_test.py` — passed, 72 tests across all three immutable approval scopes, including cross-record and cross-PR isolation.
- `bash -n ci/security-audit-required.sh` — passed.
- Scoped `git diff --check` for the gate, tests and audit README — passed.
- The first expanded cross-record test attempt failed three cases because it incorrectly rebuilt the approved fixture repeatedly in one repository. The corrected test keeps one approved source and independently adds/removes each foreign record; all 72 tests pass. This was a test-fixture repair, not a relaxation of the gate.

## Verdict

Approved by the maintainer for this exact reviewed source and integration base under the explicitly renewed single-maintainer exception. The user selected **“Authorize this repair and exact-source sign-off (Recommended)”** for the six-file patch `cdd252c75f18a4655ae25bb9898423f6e1a890d6b022e5d1181c76d25eb64201`, after disclosure of the 62 focused passing tests, unchanged-code local lint diagnostics and required hosted rerun. That patch is implementation commit `a4333b355a03648cf3769faf4eca26e988700aeb`, with parent `d4bef7f38905b2a7669ff699ae95ae6b589f612d` and unchanged integration base `97789571866ca2c181f84455a845b957427cd991`. The earlier `650141e5ad4bdf68a1c02e7a0ecae202fde10b3b` approval alone did not cover this repair. The existing genuine signer identity is retained.

This replaces only the two-human sign-off requirement for this scope. No independent second human review is claimed. The earlier PR593 and PR606 approvals are neither extended nor repointed. Technical CI, mandatory artifact signatures and independent fresh-cluster acceptance remain required. This is not a merge exception, branch-protection change, signing waiver or customer-acceptance result.

The approval follow-up may change only this audit record, `ci/security-audit-required.sh`, `ci/tests/security_audit_gate_test.py` and `docs/security-audits/README.md`. Every other tracked path must match the approved implementation source, with its exact base and ancestry retained.

Beta-approval-exception: Azure/kars#607; source=a4333b355a03648cf3769faf4eca26e988700aeb; base=97789571866ca2c181f84455a845b957427cd991
Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
