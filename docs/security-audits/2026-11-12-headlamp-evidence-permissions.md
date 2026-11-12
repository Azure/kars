<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit — Headlamp evidence permissions and retained npm repair (beta)

Date: 2026-11-12
PR: https://github.com/Azure/kars/pull/623
Reviewed implementation source: `15281ad70925895f3f7c646b6503a2b1f6bd90d1`
Reviewed integration base: `af5ff64796898060f8f9d4a9dc853013f1d51981` (`kars-bridge`)
Parent containing unchanged PR622 approval: `88b7aeb4bd9856107f03227a4734f053b44e78e8`
Scope: retained PR622 npm repair plus `sandbox-images/headlamp/Dockerfile`, `sandbox-images/headlamp/README.md`, `ci/tests/headlamp_build_evidence_test.py`.
Gated paths: `sandbox-images/headlamp/Dockerfile`, retained `mesh-plugin/package-lock.json` and `runtimes/openclaw/package-lock.json`.

## Summary

This successor retains PR622's canonical npm registry repair and immutable approval record, adding only the tested three-file Headlamp evidence-permission repair. PR622 changed 131 Microsoft mirror URL prefixes to canonical registry URLs without changing the dependency/integrity graph or npm12 remote-source restrictions. Its five real-npm source-policy tests and ten focused packaging tests passed; those results are retained evidence, not new image qualification.

On PR622's final head, Headlamp [job 131314133754](https://github.com/Azure/kars/actions/runs/43255915273/job/131314133754) in [run 43255915273](https://github.com/Azure/kars/actions/runs/43255915273) built the image and passed its all-severity image scan. It then failed to extract `/usr/share/kars/headlamp/.`: `docker cp` reported permission denied on `frontend-build/package-lock.json`. The existing `COPY --chmod=0444` also makes the destination evidence directory non-traversable. This failure is preserved, not presented as a successful build-and-evidence acceptance.

The repair stages the actual `package.json`, `package-lock.json` and `vite.config.ts` with `install -d -m0755 ../frontend-build` and `install -m0444 ...`, then copies that directory without a chmod override. The evidence directory is traversable and the files remain read-only. Ownership remains root in the shipping image, preventing the non-root runtime from writing either the directory or its files.

## T1: New capability / attack surface? (NO)

- No new runtime feature, endpoint, credential flow or privilege is introduced. The evidence is the same actual build-input bytes already intended for extraction and hash verification, not a substitute manifest.
- Runtime, base-image and toolchain pins, dependency versions and integrity data are unchanged. Entra authentication, tenant isolation, encrypted transport, governed egress and secret protection are unchanged.
- Only evidence staging permissions, the matching documentation and focused regression tests change beyond the inherited npm repair and approval plumbing.

## T2: Security-control change? (SCOPED EVIDENCE PERMISSIONS; MAINTAINER EXCEPTION)

- Mode 0755 enables traversal of the root-owned evidence directory; mode 0444 preserves read-only files. No non-root write permission is added. The actual shell staging is tested under restrictive umask 077, including unchanged source modes and bytes.
- Image scans, inventory checks, source/hash binding and integrity verification are untouched. npm12 `allow-remote=none` and lifecycle restrictions remain in force. This approval does not waive a scan or technical check.
- The existing ordinary capability rule already covers the Headlamp Dockerfile. The eighth immutable exception uses the existing exact base, source ancestry, committed marker, genuine signer and PR/integration-context checks; it adds no workflow-only exception or capability-pattern change.
- PR622's approval is still newly added relative to the shared integration base. Only a separately valid, newly added PR623 approval may carry that exact historical record. The successor's implementation comparison includes the PR622 record, so changing it invalidates approval. No prior case or record is edited or repointed, and no other unsigned record or PR can use this carry-forward path.
- This is expressly authorized maintainer sign-off, not independent human review or an independent security audit. It is an exception to only the second-human-sign-off requirement for this exact immutable scope.

## T3: Availability / fail-open risk? (REDUCED)

- The repair addresses the observed evidence extraction failure without weakening scan coverage or hash verification. The regression fails against the original Dockerfile.
- Fifteen local evidence tests passed, including actual shell staging under umask 077, unchanged evidence bytes/source modes, hash binding and invalid-inventory rejection. Docker remains deleted by the user: no fresh image build/extraction or image/Kind acceptance was performed for this repair or approval.
- Retained npm and image-scan successes do not prove a completed signed release, useful delivered artifacts, working installation, independent security review or fresh Kind customer acceptance. Beta acceptance remains 0/2.

