<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit — Kind Docker image descriptor identity repair (beta)

Date: 2026-11-12
PR: https://github.com/Azure/kars/pull/623
Approved-by: `pallakatos` (Pal Lakatos-Toth), by explicit user authorization recorded below
Reviewed implementation source: `c7a2af366ef0cbbb72418e152c59f0c317c3e856`
Reviewed integration base: `af5ff64796898060f8f9d4a9dc853013f1d51981` (`kars-bridge`)
Parent / prior published PR623 head: `384beb7c77de8ec7167fac977f5aeb99dcbc6747`
Additional implementation scope: `tests/e2e/kind-router-image.mjs` (+30/-8), `cli/src/testing/kind-router-image.test.ts` (+47/-2), `docs/governed-inference-budgets.md` (+5/-1) only.
Retained scope: PR622 npm registry repair and PR623 Headlamp evidence-permission and private-TLS repairs, including all three unchanged approval records.

## Summary

The maintained Kind image-identity helper previously treated Docker inspect `.Id` as a config digest for every image store. Containerd-backed Docker stores instead expose a manifest or index identity there. The CI-only repair binds the typed Docker Descriptor for those stores and retains config-ID binding for classic Docker. It keeps existing content hashes, platform graph, unpacked content, CRI and alias verification. The companion regression tests and governed-inference-budget documentation explain and exercise these distinct identities.

This approval does not change runtime code, container images, dependencies or production policy. It records a new, exact-source maintainer decision for the additional three-file repair on the existing PR, not a retrospective extension of any earlier sign-off.

## T1: New capability / attack surface? (NO)

- No endpoint, credential flow, runtime privilege, network access, trust anchor or image content is introduced. The implementation changes a CI verification helper, its tests and documentation only.
- Authentication, tenant isolation, encrypted transport, governed egress, secret protection and host/VM safety limits remain intact. Earlier npm, Headlamp and TLS implementation scopes are retained, not broadened.

## T2: Security-control change? (CI IDENTITY COMPATIBILITY; EXACT MAINTAINER EXCEPTION)

- Image identity remains content-bound. The helper distinguishes a typed manifest/index Descriptor from a config digest instead of conflating them. Content-hash, platform-graph, unpacked-content, CRI and alias checks are retained; identity verification is not disabled.
- This tenth gate case binds the exact source and base above, the committed approval marker and genuine maintainer signer, approved-source ancestry, and Azure/kars PR623 merge or unchanged `kars-bridge` push context.
- Only the new record, `ci/security-audit-required.sh`, `ci/tests/security_audit_gate_test.py` and `docs/security-audits/README.md` may differ from the approved implementation source. No implementation path or historical audit record is excluded from that comparison. No configurable waiver, capability-pattern change or workflow-only exemption is added; the cumulative PR already touches capability paths.
- Carry-forward under this new approval is limited to the unchanged `2026-11-12-openclaw-npm-registry.md`, `2026-11-12-headlamp-evidence-permissions.md` and `2026-11-12-private-tls-authority-key-identifier.md` records, still newly added against the common integration base. Missing or invalid new approval cannot borrow their signatures. Changing or deleting any carried record invalidates this approval.
- All nine earlier case identities and historical records remain unchanged. The original npm, Headlamp and TLS exact-source paths retain their original semantics. The new carry-forward path is available only when the new exact-source approval validates.
- Copilot records the user's delegated decision; it is not an independent human reviewer or an independent security auditor. No second human review or signature is claimed.

## T3: Availability / fail-open risk? (LOCAL CI ASSERTION REPAIRED)

- The original helper assertion is reproduced against the exact old source. The updated focused suite passes for the repaired identity semantics. This supports the local CI-helper repair, not a claim of hosted Kind success.
- Hosted verification remains pending. Earlier failed attempts remain retained as failed evidence; none is reset, replayed or relabeled successful.
- Required hosted CI, shipping-image checks, signed-release verification and fresh Kind customer acceptance remain required. Passing offline tests is not beta readiness, completed release or proof of provider/runtime customer journeys.

## Verification

Preparation artifacts are retained under `P=/Users/pallakatos/.copilot/session-state/f326f1b4-5650-4dda-857c-ca6b7f848ccf/files/beta5-headlamp-permissions-repair`; `D=$P/kind-image-approval-prepared`.

