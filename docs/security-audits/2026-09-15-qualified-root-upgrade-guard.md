<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Qualified-root upgrade guard - bounded delegated source review

Base: `3bc7ff58d8d2994e474be39fd2af201d750fc5c2`.
Initial source: `34572935d762a94248d4142c4a9ef9c82eea1ce6`.
Reviewed corrective source: `04045245eb777f8e2ac3c13182788315fa55d0dd`.
Unchanged twenty-file composition: `f343d86fe1734f0663b99ac8561f9ed9937d5355`.

Status: **Scoped source-approved; final combined-head qualification required.**
This guard does not implement the root-template migration tracked in
[Azure/kars#567](https://github.com/Azure/kars/issues/567).

## T1: New capability or attack surface?

The CLI adds a typed preflight for existing controller-changing operations.
It does not add a CRD, credential authority, migration envelope, privileged
BFF endpoint or infrastructure provisioning capability. Existing root state is
read before schema, Helm, image-publication and restart effects.

The scope covers the changed up/upgrade/fast-upgrade/push/apply paths, shared
schema/image/rollout helpers, their tests and operation documentation.
Read-only existing-cluster discovery is not permission to change Azure
networking, IAM, instance counts or SKUs.

## T2: Security-control change?

Qualified, in-progress or malformed private proof blocks a controller-changing
operation rather than permitting a healthy rollout to conceal broken credential
continuity. Unavailable state is not interpreted as an unqualified root.
Build-to-publication paths recheck before publishing mutable controller inputs.

Fresh/unqualified installations, read-only verification, genuinely root-free
operations and original credential recovery remain available. A sandbox-only
apply is not root-free if it changes controller defaults and restarts the
controller. No proof reset, binding replacement or grant deletion is introduced.

This is client-side preflight, not an atomic cross-operator reservation.
Completing enrollment still does not authorize changing a sealed root template.
Direct Helm/Kubernetes changes cannot be treated as a supported bypass.

## T3: Finding, repair and validation

The independent review found that two unconditional mutation checks also
blocked protected-root read-only schema verification. `04045245` skips those
checks only for `checkOnly`, retaining schema, ownership, publication and
server-render verification. Mutating schema preparation remains guarded.

The parent reproduced the finding with an executable failing regression.
After repair, 162 targeted tests, CLI typecheck/build and focused lint passed.
The implementation owner had separately reported 618 tests over 22 files.
On the combined candidate the parent ran 733 tests over 23 relevant lifecycle,
schema, image and command files, plus typecheck/build and focused lint.

The independent reviewer closed the original finding and the bounded combined
guard source. Three in-memory probes independently confirmed protected read-only
success, mutation refusal and unstaged-schema refusal, all without writes.
That review did not independently run the complete suites or a live upgrade.

Local dependency caches used YAML 2.9.0 rather than locked 2.8.3. Hosted
locked-dependency qualification remains required. No protected cluster template,
proof or grant was changed to demonstrate this guard.

## Delegation and verdict

The independent review found no remaining significant issue in its bounded
scope. The parent checked that all twenty source files in the composition
exactly match the reviewed corrective head.

The maintainer's explicit
[publication-review delegation](https://github.com/Azure/kars/pull/551#issuecomment-5615522306)
authorizes honestly disclosed independent-context AI attestation, not invented
human review or waiver of technical gates.

Verdict: accept this safety guard for final-head qualification, not as approval
of root-template migration or beta deployment.

Signed-off-by: pallakatos (author source attestation through explicit maintainer-delegated AI review, not a claim of personal code review) <191481949+pallakatos@users.noreply.github.com>
Signed-off-by: GitHub Copilot (independent-context delegated AI source review, not a second human) <223556219+Copilot@users.noreply.github.com>
