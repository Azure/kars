<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Private-consumer admission evidence - bounded delegated source review

QoS review: `3d1ea2ba996d534c5f3cf19cf9ec477e6be6b736` through
`25a52f4c4aae1d55bd65a4e35fbf23b16f2f59de`.
Diagnostic review: `4255ee6c926c98eeef93d8ce9d35e89765a9e366` through
`8933e29f3c2a4c2939a76d156cdbcb33f8a93c06`.

Status: **Bounded source reviews closed; the corrected native path still requires acceptance.**
No source review or local fixture result makes a failed native run successful.

## T1: New capability or attack surface?

The four-file QoS repair shares the existing root admission eligibility rule
with ordinary private-consumer execution comparison. Positive reviewed CPU or
memory requests/limits can account for one exact memory-pressure toleration;
GPU-only, zero and ephemeral-container resources cannot establish eligibility.
Explicit/wildcard policies, duplicates and altered tolerations remain exact.

No non-root consumer gains the AKS root-only admission fallback. Root namespace,
ServiceAccount and Deployment identities, template digest, strict server dry-run,
snapshot fences and retirement checks remain required.

The later five-file diagnostic change adds failure-only, fixed boolean evidence,
not a new authority or mutation. It reuses the enforcement normalization and
reports fourteen closed comparison fields. Names, environment values, images,
credentials, hashes and arbitrary object keys are not emitted.

## T2: Security-control change?

Execution mismatches remain refusals. The diagnostic path rethrows the original
error, including typed command failures; it cannot authorize a rejected Pod.
Successful admission does not emit a failure diagnostic.

The native harness accepts only one bounded line with the exact fourteen fields,
all booleans. Duplicate keys/lines, extra or missing fields, nonboolean values and
malformed payloads become unavailable evidence, not trusted or echoed data.

The shared normalization body was independently compared with its predecessor;
the diagnostic extraction changes its return form, not the matching rules.
Missing and null fields remain distinct.

## T3: Observed failure, reproduction and limits

[Native run 34992902236](https://github.com/Azure/kars/actions/runs/34992902236)
at `3d1ea2ba` passed the initial controller and intervening lifecycle cases, then
failed private-observer preview with `root-pod-admission-identity`. The retained
artifact does not contain the full rejected Pod execution specification.

The parent reproduced that same category for a reviewed non-root consumer with
an exact QoS memory-pressure toleration. Five regressions failed before the QoS
repair and 399 targeted tests passed afterward. The independent reviewer ran
71 focused in-memory assertions and closed that four-file source delta.

Kubernetes v1.31's `PodTolerationRestriction` plugin can inject this toleration,
but the plugin is not default-enabled. This is therefore **not** a claim of
confirmed default-Kind injection or the established cause of the failed run.
That review required later native comparison evidence; boolean differences
alone cannot prove which admission plugin ran. The follow-up evidence and
root-selection repair are recorded below.

The diagnostic prototype initially failed existing cases because the strict
canonicalizer does not accept absent values. The final implementation selects
present fields rather than inventing defaults or catching away the original
failure. All 400 targeted CLI tests and 15 native diagnostic tests then passed,
along with typecheck/build and focused lint.

The independent diagnostic review ran the real Python subprocess/redaction test
and sixteen in-memory probes, including original typed-error identity,
success-path silence and missing/null distinctions. It found no significant
issue in the exact five-file delta. Its later-head read confirmed those files
were unchanged; no broad assembly review is implied.

The parent subsequently validated the combined candidate with 733 CLI tests
over 23 relevant files and 94 web tests, plus the producer/chart/parity and
readiness/diagnostic suites. Local YAML 2.9.0 and Next 16.2.9 caches differ from
locked 2.8.3 and 16.3.3, respectively. Local Rust execution and native cluster
qualification remain unclaimed.

## Delegation and verdict

### Observed root-selection defect and corrective review

[Native run 35023001678](https://github.com/Azure/kars/actions/runs/35023001678)
at `e6908eb5354cfb8c6b372d2291e57c470f28530b` supplied the missing evidence:
the observer-preview rejection reported false root namespace/Deployment
identity but **all twelve normalized execution sections matched**.
Fourteen other runtime cases and all three cold API lanes passed; the native
aggregate nevertheless failed.

This isolated the forced Workload Identity opt-in selector, which treated any
opted-in Pod as requiring the root-only verifier. It was not a QoS execution
difference. The parent reproduced the error for exact non-root consumers both
inside and outside the root namespace before repairing the selection.

The initial four-file correction `0e194cf95957bd96a3f49df7c593c4a3a45303aa`
restricted early replay to the pinned root ReplicaSet lineage. Independent
review then found a high-severity direct Pod-to-root-Deployment shortcut that
could return an approved root without mandatory WI verification. That source
was not published as the PR head or deployed.

Corrective `514f3e79672c47480ca0ce15ea45b730e2123144` additionally enforces
mandatory replay before returning any approved, opted-in pinned root
Deployment, irrespective of traversal. The original strict verifier rejects
unsupported direct lineage. Early ReplicaSet enforcement remains, as do exact
non-root matching and refusal of actual injected execution changes.

The direct-owner regression failed before repair. Afterward, 474 targeted CLI
tests, typecheck/build and focused lint passed, including both preview and
apply rejection without writes. The independent reviewer closed the high
finding and the bounded combined WI-selection changes after thirteen in-memory
scenarios covering direct roots, explicit root-RS shortcuts, identity changes,
altered RS opt-in, missing root admission and non-root positive/negative cases.
The intervening Copilot change was excluded from that review.

Fresh native acceptance is still required. These closures do not relabel either
failed native run, weaken the verifier or imply a completed live enrollment.
The final local join passed 736 CLI tests over 23 relevant files, plus
typecheck/build, focused lint and fifteen native diagnostic tests. These
provisional-cache results do not replace final-head hosted qualification.

The independent AI context closed each bounded source delta separately. The
parent owns implementation, composition and combined validation.
Attestation follows the maintainer's explicit
[publication-review delegation](https://github.com/Azure/kars/pull/551#issuecomment-5615522306),
not a claim of a second human reviewer or permission to waive failing checks.

Verdict: accept the source repair and diagnostic collection for exact-head
qualification. Do not merge or deploy on the strength of the reproduced category
alone; retain all native, CodeQL and branch requirements.

Signed-off-by: pallakatos (author source attestation through explicit maintainer-delegated AI review, not a claim of personal code review) <191481949+pallakatos@users.noreply.github.com>
Signed-off-by: GitHub Copilot (independent-context delegated AI source review, not a second human) <223556219+Copilot@users.noreply.github.com>
