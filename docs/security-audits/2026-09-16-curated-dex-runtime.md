<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Curated Dex runtime: bounded delegated source review

Base: `b413f340d8d41aa3227538deee13569ca62d3981`.
Reviewed source: `f7cc7df8ef1cdfce9d4d92159a4e0ade5153bc1f`.

Status: **Source-approved for hosted qualification, not runtime publication or
deployment approval.** The final distroless runtime, OIDC/SQLite, linkage and
image-scan job must actually pass before acceptance.

## T1: New surface and reason for the change

This is a security rebuild of the existing optional Dex IdP, not a second
authentication framework or a new credential authority. The latest official
stable v2.45.1 image was scanned before use and rejected with 128 High and five
Critical findings. It was not mirrored or deployed. Moving its registry
reference alone would not repair its contents.

The recipe pins upstream application source at
`11d2eeb52b42e1980e14cb91e69dd9e3faab2076`, its archive hash, the Go 1.26.8
builder and Microsoft Azure Linux 3 distroless base. Real Go-generated root
and nested API module locks are committed with their source/input, checksum
and module inventories. The normal build uses those locks read-only; it
cannot run the separate dependency-resolution recipe implicitly.

No configuration, credentials, default issuer or user account ships in the
runtime. The existing chart's direct `dex serve` invocation remains supported.
Unused upstream template-expansion wrappers are not included. CGO and real
SQLite remain enabled; other upstream connector/storage code is retained,
not replaced by stubs. Unconfigured backend integrations remain unqualified.

## T2: Security-control and compatibility changes

The dependency selections close the recorded Go/library advisories. A new
advisory can still reject the final image; selection alone is not a clean scan.
Two disclosed production call-site patches use constant `"%s"` formats for
already-formatted OAuth errors rather than disabling vet. A historical SAML
test fixture uses its signed time through the library's test clock. Production
certificate validation and signed fixture bytes remain unchanged.

Patch application verifies original source, reviewed patch bytes, expected
post-patch files and the complete final source inventory. It does not ignore
changed application files or regenerate expected hashes from arbitrary output.
Git check/apply uses the existing builder toolchain with strict whitespace
checking and no unsafe-path option.

The shipping filesystem preserves the pinned Microsoft base, its CA trust
and RPM inventory. It adds the application, web assets and explicit source,
dependency and legal notices, not Debian libraries or build tools. Authentic
upstream `LICENCE` files are collected alongside `LICENSE`; missing licenses
still fail rather than being relabeled or skipped.

The runtime probe verifies the saved, unexpired ID token and its original
key identity against the restarted SQLite-backed server **before** refresh.
It rejects lost/replaced keys, including reuse of a key ID, while permitting
rotation that retains the previous verification key. Existing refresh,
userinfo, nonce, issuer, audience and negative authentication checks remain.
Test credentials stay in private ephemeral fixtures, not shipping layers.

## T3: Qualification and retained limits

Actual no-push hosted generation produced the committed Go lock artifact.
Its transport/member hashes, source inputs, toolchain and selected graph were
verified on import. Actual hosted execution subsequently passed the focused
compatibility tests, upstream root/API race-suite commands, nine signing-key
continuity scenarios and probe compilation. The upstream reports were retained
in the test image; successful command exit is not an invented test count or
proof of skipped external-service integrations.

Earlier failures remain recorded: a license filename was not recognized;
newer vet checks rejected two call sites; a historical signed fixture expired;
and the builder lacked the first selected patch utility. Each was corrected
without changing an enforcement expectation or disabling security checks.

Twenty pinned-source application/integrity regressions passed with zero skips.
The parent also checked CI aggregate/report contracts and precise licensing
classification. Go wrappers use normal source headers; only enumerated legal,
generated integrity and upstream snapshot files have non-header coverage.
Unknown source files in those directories remain checked. Literal unified-diff
context whitespace is preserved and validated by patch/integrity tests rather
than corrupting the checksum-bound patch data.

The mandatory new IdP component job builds the actual runtime and tools,
collects upstream reports, invokes the memory/SQLite/old-key/linkage/inventory
harness and scans the final OS and Go binary with pinned Trivy and a fresh DB.
High/Critical findings, including unfixed findings, remain fatal. Evidence is
retained even on failure. Existing component jobs, cancellation policy, native
requirements and read-only workflow permissions remain unchanged.

Independent contexts reviewed the packaging/dependency/source boundary,
closed the old-key-continuity finding and compatibility patches, and reviewed
the CI execution path. No residual source finding remains within those scopes.
This does not establish final-image qualification, independent rebuild
reproducibility, enterprise connector compatibility or live beta acceptance.
No cluster or main-branch mutation is authorized by this record.

## Delegation and verdict

### Subsequent security discussion follow-up

Candidate `369aa8616a239cfab7b4612f99188e99441596d2` passed the complete
technical checks, including actual final-image OIDC/SQLite, linkage and
High/Critical scans. Guarded merge nevertheless stopped before any write on
two unresolved GHAS conversations. These were not waived because the image
threshold passed.

CVE-2026-32952 is a real Medium-severity issue in the selected
`github.com/Azure/go-ntlmssp` version. A new hosted Go resolver output selects
v0.1.1. The parent verified that this is the only changed module selection,
with the nested API and archived baseline bytes unchanged; the previous
qualified artifact remains retained separately. Checksums were generated by
Go, not transcribed. The upstream overflow regression and package race suite
must execute on the new candidate.

GO-2026-5932 concerns the `golang.org/x/crypto/openpgp` package and its
subpackages, not every use of the crypto module. The build now retains its
actual `go list -deps` inventory and rejects those package paths. That guard
must execute and its evidence must be reviewed before resolving the note.
There is no module-wide scanner exception or unsupported fixed-version claim.

The prior binary/runtime evidence does not qualify this changed dependency
selection. Fresh source review and all current-head build/runtime/security
requirements remain mandatory; this follow-up does not itself approve merge.

This uses the maintainer's explicit
[publication-review delegation](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
The implementation, independent AI review and parent composition are disclosed
as such, not two human reviews. Accept this bounded source for qualification;
all current-head technical/security and deployment gates remain required.

Signed-off-by: pallakatos (author source attestation through explicit maintainer-delegated AI review, not a claim of personal code review) <191481949+pallakatos@users.noreply.github.com>
Signed-off-by: GitHub Copilot (independent-context delegated AI source review, not a second human) <223556219+Copilot@users.noreply.github.com>
