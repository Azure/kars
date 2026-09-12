# Evaluator evidence parity - bounded delegated source approval

Date: 2026-09-11
Status: **Source-approved under explicit maintainer delegation; not deployment approval.**

## Exact scope

Source head: `b5c5d5435557b943a34eeeea08cbf3522cfb7b6c`

Integration base: `d1535e53b99604dcaca11b44e6b3e99dd8238eba`

This record covers Azure/kars#562's evaluator/conformance result contract:
runner transport/outcome/report handling, controller Job/Pod/report attribution
and retention, requested-run and scheduled-run transitions, bounded history and
drift conditions, CLI result presentation, CRD status additions, and extraction
of the existing shared MCP content parser. It does not approve the Bridge
application, credential/private-activation work, or a combined release.

The older `2026-09-10-evaluator-runner-contract.md` approves only its recorded
runner-launch compatibility source. Its existing signatures do not approve this
larger result-attribution scope. New implementation notes in that older document
are history, not an extension of its sign-off.

## Authorization and reviewer identity

The maintainer's explicit delegation is recorded in
[comment 5615522306](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
It authorizes publication sign-offs after additional focused-agent review rounds
and requires blocking findings to be repaired and re-reviewed, without fabricating
human review or waiving technical gates.

The author attestation below is exercised by Copilot under that maintainer
delegation. The independent review was performed in a separate read-only AI
context (`public-evaluator-contract-review`), not by a second human. The
implementation context (`evaluator-attribution-fixes`) did not approve its own
corrections. This disclosure is part of the approval, not a claim that automated
checks authenticate sign-off identities.

## Findings and source closure

The initial full exact-ref review found three defects:

- Fractional report completion could be rejected against Kubernetes'
  whole-second container termination timestamps.
- A missing newer requested Job could borrow older Job success and incorrectly
  associate it with the current request.
- Merge-patching a singleton Degraded condition could erase unrelated Sandbox
  conditions.

The corrections preserve API timestamp precision without accepting reports in
the next second; bind evidence to actual producer/request identities and current
intent; and preserve unrelated conditions under UID/resourceVersion fences.
Condition transition times, observed generation and unique Degraded entries are
covered explicitly.

The first closure review then found that a completed historical request could
block fresh scheduled reports after intent changes. The final correction
separates authenticated historical fulfillment from current qualification:
history may release an already-fulfilled request boundary, but cannot become a
current result. New reports still require their own current Job/CronJob/Pod,
specification, corpus, timestamp, exit and request evidence. Surviving historical
Jobs are rechecked before observation and publication.

The final exact `600f311d` through `b5c5d543` closure review found no remaining
issue in that residual scope. The original timestamp, request-attribution and
condition-preservation closures remain intact. Cap-only module extractions
preserve the inspected implementation and test bodies.

## Execution evidence

Exact source `b5c5d543` passed public
[CI run 34646156486](https://github.com/Azure/kars/actions/runs/34646156486),
including strict Rust lint/build and workspace tests, CLI validation, native
Kind acceptance, schema/admission checks, benchmark compilation and chaos jobs.
Its CodeQL and other technical checks passed as well.

The added regression definitions comprise two timestamp cases, fourteen
request/receipt/scheduling cases, two condition-merge cases and ten intent
transition cases. Existing tests remain registered. This is not a claim of
twenty-eight independent live clusters or twenty-eight new native scenarios.

Earlier `55c2f0f` failed Clippy because two timestamp tests were nested; that
failure is retained. `600f311d` corrected registration and passed its own full
technical run before the final transition correction was published. Local
formatting and metadata checks were never substituted for Rust execution.

The source audit presence check previously accepted the older modified signed
record. That automated result is not this approval's basis: this new record
binds the actual reviewed source and discloses the delegation explicitly.

## Limits

No new permission or trust downgrade is approved. Unattributed, incomplete,
failed, stale or replaced evidence must not become Ready or confirmed drift.
Retained history is not current evidence merely because it once passed.

This source approval does not authorize integration-branch bypasses, main
promotion, image publication, customer/H100 deployment, private repository
visibility changes, or release readiness. The current PR head and any combined
integration head still require their applicable protected checks and review.

Signed-off-by: pallakatos (author source attestation through explicit maintainer-delegated AI review, not a claim of personal code review) <191481949+pallakatos@users.noreply.github.com>
Signed-off-by: GitHub Copilot (independent-context delegated AI source review, not a second human) <223556219+Copilot@users.noreply.github.com>