- `P/kind-image-implementation-scope.json` records the three implementation paths and line counts above. `P/kind-image-validation.json` records the implementation results: `cd cli && npm test -- src/testing/kind-router-image.test.ts` — passed, 23 tests; `cli/node_modules/.bin/oxlint cli/src/testing/kind-router-image.test.ts tests/e2e/kind-router-image.mjs` — passed, zero warnings/errors; original assertion reproduced against the old source. These implementation checks were previously executed, not rerun during approval-only preparation. The validation artifact's earlier publication-authorization status predates the subsequently confirmed decision below.
- Published baseline: `P/tls-approval-prepared`, verified against all four `after_sha256` entries in `P/tls-approval-source-hashes.json`. The eight earlier historical records are verified against that artifact's frozen hashes, plus the current TLS record. The user-owned checkout and older dirty local gate are not preparation baselines.
- From `D/.validation/baseline`: `TMPDIR="$D/.validation/scratch" TMP="$D/.validation/scratch" TEMP="$D/.validation/scratch" PYTHONPATH="$D/.validation/baseline/ci/tests" PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p security_audit_gate_test.py -v` — passed, 260 tests, zero skips (`.validation/baseline-tests.log`). The same command from `D` — passed, 314 tests, zero skips (`.validation/prepared-tests.log`).
- The existing fixture harness supplies isolated local Git repositories and substituted test hashes. The new subclass adds 54 tests including inherited behavior: exact PR/push and synthetic-merge contexts, committed signer/marker checks, source/base/ancestry binding, all three prior records' retention and tamper/deletion rejection, missing/invalid new approval, earlier-marker rejection, helper/test/document/lock drift, policy-only edits and other unsigned records. All existing test bodies remain unchanged except adding the tenth record to the cross-scope rejection matrix.
- With the new test module's `GATE` redirected only to the frozen old script, `test_exact_beta_approval_accepts_one_real_signer` and `test_image_scope_carries_three_unchanged_records_in_exact_contexts` fail as expected: two tests, three failed assertions, zero errors/skips (`.validation/new-positives-old-gate.log`). The old gate does not already approve this source. The baseline itself is not edited.
- `bash -n ci/security-audit-required.sh`, per-file `git diff --no-index --check`, and isolated `git apply --check --whitespace=error-all` / patch reconstruction — passed. The four prepared files reconstruct byte-for-byte; all nine previous case blocks and historical records are preserved. `.validation/verification.json` retains exact commands/results and SHA-256 evidence for the four staged files and unchanged historical cases/records.
- Only the four staged approval files and isolated validation artifacts are written. Local fixture commits are test data only. No checkout edits, remote writes, real branch/ref updates, publication, CI dispatch, Docker or cluster operations, merge, protection changes or release actions occur in this preparation.

## Verdict and authorization

Accept this exact-source repair under the maintainer's explicit decision, subject to all required technical checks. At `2026-11-12T22:19:41.037506+00:00`, the user was asked:

> The CI-only image-identity repair is ready: three files, 23 tests passed, no runtime or image changes. May I publish it on Azure/kars#623 with refreshed source-bound sign-off, then merge and release after required CI passes, retaining the previously authorized reviewer-only exception and immediate protection restoration?

The user selected **“Authorize this CI repair, sign-off, merge and release (Recommended)”**. The confirmed question and answer are retained in `P/kind-image-authorization.json`. This record binds that delegated single-maintainer authorization to the immutable implementation source, parent, base and three-file scope above. It does not claim the user personally executed these tests, independently audited the source or supplied a second human review.

The approval follow-up may change only this new record and the three enumerated gate/test/README paths after the reviewed implementation source. All previous records remain immutable. Normal merge must be attempted first; only a review-requirement refusal permits the previously authorized reviewer-only exception for PR623, followed by immediate full protection restoration. Required CI, scans, signing, source verification and fresh Kind acceptance are not waived. Neither this record nor the gate implements a branch-protection bypass or executes publication, merge or release. This task stops at validated preparation.

Beta-approval-exception: Azure/kars#623; source=c7a2af366ef0cbbb72418e152c59f0c317c3e856; base=af5ff64796898060f8f9d4a9dc853013f1d51981
Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