## Verification

- Read-only GitHub API verification confirms PR623's branch `fix/headlamp-evidence-permissions`, the exact source/base above, source parent `88b7aeb4bd9856107f03227a4734f053b44e78e8`, and the integration base as the source's merge base. The parent-to-source comparison contains only the three Headlamp repair files. The three existing approval-plumbing baselines and all seven historical records match the exact remote source and its parent byte-for-byte.
- Retained `headlamp-permissions-tests.log` — passed: 15 local tests. Retained `headlamp-permissions-baseline.log` — the new permissions regression failed against the original Dockerfile as expected. The implementation patch SHA256 is `d06d88b9933b6b0400b6552090ad1f0d62164d578c6c0d2896b4238f6daa5a93`.
- Retained PR622 evidence — five real-npm source-policy tests and ten focused packaging tests passed. No npm installation or packaging rerun was performed for this approval-only follow-up.
- From frozen approval baseline: `TMPDIR=<P>/approval-scratch TMP=<P>/approval-scratch TEMP=<P>/approval-scratch PYTHONPATH=/Users/pallakatos/Private/Repos/kars/ci/tests PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p security_audit_gate_test.py -v` — passed: 176 tests, zero skips. Git fixtures were isolated from the checkout.
- From prepared approval source, the same full-suite command above — passed: 214 tests across eight scopes, zero skips. The original 176 tests remain, with their cross-PR/record matrices extended to PR623. New coverage checks ordinary Headlamp Dockerfile gating, exact source/base/ancestry, inherited-record immutability, missing/forged successor fields, other unsigned records, lock/implementation drift and synthetic merge/push contexts. Logs: `approval-tests.log` and `approval-baseline-tests.log`.
- New exact-source and inherited-record positive tests against the unchanged published gate — failed as expected: both tests reject the unsupported PR623 approval, with three failed assertions including PR and integration contexts; no errors or skips. The failures are retained in `approval-new-tests-baseline.log`.
- `bash -n ci/security-audit-required.sh` — passed. Per-path `git diff --no-index --check -- <frozen-baseline-file> <prepared-file>` (using `/dev/null` for this new record) — passed with no whitespace diagnostics. `git apply --check --whitespace=error-all <P>/approval.patch` and isolated patch application — passed; reconstructed files match prepared bytes exactly and all seven historical records remain unchanged. Exact marker and signer counts are one each. The first validation driver mistakenly treated the no-index diff's expected exit 1 as failure; its log is retained, and corrected validation requires exit 0/1 with no diagnostics.
- No checkout edits, publication, merge, release, branch-protection changes, container/cluster/provider execution or beta-board changes were performed for this preparation. Hosted technical CI and fresh release/Kind acceptance remain outstanding.

## Verdict

Accept the exact-source repair under the maintainer's recorded decision, subject to all required technical checks. At `2026-11-12T16:28:53.529973+00:00`, the user selected **“Authorize publication, sign-off, merge and release, including the conditional reviewer-only exception”** for the successor to PR622 carrying its npm repair and the tested Headlamp evidence-permission fix. The parent authorization binds the implementation patch SHA256 recorded above; this follow-up binds the resulting remote source and reviewed base explicitly.

The approval-only follow-up may change only this record, `ci/security-audit-required.sh`, `ci/tests/security_audit_gate_test.py` and `docs/security-audits/README.md`. All seven earlier immutable cases and records remain unchanged. PR622's sign-off does not itself authorize the successor. This decision is maintainer sign-off, not an independent security review or beta-readiness declaration.

Only if normal merging refuses for review requirements may the publisher use the conditional reviewer-only exception for PR623, restoring full protection immediately afterward. Required checks, scans, artifact signing/source validation and fresh-Kind acceptance are not waived. This record does not publish, merge, release or alter branch protection by itself; the current task prepares it only. No permission to broaden implementation is granted.

Beta-approval-exception: Azure/kars#623; source=15281ad70925895f3f7c646b6503a2b1f6bd90d1; base=af5ff64796898060f8f9d4a9dc853013f1d51981
Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
