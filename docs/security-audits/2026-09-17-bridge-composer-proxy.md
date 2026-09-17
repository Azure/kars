<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Bridge composer proxy and model catalogs - delegated source review

Date: 2026-09-17.
Base: `3a6c81564cf2a8a5a201b0cbe63e000738d70133`.
Initial reviewed source: `8a6ab1f9c05bc380439c3382da71dc69da4ccf5f`.
Corrective source: `cbb26ea4ffbf39f36247a5af86c75da27e84245b`.

Status: **Scoped source-approved for final-head qualification, not merge or
deployment approval.**

Scope: the Bridge model catalog reader and both callers; the optional
orchestrator proxy Helm rule, policy-only configuration command, related
Python/Rust/Kind tests, CI registration and installation documentation.

## T1: New capability / attack surface? YES

AKS Kubernetes proxy traffic reaches the router from konnectivity Pods, not
the BFF namespace. Actual Cilium policy-denied drops confirmed that mismatch.
The optional additive rule permits `app=konnectivity-agent` Pods AND the exact
`kube-system` namespace to only the `bridge-orchestrator` Sandbox Pods in its
existing runtime namespace on TCP 8443. It does not permit gateway ports,
other sandboxes, arbitrary kube-system Pods, or additional egress.

This is an L4 allowance, not an HTTP-path filter. The BFF retains its existing
Kubernetes-authorized, fixed inference paths; the router retains its own
authentication/governance. No direct transport fallback, provider credential,
BFF NetworkPolicy mutation privilege or second mesh provider is introduced.

## T2: Security-control change? YES, BOUNDED OPT-IN

Live Helm lookup checks namespace/Sandbox UIDs, namespace ownership claim and
Bridge labels, and refuses absent, replaced, terminating or mismatched targets.
Only the reviewed non-hostNetwork AKS proxy selector is accepted. Offline
rendering with the option enabled cannot fabricate target ownership.
Old/default values keep the allowance off.

The configuration command reads the original private Helm user values,
including null deletions. Using computed `--all` values would restore deleted
OIDC role defaults on a fresh overlay; an actual read-only preflight found
that difference, which was corrected before any deployment. Values remain
in a mode-0600 temporary file and are neither arguments nor printed diagnostics.

The command requires an exact policy-only manifest delta, server dry-run,
unchanged release revision/manifest and live resource fences. Helm owns the
rule and removes it on disable/uninstall; the namespace, baseline sandbox
policy, core resources and grants are retained.

## T3: Availability / fail-open risk and retained finding

The initial independent review identified a High-severity correctness gap:
identical stored manifests do not stop Helm from reverting a live drifted
BFF image/replica count or recreating a missing workload. A successful Helm
dry-run does not establish a policy-only live effect.

The corrective source checks all non-policy live resources against every
declared field, rejects missing/terminating resources, and rechecks complete
UID/resourceVersion snapshots immediately before mutation. Only normal API
Secret stringData/data encoding is normalized. Post-operation configuration
comparison retains UIDs, generation, templates, metadata and data, excluding
only status, resourceVersion and managedFields.

These are client-side fences, not an atomic multi-object lock. Operators must
not make concurrent changes. Unexpected drift or an ambiguous upgrade response
is an explicit failure, without automatic retry, rollback or an invented
qualification result. The focused independent correction review found no
remaining significant issue in the reviewed changes, closing the live-drift
finding for this bounded, non-concurrent operation.

JSON string-array and legacy CSV model catalogs are supported. Invalid arrays
raise an error through both consumers rather than publishing partial/quoted
deployment names. Default precedence and unset-default behavior are preserved.

## Verification and rollout limits

Thirteen local Python cases pass, including actual Helm lookup against a
loopback API and mocked command-boundary failures. Helm lint, Rust formatting
and diff checks pass. No local Rust build or npm network install was performed.
Initial-source hosted BFF build/test and add-on installation/removal passed.
The new corrective native drift/missing-Deployment cases have not yet executed.
All final-head required checks remain mandatory.

Read-only preflight against the actual AKS release passed with the new live
fences. No proxy rule, workload, credential or namespace was changed. The
eventual health probe exercises `pods/proxy`; it does not prove BFF authorization,
model inference or authenticated Home composition.

The existing private retirement proof seals BOTH BFF and controller templates.
A new BFF image cannot be installed through an ordinary Helm upgrade while
claiming qualification continuity. The parser rollout remains blocked on the
supported consumer-template migration tracked by
[Azure/kars#567](https://github.com/Azure/kars/issues/567).
No sealed proof, epoch or controller template is changed by this repair.

## Delegation and verdict

The maintainer explicitly authorized focused-agent review and subsequent
[delegated publication sign-off](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
The initial independent AI review found the live-drift issue above. A separate
focused correction review examined `8a6ab1f9..cbb26ea4` and found no significant
remaining issues. It was necessary because the first synchronous review context
could not receive follow-up messages. Neither context is represented as a second
human reviewer, and technical gates are not waived.

Verdict: **accept the bounded corrective source for final-head qualification**.
This does not qualify runtime publication, the sealed BFF replacement, or
the customer's full Home-to-maintenance-team workflow.

Signed-off-by: pallakatos (author source attestation through explicit maintainer-delegated AI review, not a claim of personal code review) <191481949+pallakatos@users.noreply.github.com>
Signed-off-by: GitHub Copilot (independent-context delegated AI source review, not a second human) <223556219+Copilot@users.noreply.github.com>
