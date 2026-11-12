<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit — Private TLS authority key identifier repair (beta)

Date: 2026-11-12
PR: https://github.com/Azure/kars/pull/623
Branch: `fix/headlamp-evidence-permissions`
Reviewed implementation source: `acb9d9a2ccef87003830fa10c2cc3eb08e5129f9`
Reviewed integration base: `af5ff64796898060f8f9d4a9dc853013f1d51981` (`kars-bridge`)
Parent / prior published PR623 head: `4afec6aee6b85a5c4d5476c55590d77f67fdfc41`
Implementation patch SHA-256: `cc6d5703fd108037792d01ff77138c5e888b4c1fa7b7fea8b994a65d3f8e5166`
Additional implementation scope: `controller/src/providers/sre_tls.rs`, `CONTRIBUTING.md` only.
Retained scope: PR622 npm registry repair and PR623 Headlamp evidence-permission repair, including their unchanged approval records.

## Summary

The private TLS issuer now sets `leaf.use_authority_key_identifier_extension = true`. This adds the issuing CA's key identifier to newly issued leaf certificates so strict clients can validate the existing issuer relationship. The repair reuses the maintained private-CA issuance path; it neither creates another issuer nor relaxes client verification. A real OpenSSL regression covers strict server verification for the issued IP and DNS identities and rejection of a wrong CA or name. The contributor guide documents the OpenSSL prerequisite.

The local baseline reproduced OpenSSL verification code 85, `Missing Authority Key Identifier`. After repair, the exact unchanged Hermes httpx client verified a real local TLS connection using Python 3.14.4 / OpenSSL 3.6.5. This establishes a concrete local defect and its repair, not the root cause of the hosted Kind failure.

## T1: New capability / attack surface? (NO)

- No new endpoint, trust anchor, credential flow, runtime privilege or network access is introduced. The CA remains the issuer; leaf certificates continue to be signed by it.
- Authority Key Identifier identifies the signing key; it is not a substitute for signature, chain, CA trust, server-purpose or hostname/IP verification. Those checks remain enabled. No trust-all SSL context, hostname bypass or fallback to plaintext is added.
- Authentication, tenant isolation, encrypted transport, governed egress, secret handling and host/VM safety limits remain intact. The inherited npm and Headlamp repairs are not broadened.

## T2: Security-control change? (ISSUANCE COMPATIBILITY; EXACT MAINTAINER EXCEPTION)

- The change makes newly issued leaves compatible with strict issuer verification, rather than disabling the strict control. Negative verification tests retain wrong-CA and wrong-identity rejection.
- This ninth source-bound approval case uses the existing exact base, source ancestry, committed marker, genuine signer and Azure/kars PR623/integration-context checks. No capability-pattern change, workflow-only exemption, configurable waiver or additional excluded implementation path is introduced.
- The new approval may carry only the unchanged `2026-11-12-openclaw-npm-registry.md` and `2026-11-12-headlamp-evidence-permissions.md` records, both still newly added against the common integration base. The TLS source comparison includes every historical record. Altering or deleting either carried record invalidates this approval; missing or invalid TLS approval cannot borrow an older sign-off.
- All eight earlier case blocks and immutable records remain unchanged. The prior exact Headlamp approval path still works for its original source. The old Headlamp record is not repointed to TLS, and PR622's signer does not independently approve this successor.
- This is an expressly authorized single-maintainer decision, not independent human review or an independent security audit. No second reviewer or signature is invented.

## T3: Availability / fail-open risk? (LOCAL ISSUANCE DEFECT REPAIRED)

