<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security audits

Lightweight, per-change security review records. The `security-audit-required`
CI gate (`ci/security-audit-required.sh`) requires any PR that touches a
**capability-introducing** path to add one
`docs/security-audits/<YYYY-MM-DD>-<slug>.md` here, signed off by two distinct
people (author + an independent reviewer).

Capability paths (see the gate for the exact regex): controller CRD/reconcilers/
admission, inference-router mcp/a2a/providers/routes, CLI commands/migrate/
adapters, OpenClaw runtime core, the shared mesh plugin and OpenClaw runtime
`package-lock.json` files, sandbox-image Dockerfiles/entrypoints, seccomp
profiles, and bundled Helm files. Test files are exempt.

## How to add one

1. Copy [`_template.md`](_template.md) to `YYYY-MM-DD-<slug>.md`.
2. Fill in the threat triage (T1 new surface? T2 control change? T3 availability?)
   and a short verdict.
3. End with two `Signed-off-by:` lines using real emails (author + reviewer).

Add a **new record for the current change scope**. Modifying or renaming an
older signed audit does not extend its approval to new capabilities. Identify
the reviewed source head and base, distinguish source review from executed
qualification, and retain unresolved findings and failed-run evidence honestly.
The gate rejects an unavailable review base rather than checking an unrelated
worktree diff.

The gate checks record presence and distinct signer emails; it does not
authenticate identities or verify that the prose covers the changed source.
Reviewers must check those facts. An explicitly maintainer-authorized delegation
must cite that authorization and disclose its actual participants and limits
in the record, never imply that AI review was a second human review or that
technical gates were waived. No delegation is inferred by default.

## Source-bound beta approval exceptions

For Azure/kars#593 only, maintainer `pallakatos` explicitly authorized a
single-maintainer approval of source `28ff64372ca97e6bdd6db833d68afa90239cde7c`
against base `b2ec2094cc663d5b747fec7f3c9d37cce667a24a`. The authorization and
one genuine sign-off are recorded in
[`2026-11-04-beta-publication.md`](2026-11-04-beta-publication.md).
**No second human sign-off is claimed.** This is an exception to, not fulfillment
or replacement of, the normal author-plus-independent-reviewer policy.

Separately, for Azure/kars#606, the maintainer explicitly authorized the combined
OverlayFS and Grafana build repair at source
`187ada4b45203bc831e7976961ffcb2f92160886` against integration base
`ab78d11d66b626b4a176603643011ebc1cca1dff`. Its current-PR record is
[`2026-11-10-overlay-package-update.md`](2026-11-10-overlay-package-update.md).
This additional approval does not repoint or extend the older PR593 approval.

For Azure/kars#607, the maintainer explicitly renewed the single-maintainer
sign-off of interactive local-Dex onboarding and the six-file Task credential
status-race repair at source `a4333b355a03648cf3769faf4eca26e988700aeb`
against integration base `97789571866ca2c181f84455a845b957427cd991`.
The user selected **“Authorize this repair and exact-source sign-off”** for the
frozen repair patch `cdd252c75f18a4655ae25bb9898423f6e1a890d6b022e5d1181c76d25eb64201`.
This is new authorization, not inherited approval from the earlier
`650141e5ad4bdf68a1c02e7a0ecae202fde10b3b` scope. Its current record is
[`2026-11-10-kind-local-onboarding.md`](2026-11-10-kind-local-onboarding.md).
This decision replaces only the two-human sign-off requirement for that exact
scope; technical CI, artifact signing and fresh-cluster acceptance remain
required. Neither the PR593 nor PR606 exception is changed or extended.

For Azure/kars#608, the maintainer explicitly authorized publishing the additional
three-file Headlamp Go 1.27.4 patch and recording a source-bound single-maintainer
sign-off, selecting **“Publish fix with my source-bound sign-off (Recommended)”**.
The frozen patch is
`c7f4bf780d9ef403134b5f2c8ea72581c20fe7c66be403591c24fd46b8b23df6`.
The maintainer subsequently selected **“Publish the test correction and update my sign-off (Recommended)”**
to include the missed CLI assertion for that same verified builder version and digest.
The updated approved source `bec8d459b360ee6c9d5416b313df8a356195bbd7` against base
`2068be5faab76afeb1fa51afb7ebd20fd8177530` includes the previously authorized
native release-runner repair, unchanged Headlamp patch and this one-line test correction.
All 68 focused Headlamp CLI tests pass. Its current record is
[`2026-11-11-headlamp-go-release-repair.md`](2026-11-11-headlamp-go-release-repair.md).
This decision does not waive the shipping-image vulnerability scan or any other
technical gate, and does not extend or modify the three earlier approvals.

