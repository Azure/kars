<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit — OpenClaw staged SDK byte comparison (beta)

Date: 2026-11-12
PR: https://github.com/Azure/kars/pull/617
Reviewed implementation source: `234b63e60f231f1c80286efaf9d2d44c91854f79`
Reviewed integration base: `e5db80c4e761cb5a92eb097b98713ba43d3d9394`
Scope: `sandbox-images/openclaw/Dockerfile`, `cli/src/testing/sandbox-hardening.test.ts`.
Gated paths: `sandbox-images/openclaw/Dockerfile`.

## Summary

Beta.3 run [43113949998](https://github.com/Azure/kars/actions/runs/43113949998) failed in both native OpenClaw builds because the AL3 builder has no `cmp`. The public-release caller supplies the optional staged SDK argument and reaches this compatibility check. The canonical vendored SDK passed its SHA256 check before both builds exited 127. The run ended with 48 successful jobs, two failed builds and three skipped downstream jobs; no CLI/chart or public release was published.

The repair uses the existing Node.js executable and `Buffer.equals` to compare the staged tarball with the canonical file. It does not replace the vendored SDK, install another utility, change a base image or alter runtime/dependency pins. The basename guard and canonical checksum verification remain in place.

## T1: New capability / attack surface? (NO)

- No new endpoint, privilege, network access, dependency, credential flow or runtime feature is introduced.
- Node.js is already part of the AL3 builder. Both paths are passed as arguments, not interpolated into JavaScript source; shell quoting and the basename guard are retained.
- OpenClaw 2026.9.8, Node 24, Hermes and the AL3 shipping image are unchanged.

## T2: Security-control change? (EQUIVALENT SDK CHECK; SCOPED HUMAN EXCEPTION)

- Byte equality remains mandatory when the optional staged SDK is supplied. Changed bytes or lengths return exit 1. Missing files also fail nonzero. The canonical SHA256 verification still runs first.
- Five behavioral tests extract and execute the actual inline Dockerfile JavaScript with an empty utility PATH, covering identical binary files, a changed byte, truncation and either missing input.
- The maintainer explicitly authorized a single-maintainer sign-off for this source/base. This replaces only the two-human sign-off requirement for this scope; no independent human review is claimed. Technical checks, scans, signing and release/source validation remain mandatory.

## T3: Availability / fail-open risk? (REDUCED)

- The missing build utility is replaced without skipping its integrity check. Invalid or absent staged inputs still fail the build.
- Both inputs are small, pinned build-time SDK archives; this does not introduce a runtime upload or unbounded runtime file reader.
- Prior beta failures remain consumed evidence. Successful source tests do not establish rebuilt-image qualification, signed-release availability or independent fresh-Kind acceptance.

## Verification

- `cd cli && npm test -- src/testing/sandbox-hardening.test.ts` — passed: 19 tests, with two existing GNU-cp tests skipped on macOS. All five added behavioral cases failed before the production repair and pass after it.
- `cd cli && ./node_modules/.bin/oxlint src/testing/sandbox-hardening.test.ts` — passed, zero warnings/errors.
- `git diff --check -- sandbox-images/openclaw/Dockerfile cli/src/testing/sandbox-hardening.test.ts` — passed.
- `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p security_audit_gate_test.py` — passed, all 120 tests across five immutable approval scopes, including cross-record and cross-PR rejection.
- `bash -n ci/security-audit-required.sh` — passed.
- `git diff --check -- ci/security-audit-required.sh ci/tests/security_audit_gate_test.py docs/security-audits/README.md sandbox-images/openclaw/Dockerfile cli/src/testing/sandbox-hardening.test.ts` — passed.
- Full `make test` / `make lint`, local container builds and fresh-Kind installation were not run. The user removed Docker; it was not restored. Hosted image rebuild and customer acceptance remain required.

## Verdict

The maintainer selected **“Publish this repair via gh and update my sign-off (Recommended)”** after disclosure of the exact two-file build repair, passing focused tests, unchanged SDK verification, new PR publication through `gh`, source-bound sign-off, merge only after required checks and release of the next unused beta. Approved patch SHA-256: `25feb4ad40ce8574593ad92727b3b1a94b21fd9a8398505c5de4204252e76335`.

The approved implementation commit contains only the two scoped files above. The approval-only follow-up may change only this record, `ci/security-audit-required.sh`, `ci/tests/security_audit_gate_test.py` and `docs/security-audits/README.md`. Exact base, ancestry, repository and PR/integration context are enforced. No preceding PR approval is repointed or reused. No technical-check bypass, artifact-signing waiver or customer-acceptance result is granted.

Beta-approval-exception: Azure/kars#617; source=234b63e60f231f1c80286efaf9d2d44c91854f79; base=e5db80c4e761cb5a92eb097b98713ba43d3d9394
Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