- New certificates contain the AKI required by the tested strict client. Certificates issued before this change are not rewritten automatically and require the existing normal rotation/reissuance process. The fix applies to fresh Kind issuance; successful hosted issuance or migration is not claimed.
- The retained [CI run 43261164174, attempt 3](https://github.com/Azure/kars/actions/runs/43261164174/attempts/3) failed only Kind, with a `ConnectError` at `proxy.py:230`. That error alone does not confirm an AKI cause. All previous failed attempts remain evidence; none is relabeled successful.
- Required hosted CI, shipping-image checks, signed release verification and fresh Kind customer acceptance remain outstanding for this exact TLS source. Local tests do not establish beta readiness or a completed release.

## Verification

Preparation artifacts are retained under `P=/Users/pallakatos/.copilot/session-state/f326f1b4-5650-4dda-857c-ca6b7f848ccf/files/beta5-headlamp-permissions-repair`.

- Read-only GitHub API comparison confirms source `acb9d9a2ccef87003830fa10c2cc3eb08e5129f9` has the prior PR623 head as its sole parent and changes only the two implementation files listed above. The exact three `tls-source-before` approval-plumbing baselines match that source byte-for-byte; all eight historical records were frozen from the same immutable source.
- Previously executed `cargo test --locked --offline --package kars-controller issued_certificates_pass_strict_server_verification` — passed: 1 test, 2139 filtered out. Real OpenSSL strict verification covers IP/DNS positives and wrong-CA/name negatives. The new regression failed against the original issuer as expected; `tls-repro/regression-baseline.json` records exit 101 and the missing-AKI cause. These implementation checks were not rerun for this approval-only preparation.
- Retained `tls-repro/before.json` — real strict client rejected the original certificate with code 85. Retained `tls-repro/after.json` — the unchanged Hermes httpx client passed real local TLS on Python 3.14.4 / OpenSSL 3.6.5. Its `hosted_kind_acceptance` remains false.
- From `P/tls-approval-prepared`: `TMPDIR="$P/tls-approval-scratch" TMP="$P/tls-approval-scratch" TEMP="$P/tls-approval-scratch" PYTHONPATH="$P/tls-approval-baseline/ci/tests" PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p security_audit_gate_test.py -v` — passed: 260 tests, zero skips (`tls-approval-tests.log`). The same command from `P/tls-approval-baseline` — passed: 214 tests, zero skips (`tls-approval-baseline-tests.log`). All fixture repositories and scratch directories are inside the session artifact folder, not the checkout.
- The suite preserves existing eight-scope coverage and adds 46 TLS-scope tests, including inherited behavior checks. It verifies exact contexts, source/base/ancestry, committed approval and signer requirements, both historical records' immutable retention, tamper/deletion rejection, missing TLS approval, old-marker rejection, implementation/document/lock drift, other unsigned records, policy-only changes and synthetic merge/push contexts. Fixture commits use substituted test hashes; this is gate regression coverage, not an execution of hosted CI on the real source.
- Against the unchanged published gate, the two new positive tests (`test_exact_beta_approval_accepts_one_real_signer` and `test_tls_scope_carries_both_records_unchanged_in_exact_contexts`) fail as expected: three failed assertions, zero errors or skips (`tls-approval-new-tests-baseline.log`). The new scope is not already approved by the old gate.
- `bash -n ci/security-audit-required.sh`, per-file `git diff --no-index --check`, and isolated `git apply --check --whitespace=error-all` / patch reconstruction — passed. The exact four prepared files reconstruct byte-for-byte; all eight historical records and case blocks remain unchanged. The separate `tls-approval-source-hashes.json` records file, patch and historical-record SHA-256 values; `tls-approval-validation.log` retains exact validation commands and results.
- Preparation changes only this new record and the three approval-plumbing files under `tls-approval-prepared`. No checkout edits, remote commits, ref updates, publication, CI dispatch, merge, protection changes, container builds or release actions were performed by this preparation.

## Verdict and authorization

Accept this exact-source repair under the maintainer's recorded decision, subject to all required technical checks. At `2026-11-12T20:37:08.951024+00:00`, the user selected **“Authorize TLS repair publication, sign-off, merge and release (Recommended)”**. The evidence in `P/tls-authorization.json` explicitly covers the two-file repair, source-bound maintainer sign-off, required CI, normal merge first, and a conditional reviewer-only exception after refusal with immediate full protection restoration. It binds the parent, base and implementation patch hash above. This record binds the resulting immutable implementation source on the existing PR623; no replacement PR is needed.

The decision is genuinely authorized by Pal Lakatos-Toth, with no fictional independent reviewer. It is an exception only to the normal two-human-sign-off requirement for this exact scope, not an assertion that independent review occurred. The approval follow-up may change only this new record, `ci/security-audit-required.sh`, `ci/tests/security_audit_gate_test.py` and `docs/security-audits/README.md` after the reviewed source. It may not broaden implementation or modify any historical record.

Normal merge must be attempted first. Only a review-requirement refusal permits the separately authorized reviewer-only exception for PR623, followed immediately by full protection restoration. Required CI, scans, artifact signing/source checks and fresh Kind acceptance are not waived. This record and gate grant no automatic branch-protection bypass and execute no publication, merge or release. This task stops at validated preparation.

Beta-approval-exception: Azure/kars#623; source=acb9d9a2ccef87003830fa10c2cc3eb08e5129f9; base=af5ff64796898060f8f9d4a9dc853013f1d51981
Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