For Azure/kars#617, the maintainer selected **“Publish this repair via gh and update my sign-off (Recommended)”**
to authorize the two-file OpenClaw SDK comparison repair, patch SHA-256
`25feb4ad40ce8574593ad92727b3b1a94b21fd9a8398505c5de4204252e76335`.
The exact source is `234b63e60f231f1c80286efaf9d2d44c91854f79` against base
`e5db80c4e761cb5a92eb097b98713ba43d3d9394`. The bundled Node.js replaces the
unavailable `cmp` utility while preserving exact SDK byte equality on AL3.
Its separate record is [`2026-11-12-openclaw-sdk-compare.md`](2026-11-12-openclaw-sdk-compare.md).
No previous approval is extended and no technical gate is waived.

For Azure/kars#621, the maintainer separately selected **“Publish this repair via gh and update my sign-off (Recommended)”**
to authorize the two-file Playwright native release-build repair at source
`efadcd4932c08844a94e62080e45b6256c29a58a` against base
`01548f2c365a94ca2016987f03a496a237ea46ff`. The release workflow reuses native
amd64/arm64 builds and the existing signed merged-manifest receipt path, without
changing AL3, dependencies or image pins. Its separate record is
[`2026-11-12-playwright-native-release.md`](2026-11-12-playwright-native-release.md).
Because this source changes only a workflow and its tests, the normal capability
path filter does not require an audit. Adding this specific voluntary record
nevertheless invokes its source-bound checks; later historical record edits do
not reopen it. Earlier approvals remain unchanged. Offline tests are not native
container qualification, and no branch-protection change is authorized.

For Azure/kars#622, the maintainer selected **“Publish, sign off, merge after checks, and release; allow the temporary review-only exception if needed”**
to authorize the four-file OpenClaw npm registry repair at source
`28ca292c01bbd4310af90563fc2496281fce7141` against integration base
`af5ff64796898060f8f9d4a9dc853013f1d51981`. Its separate record is
[`2026-11-12-openclaw-npm-registry.md`](2026-11-12-openclaw-npm-registry.md).
Only 131 registry URL prefixes change across the two dependency locks; dependency
versions, integrity hashes, vendored AGT archives and lifecycle restrictions remain
unchanged. Those two exact lockfile paths now use the ordinary capability gate,
not the workflow-only voluntary-record handling for PR621. This is an explicitly
authorized maintainer sign-off, not independent human or security review. Only if
normal merging refuses for review requirements may the publisher use the user's
temporary review-only exception for this PR; restore full protection immediately.
Required checks, scans, signed-release verification and fresh-Kind acceptance are
not waived. This permission changes no earlier approval or repository protection
by itself; the gate does not implement a branch-protection bypass.

For Azure/kars#623, the maintainer selected **“Authorize publication, sign-off, merge and release, including the conditional reviewer-only exception”**
to authorize the successor retaining PR622's npm repair plus the tested three-file
Headlamp evidence-permission fix at source
`15281ad70925895f3f7c646b6503a2b1f6bd90d1` against integration base
`af5ff64796898060f8f9d4a9dc853013f1d51981`. Its separate record is
[`2026-11-12-headlamp-evidence-permissions.md`](2026-11-12-headlamp-evidence-permissions.md).
The existing ordinary capability rule already covers the Headlamp Dockerfile;
no additional workflow-only exception or capability-pattern change is needed.
The repair stages the actual frontend evidence files with mode 0444 in a mode
0755 directory, preserving root ownership and non-root read-only access. Scans,
hash binding, dependency/runtime/base/toolchain pins, authentication and isolation
are unchanged. Local tests do not establish fresh image extraction or Kind acceptance.

