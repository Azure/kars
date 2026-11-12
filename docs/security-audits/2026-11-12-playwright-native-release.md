<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit — Playwright native release builds (beta)

Date: 2026-11-12
PR: https://github.com/Azure/kars/pull/621
Reviewed implementation source: `efadcd4932c08844a94e62080e45b6256c29a58a`
Reviewed integration base: `01548f2c365a94ca2016987f03a496a237ea46ff`
Scope: `.github/workflows/release-public-interim.yml`, `cli/scripts/release-artifacts.test.mjs`.
Gated paths: none under the normal capability-path filter; this newly added voluntary record invokes the explicit source-bound approval checks.

## Summary

Beta.4 run [43181183188](https://github.com/Azure/kars/actions/runs/43181183188),
[job 131064869925](https://github.com/Azure/kars/actions/runs/43181183188/job/131064869925),
failed building the arm64 managed Playwright image under QEMU. Dockerfile line 20
runs `browser-platform.sh playwright install chromium`. All five browser archive
download attempts reached 60–80% before `qemu: uncaught target signal 4 (Illegal instruction)`.
The final `Download failure, code=null` comes from child-process exit handling;
it is a symptom of those crashes, not evidence of a CDN outage. The exact
faulting instruction or native component is not established by the log. The
amd64 browser download succeeded. The failed run and its original log are retained;
this repair does not relabel the release as successful.

The two-file repair splits only the Playwright release build into native amd64
and arm64 matrix rows and adds its image to the existing signed merged-manifest
path used by BFF, Prometheus and Grafana. The AL3 Dockerfile, browser platform
helper, dependency lock, browser version and image pins are unchanged. Native
Playwright image qualification already exists in the maintained CI workflow;
this record does not claim that qualification was executed for this repair.

## T1: New capability / attack surface? (NO)

- No new runtime capability, endpoint, network permission, credential flow or dependency is introduced.
- The release uses the existing GitHub-hosted native runner pattern rather than emulating arm64. Build contexts and image repositories remain unchanged.
- No local Docker installation, container execution, cluster or inference provider call was performed for this repair.

## T2: Security-control change? (PRESERVED ARTIFACT CONTROLS; SCOPED HUMAN EXCEPTION)

- Architecture-specific builds retain SBOMs, provenance and keyless signatures. They do not publish authoritative per-architecture image receipts.
- The merged manifest must contain both Linux architectures. Its provenance and keyless signature precede the single authoritative image receipt, under the existing artifact name.
- CLI/chart packaging still waits for merged receipts, verifies anonymous image access and digest equality, and binds `managedMcp.playwrightImage` to the merged digest. No artifact check or signing step is removed.
- The maintainer explicitly authorized a separate source-bound single-maintainer sign-off for PR621. No second human review is claimed. Earlier approvals, technical checks, vulnerability scans and branch protections remain intact.
- The gate recognizes this newly added voluntary record despite the workflow/test-only scope. Exact source, base, ancestry, marker, signer and PR/integration context checks remain mandatory for the exception; historical record edits do not reopen approval.

## T3: Availability / fail-open risk? (EMULATION FAILURE PATH REMOVED)

- Native arm64 builds remove QEMU from the failing browser-install step; they do not retry indefinitely, change the browser download source, substitute another OS or suppress installation errors.
- Missing or duplicate architectures fail manifest validation. Missing, duplicate, malformed or untrusted image receipts still fail packaging checks. Prereleases do not publish `latest`.
- Offline regression success does not establish native container build success, browser qualification, public release availability or independent fresh-Kind customer acceptance. Those remain required.

## Verification

- `node --test --test-name-pattern='native|required|image binding|receipts|workflow publishes' cli/scripts/release-artifacts.test.mjs` — expected failure before the workflow repair (six passed, one failed); passed after repair (seven passed). Behavioral manifest tests execute the maintained Bash with a test-only simulated `docker` function, not Docker itself, for BFF and Playwright across five architecture combinations and both release channels.
- `git diff --check -- .github/workflows/release-public-interim.yml cli/scripts/release-artifacts.test.mjs` — passed.
- `TMPDIR="$PWD/.audit-gate-test-work" PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p security_audit_gate_test.py` — passed, all 149 tests across six immutable approval scopes. Fixtures were created only under the repository-local scratch directory and cleaned afterward. Coverage includes cross-record/cross-PR rejection, exact ancestry/base/marker/signer/allowed-path checks, workflow-only record enforcement and historical-record isolation.
- `bash -n ci/security-audit-required.sh` — passed.
- `git diff --check -- ci/security-audit-required.sh ci/tests/security_audit_gate_test.py docs/security-audits/README.md` — passed. The new untracked record was separately checked with Python for trailing whitespace and its final newline; passed.
- No native/container rebuild, full packaging test or fresh-Kind installation was performed. No new release result or customer acceptance is asserted.

## Verdict

The maintainer selected **“Publish this repair via gh and update my sign-off (Recommended)”**
in response to: **“May I publish this two-file repair through gh as a new Azure/kars PR,
add your sign-off bound to its exact source, and merge/release the next unused beta
after checks pass? This does not authorize changing branch protections.”**
This separately authorizes the PR621 implementation identified above, not an
extension of PR617 or any preceding approval.

The approved implementation commit contains only the two scoped files. The
approval-only follow-up may change only this record, `ci/security-audit-required.sh`,
`ci/tests/security_audit_gate_test.py` and `docs/security-audits/README.md`. The
record documents one genuine maintainer decision; AI preparation is not an
independent human sign-off. Merge/release remains conditional on required checks;
no protection change, technical-check bypass, artifact-signing waiver or
fresh-system acceptance claim is authorized.

Beta-approval-exception: Azure/kars#621; source=efadcd4932c08844a94e62080e45b6256c29a58a; base=01548f2c365a94ca2016987f03a496a237ea46ff
Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
