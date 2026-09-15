<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Writer rollout revision settling - bounded delegated source review

Base: `976534af1ddbd92dec15584da19694650f743754`.
Reviewed source: `81bb7b24a90f47d1c9f9e0969f65cd61d81aa117`.

Status: **Scoped source-approved; actual combined-head native qualification is required.**
This is a new review, not an extension of the earlier beta setup audit.

## Failure and scope

[Native run 34983443416](https://github.com/Azure/kars/actions/runs/34983443416)
failed the private-observer case on MCP-only source `a9a35bba`. Its preserved
diagnostics report a Deployment revision mismatch while generation, projection,
template, replica and retirement-witness comparisons matched. The boolean
diagnostics do not disclose whether that revision was old or otherwise
unexpected; this record does not claim the exact failure has already been fixed.

The investigation reproduced a legitimate ordering that the CLI rejected:
Kubernetes `Recreate` can advance `observedGeneration` before creating the new
ReplicaSet and publishing its rollout revision. See the upstream
[Recreate controller](https://github.com/kubernetes/kubernetes/blob/v1.31.0/pkg/controller/deployment/recreate.go)
and [revision synchronization](https://github.com/kubernetes/kubernetes/blob/v1.31.0/pkg/controller/deployment/sync.go).

The change is limited to `cli/src/lib/private-activation-writer-settle.ts`,
its regression module and the corresponding credential-enrollment guide.

## Control and availability analysis

There is no new endpoint, permission, credential format or mutation. The exact
restoration template with its captured old rollout revision may remain pending
even when Kubernetes has observed the new generation. This is not permission
to complete retirement with a stale revision.

Completion still uses the strict restored-shape predicate: a changed projection
requires the exact next rollout revision, alongside current grant/Task/Sandbox
authority, unchanged identities and intent, witnessed pause/revoke/refill,
fresh consumed projection, valid lineage and old-Pod retirement. Arbitrary
template changes and missing, malformed, lower or unexpected revisions fail.

Observation remains read-only and bounded by the original 120-second deadline.
Waiting does not reset that deadline, publish activation, invent witnesses or
replay writes. Caller role-absence and activation revalidation remain unchanged.
The repair reduces false rejection without converting an incomplete rollout
into readiness. It does not enable qualified-root template upgrades.

## Executed verification and independent closure

Two new cases failed before the implementation change: old revision with current
observed generation, and preservation of the deadline's explicit failure.
Afterward, 233 targeted cases covering writer settling, private continuity,
AKS admission and credential-grant commands passed, as did CLI types, build and
focused lint. Nine new cases cover exact-next completion, permanently old
revision, malformed/missing/unexpected revisions and template drift.

The independent core AI review context examined the complete three-file delta
and directly relevant callers and found no high-confidence blocker. It verified
that the pending predicate cannot replace strict completion. That reviewer did
not execute tests or contact a cluster; execution above is parent evidence.

Local YAML was cached 2.9.0 rather than locked 2.8.3. Hosted locked-dependency
execution and the full native lane on the final combined head remain mandatory.
The earlier native failure is retained, not waived or relabeled as success.
No matching-source image deployment or H100 acceptance is asserted.

## Delegation and verdict

The parent implemented this repair; a separate AI review context provided the
bounded closure. The maintainer's explicit
[publication-review delegation](https://github.com/Azure/kars/pull/551#issuecomment-5615522306)
authorizes the disclosed source attestations below, not a claim of a second
human review, personal maintainer inspection, or technical-gate waiver.

Verdict: accept the bounded source repair, subject to actual final-head
qualification and the separate existing deployment constraints.

Signed-off-by: pallakatos (author source attestation through explicit maintainer-delegated AI review, not a claim of personal code review) <191481949+pallakatos@users.noreply.github.com>
Signed-off-by: GitHub Copilot (independent-context delegated AI source review, not a second human) <223556219+Copilot@users.noreply.github.com>
