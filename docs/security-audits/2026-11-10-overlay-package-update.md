<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit — OverlayFS and pinned Grafana build repairs (beta)

Date: 2026-11-10
Scope: `sandbox-images/openclaw/update-node-dependencies.cjs`, `runtimes/tests/test_openclaw_node_security.py`, `runtimes/openclaw/README.md`, `sandbox-images/grafana/Dockerfile`, `sandbox-images/grafana/test_build_helpers.py`, `sandbox-images/grafana/README.md`.
Gated paths: `sandbox-images/grafana/Dockerfile`. The original OverlayFS-only scope did not trigger the capability gate; the subsequently authorized Grafana addition does. This current-PR record now covers the combined repair, not a later extension of the historical PR593 source approval.

## Summary

Both native sandbox-base builds in release run 42777543505 failed because OverlayFS rejected renaming an existing lower-layer MCP SDK directory with `EXDEV`. Keep the normal rename path; only for `EXDEV`, complete a local rollback copy before removing the original and installing the prepared replacement. Preserve relative links and the package-directory mode in that backup. Restore the backup if installation fails; refuse other rename errors without fallback.

Grafana's AL3 builder installed Microsoft Go 1.27.3, whose non-local selection guard rejected the recipe's existing `GOTOOLCHAIN=go1.27.2`. Add Microsoft's documented `MS_GOTOOLCHAIN_ALLOW_NON_LOCAL=1` only in the builder. Intentionally retain the previously qualified upstream Go 1.27.2 selection, exact version assertion, module checksum verification and both AL3 stages; do not silently switch to the newer Microsoft compiler or enable runtime downloads.

The maintainer explicitly selected “Authorize the scoped Grafana repair and publication exception” after being asked to approve this additional three-file repair in Azure/kars#606, its source-bound single-maintainer audit record, and a temporary review-only merge exception only after every required check passes. Signing and production safeguards remain mandatory. The exact additional patch SHA256 is `167e76e5bf81e273a7a449ac50c0bca3834f00abf88d8b1785c7c475e645e045`.

The combined approved implementation is source `187ada4b45203bc831e7976961ffcb2f92160886`, descended from the published OverlayFS repair `3c5d6e35899a7b1b83236f3a8345ab2b343255f9`, against integration base `ab78d11d66b626b4a176603643011ebc1cca1dff`. Approval-plumbing successors may change only the gate, its tests, the audit README and this exact record. The implementation has these SHA256 content digests:

| Path | SHA256 |
| --- | --- |
| `sandbox-images/openclaw/update-node-dependencies.cjs` | `26c9e1f326b77531ceee678ad98d5604662969ceac9c47ea8ee08a1c342c2b10` |
| `runtimes/tests/test_openclaw_node_security.py` | `b1140e4ecb1f6587ceeef17a10ea949d625ef823f9cc3395c30d2206231825dd` |
| `runtimes/openclaw/README.md` | `43724df81f1ceeb025c3ebad67b6353fcca54b02ededd6d24ba7a19b8142b96d` |
| `sandbox-images/grafana/Dockerfile` | `a22f1c3d8e653fc7255cc8e6c0895248947760c733c63d83ac6677c01d819222` |
| `sandbox-images/grafana/test_build_helpers.py` | `80bcb572e3407d63714041b0f64717e9e8416767f2a789bc4bc770a71b7e4a39` |
| `sandbox-images/grafana/README.md` | `9151c33ac0a3bb41f2ae7ff413cc301cbed8db46a154eabd1991315786f4ca04` |

## T1: New capability / attack surface? (YES/NO)
- NO. This is a build-only filesystem compatibility repair, not an agent capability or runtime installation path.

## T2: Security-control change? (YES/NO/NEUTRAL)
- NEUTRAL. Archive checksums, package identities, engines, dependency/peer constraints, target-path checks and prevalidation are unchanged. No network or lifecycle scripts are enabled. AL3 packaging, pinned runtime versions and installed security updates remain required.
- Grafana intentionally opts into the already-pinned upstream compiler in the AL3 build stage. The opt-in does not disable Go checksums and is absent from the final runtime stage.
- The normal two-human-reviewer capability policy remains in place, with this explicitly authorized, additional fixed-source exception for Azure/kars#606. The earlier PR593 source binding remains unchanged. There is one genuine maintainer sign-off; no second human review or completed technical acceptance is claimed.

## T3: Availability / fail-open risk? (REDUCED/NEUTRAL/INCREASED)
- REDUCED. The observed OverlayFS failure gains a narrow fallback. Incomplete backup copies are removed without modifying the original package. Other errors remain fatal; a failed Docker build step does not produce the final image layer.

## Verification
- `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest runtimes.tests.test_openclaw_node_security -v` — passed, 28 tests, including replacement, rollback, backup failure and non-EXDEV rejection.
- The three new EXDEV regression cases fail against the exact integrated updater; the unrelated-error guard passes against it.
- `node --check sandbox-images/openclaw/update-node-dependencies.cjs` — passed.
- `git diff --check -- sandbox-images/openclaw/update-node-dependencies.cjs runtimes/tests/test_openclaw_node_security.py runtimes/openclaw/README.md` — passed.
- Initial rollback-mode test failure was retained and corrected before approval. Local tests used Node v24.9.0; they do not establish success of the release's pinned Node image build.
- `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s sandbox-images/grafana -p 'test_*.py'` — passed, 8 tests. The new missing-opt-in test failed before the correction; this is source-contract coverage, not a Linux image rebuild.
- `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p security_audit_gate_test.py` — passed, 48 tests covering both exact approvals, changed-source/base/ancestry rejection, cross-approval isolation, dirty approval rejection and normal two-signer enforcement.
- `bash -n ci/security-audit-required.sh` and `sh -n sandbox-images/grafana/build-backend.sh` — passed.
- The initial PR606 CI private-TokenRequest denial test failed; successful CI requalification is still required. The original dispatcher release job also failed to upload its cosign signature after image and provenance publication; signing is not waived.
- Rebuilt Linux images, signed publication and independent fresh-Kind customer acceptance remain required; none is claimed by this record.

## Verdict
Accept the narrowly scoped repair for hosted qualification and release rebuilding. Do not skip security updates or treat these local tests as complete beta acceptance.

Beta-approval-exception: Azure/kars#606; source=187ada4b45203bc831e7976961ffcb2f92160886; base=ab78d11d66b626b4a176603643011ebc1cca1dff
Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