PR623 retains the immutable PR622 record from parent
`88b7aeb4bd9856107f03227a4734f053b44e78e8`; it is still a new file relative to their
common integration base. The gate permits only that exact historical record to
be carried by the separately validated, newly added PR623 approval. The successor's
source comparison includes the historical record, so any change to it fails; a
missing or invalid successor marker/signature, changed source/base/ancestry,
another PR context or another unsigned record still fails. This does not repoint
PR622 or make its sign-off authorize PR623. All seven earlier cases and records
remain unchanged. This is authorized maintainer sign-off, not independent human
or security review. Only if normal merging refuses for review requirements may
the publisher use the conditional reviewer-only exception for PR623, restoring
full protection immediately. Technical checks, scans, signed-release verification
and fresh-Kind customer acceptance are not waived. The record itself does not
publish, merge, release or change branch protection.

For the additional private-TLS repair on the **same Azure/kars#623**, the maintainer
selected **“Authorize TLS repair publication, sign-off, merge and release (Recommended)”**
at `2026-11-12T20:37:08.951024+00:00`. This separately authorizes source
`acb9d9a2ccef87003830fa10c2cc3eb08e5129f9` against the unchanged base
`af5ff64796898060f8f9d4a9dc853013f1d51981`, with published PR623 head
`4afec6aee6b85a5c4d5476c55590d77f67fdfc41` as its parent. The implementation
patch SHA-256 is `cc6d5703fd108037792d01ff77138c5e888b4c1fa7b7fea8b994a65d3f8e5166`.
Its new record is
[`2026-11-12-private-tls-authority-key-identifier.md`](2026-11-12-private-tls-authority-key-identifier.md).
The two-file repair adds the issuer authority key identifier to private TLS leaf
certificates, real strict OpenSSL verification coverage, and prerequisite
documentation. It does not disable certificate, hostname or issuer verification.
Local strict OpenSSL and unchanged Hermes httpx-client checks passed; the hosted
Kind failure's cause is not yet confirmed. Existing certificates require normal
rotation; the issuance fix targets fresh Kind certificates, not an automatic
repair of already issued certificates or proof of hosted acceptance.

The new TLS approval may carry only the unchanged PR622 npm and earlier PR623
Headlamp records. Both remain included in the exact TLS source comparison; only
the new record and the three policy/test/documentation paths are excluded.
Missing or invalid TLS approval, source/base/ancestry/context drift, edits to
either historical record, and other unsigned scopes fail closed. The old exact
Headlamp approval path remains valid for its old unchanged source. All eight
earlier cases and records remain immutable; neither old sign-off authorizes TLS.
No new capability-pattern or workflow-only exemption is introduced. This is one
genuinely authorized maintainer decision, not a fictional independent reviewer.
Normal merge must be attempted first; the separately authorized reviewer-only
exception is conditional on a review-only refusal and immediate full protection
restoration. Required CI, scans, signed-release checks and fresh Kind customer
acceptance remain required. Preparing this record does not publish, merge,
release or alter protections.

For each exception, the gate requires its exact committed record, signer and source/base marker,
the exact review base, and the approved source as an ancestor. Every tracked
file must match that source except the four explicitly reviewed approval-plumbing
paths: the gate, its regression test, this README and the approval record.
GitHub CI additionally restricts each exception to its specified Azure/kars PR merge
or its unchanged integration on `kars-bridge`. The other exception's audit record
is not excluded from the source comparison. Synthetic merge commits are allowed;
changed base, implementation, workflows, unrelated documents or new files are not.
There is no configurable source SHA, signer or waiver environment variable.

Future capability changes still need a new record with two distinct human
signers. The gate exception itself grants neither merge/release authorization nor technical
acceptance: all technical gates, signed-release checks and fresh-system customer
journeys remain required. Remove the one-time exception when this beta approval
no longer needs to be evaluated; do not repoint it at a later source revision.

These docs are intentionally **tracked** (committed with the PR), unlike the
private `docs/internal/` planning folder.

The record must be newly added relative to the reviewed base. Modifying or
renaming an old signed record does not approve a new capability. Formatting or
copyright changes to historical records neither extend their sign-off scope nor
reopen their completed approval decisions. A new capability still requires its
own newly added, signed record, and an unavailable review base fails the gate.
