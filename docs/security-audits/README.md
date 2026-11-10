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
adapters, OpenClaw runtime core, sandbox-image Dockerfiles/entrypoints, seccomp
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

For Azure/kars#607, the maintainer explicitly authorized a single-maintainer
sign-off of interactive local-Dex onboarding at source
`650141e5ad4bdf68a1c02e7a0ecae202fde10b3b` against integration base
`97789571866ca2c181f84455a845b957427cd991`. Its new record is
[`2026-11-10-kind-local-onboarding.md`](2026-11-10-kind-local-onboarding.md).
This decision replaces only the two-human sign-off requirement for that exact
scope; technical CI, artifact signing and fresh-cluster acceptance remain
required. Neither earlier exception is changed or extended.

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
signers. The exception grants neither merge/release authorization nor technical
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
