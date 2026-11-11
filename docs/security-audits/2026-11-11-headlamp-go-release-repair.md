<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit — Native release builds and Headlamp Go patch (beta)

Date: 2026-11-11
PR: https://github.com/Azure/kars/pull/608
Reviewed implementation source: `11edc2dc6fde46d8bbf8319ebc15ce2e2c0df18e`
Reviewed integration base: `2068be5faab76afeb1fa51afb7ebd20fd8177530`
Scope: `.github/workflows/release-public-interim.yml`, `cli/scripts/release-artifacts.test.mjs`, `sandbox-images/headlamp/Dockerfile`, `sandbox-images/headlamp/build-backend.sh`, `sandbox-images/headlamp/README.md`.
Gated paths: `sandbox-images/headlamp/Dockerfile`.

## Summary

The native release repair moves Bridge BFF, Prometheus and Grafana ARM64 source builds from QEMU to native GitHub-hosted ARM64 runners, retaining native AMD64 builds. It merges both architectures, validates the manifest, attests and signs the merged digest, and makes CLI packaging wait for authoritative merged-image receipts. Other image rows, AL3 runtime packaging, image build time limits and stable-only `latest` publication are unchanged.

The first hosted check of that repair found CVE-2026-97054 in Headlamp's unchanged Go 1.27.3 backend. The additional three-file patch pins the verified multi-platform Go 1.27.4 builder and updates the exact toolchain guard, image label and documentation together. Headlamp 0.41.0, its upstream source, module pins, frontend/plugin build paths and AL3 runtime base remain unchanged. No advisory is ignored and the scanner is not weakened.

## T1: New capability / attack surface? (NO)

- No new runtime endpoint, privilege, network access, dependency API or product feature is introduced.
- Native ARM64 builds use GitHub-hosted Ubuntu runners rather than emulated compilation on the AMD64 runner. Build tools remain build-stage-only; the shipping Headlamp image stays non-root AL3 distroless without a compiler or package manager.
- The Go builder is pinned by both version and verified OCI index digest, with Linux AMD64 and ARM64 entries present. Verification retrieved registry metadata only; it did not run a local container.

## T2: Security-control change? (YES)

- Native architecture tags are intermediate products. Only the validated merged digest receives the authoritative receipt used by packaging. Missing, duplicate or wrong Linux architectures fail before receipt publication. GitHub provenance and keyless Cosign signing remain mandatory.
- The patched Go standard library addresses the HTTP/1 header-map race reported by CVE-2026-97054. Official Go release metadata and the retained scan identify Go 1.27.4 as fixed; this is not a claim that an exploit was reproduced against Headlamp.
- Shipping-image, frontend-lock, rendered-inventory and plugin-lock scans retain all severities, unfixed findings and their existing exit requirements. The original UNKNOWN-severity finding is retained, not reclassified or dismissed.
- The explicitly authorized single-maintainer exception changes only the human sign-off requirement for this exact source/base. It neither fulfills the normal two-human policy nor waives any technical gate.

## T3: Availability / fail-open risk? (REDUCED)

- Native builds address the beta.2 QEMU source-compilation timeouts without enlarging their 90-minute limit. The manifest job remains bounded and fails closed on missing outputs, invalid platforms, failed attestation or failed signing.
- The Go patch addresses an HTTP/1 server panic condition involving concurrent body reads and response-header writes. Runtime behavior still requires a newly built image and fresh qualification.
- Beta.1 and beta.2 remain consumed failures. No release artifact is presumed to exist, and no independent fresh-Kind customer acceptance is claimed.

## Verification

- `node --test cli/scripts/release-artifacts.test.mjs` — passed, 10 tests on the native workflow repair. Its three new tests failed against the frozen pre-repair workflow as expected. They cover native runner/platform pairing, distinct outputs, signed receipt ordering, complete manifests and prerelease `latest` rejection.
- `go run github.com/rhysd/actionlint/cmd/actionlint@v1.7.11 .github/workflows/release-public-interim.yml` — passed on the unchanged native workflow repair.
- `GOTOOLCHAIN=go1.27.4 GOMAXPROCS=2 GOMEMLIMIT=1GiB GOFLAGS=-p=2 HEADLAMP_UPSTREAM_SOURCE=<retained pinned backend/cmd/headlamp.go> TMPDIR=<canonical temporary directory> go test -count=1 -v sandbox-images/headlamp/patch-backend.go sandbox-images/headlamp/patch-backend_test.go sandbox-images/headlamp/static-cache.go sandbox-images/headlamp/static-cache_test.go sandbox-images/headlamp/oidc-navigation.go sandbox-images/headlamp/oidc-navigation_test.go` — passed, 12 top-level tests, no skips. The source fixture is from pinned commit `066c5be9deb14b12bcef3485b531c64a84a2a219`. The initial default macOS `/var` temporary-path assertion failure is retained; unchanged tests pass with a canonical temporary directory.
- `sh -n sandbox-images/headlamp/build-backend.sh` — passed.
- Scoped `git diff --check` for the three-file Headlamp patch — passed.
- Hosted run `43032741802`, job `130563683553`, failed the original shipping-image scan with CVE-2026-97054 in `headlamp/headlamp-server`, Go stdlib 1.27.3, fixed in 1.27.4. Original artifact `13861637469` archive SHA-256: `a74b212f19babec892002fbaeceba649f54099601913592c39bbbbf609d76dda`. Source and inventory scans passed. These results do not qualify the changed image.

- `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s ci/tests -p security_audit_gate_test.py` — passed, 96 tests across all four immutable approval scopes, including cross-record and cross-PR isolation. An initial missing-colon syntax error in the expanded test was corrected before this run; its failed log is retained.
- `bash -n ci/security-audit-required.sh` — passed.
- Scoped `git diff --check` for the gate, tests, audit README and Headlamp files — passed.

Fresh hosted build, vulnerability scans and all required checks remain mandatory. No local Docker restoration, container execution, cluster/provider action or host-trust modification was performed. Full native release execution, anonymous signed artifact verification and the independent fresh-Kind installation remain unproven.

## Verdict

Approved by the maintainer for the exact source and integration base above. The user selected **“Publish fix with my source-bound sign-off (Recommended)”** after disclosure of the three-file Go patch, passing 12 backend tests, unchanged AL3/scans, required audit-gate tests and continuing hosted checks, merge and beta release. The frozen Headlamp patch SHA-256 is `c7f4bf780d9ef403134b5f2c8ea72581c20fe7c66be403591c24fd46b8b23df6`; it extends the previously authorized two-file native release repair at parent `cf6dea02c699d0da9e8057ca5139884f9d5e926b`.

No second human review is claimed. The PR593, PR606 and PR607 approvals are unchanged and do not supply this sign-off. The follow-up may change only this record, `ci/security-audit-required.sh`, `ci/tests/security_audit_gate_test.py` and `docs/security-audits/README.md`. Every other tracked path must match the approved implementation source, with exact base and ancestry retained. No technical-check bypass, artifact-signing waiver or customer-acceptance result is granted by this record.

Beta-approval-exception: Azure/kars#608; source=11edc2dc6fde46d8bbf8319ebc15ce2e2c0df18e; base=2068be5faab76afeb1fa51afb7ebd20fd8177530
Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
