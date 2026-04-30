# Phase 2 — S7.F: Content-Safety floor admission policy

**Date:** 2026-04-29
**Slice:** `phase2-content-safety-floor` (sub-slice S7.F of S7 craftsmanship train)
**Author:** AzureClaw maintainers
**Sign-offs:** `@maintainer-1`, `@maintainer-2`

## Scope

Add a ValidatingAdmissionPolicy that enforces a *cluster-wide
minimum* on `InferencePolicy.spec.contentSafety.*` severity floors.
Authors of an `InferencePolicy` CR can no longer set a severity
*more permissive* than the cluster operator's minimum. This is a
runtime enforcement of §10.4 #4 ("VAP/MAP expansion beyond the
Phase 1 core set") that complements the controller's existing
`inference_policy_compile` enum validation: the VAP rejects at API
admission so out-of-policy CRs never land in etcd, while the
controller's compile pass keeps protecting the read path for older
objects and the `azureclaw push` flow.

- New template
  `deploy/helm/azureclaw/templates/admission-content-safety-floor.yaml`.
- New Helm values:
  - `admission.contentSafetyFloor.enabled` — default `true`.
  - `admission.contentSafetyFloor.minimum` — default `"Medium"`,
    must be one of `Safe|Low|Medium|High`. Helm fails fast with
    a clear message if a different string is provided.
- Per-CR opt-out: `azureclaw.azure.com/dev-only: "true"` label,
  matching the existing null-provider-block VAP convention so a
  single label flips a CR into "non-prod" posture across all
  admission policies.

Severity ordering follows Azure Content Safety:
`Safe (0) < Low (1) < Medium (2) < High (3)`. Lower ordinal == stricter
floor (block more content). An InferencePolicy with
`spec.contentSafety.hate: High` would block only the *worst* hate
content, which is more permissive than `Medium` and is therefore
rejected when the cluster floor is `Medium`.

## Out of scope

- **Per-namespace floor overrides.** A more granular floor (e.g.,
  prod namespaces=`Low`, dev namespaces=`High`) requires a second
  binding + a NamespaceSelector. Deferable; today's binding
  `namespaceSelector: {}` applies cluster-wide and the dev-only
  label is the documented bypass. Future slice if operator demand
  warrants.
- **`requirePromptShields` admission** — the boolean field is
  validated by the controller's compile pass; admission-time check
  would only matter if cluster ops want to *force* it on for all
  InferencePolicies. Not requested.
- **MutatingAdmissionPolicy that auto-tightens floors** to the
  cluster minimum. We deliberately reject vs. mutate so author
  intent is visible — silent re-writes during reconcile are an
  audit-trail anti-pattern.

## Hard-rule checklist (`docs/implementation-plan.md` §0.2)

| # | Rule | Status |
|---|------|--------|
| 1 | No fork; no upstream re-implementation | ✓ — uses GA VAP API only |
| 3 | No file growth past Phase 2 cap | ✓ — new 152-line template |
| 4 | No env-var scope creep | ✓ — toggled via Helm values, not env |
| 8 | No custom-crypto / framing | ✓ — N/A |
| 9 | Audit doc with two sign-offs | ✓ — this doc |
| 10 | Verify, don't guess | ✓ — `helm template` renders both the policy and the binding; invalid `minimum` value triggers `helm fail` immediately; CEL syntax mirrors the existing `admission-null-provider.yaml` patterns (`?` chained access, `optMap`, `orValue`) |

## Test coverage

- `helm lint deploy/helm/azureclaw` — clean.
- `helm template` produces 3 occurrences of
  `azureclaw-content-safety-floor` (policy + binding + the
  `policyName` reference inside the binding).
- `helm template --set admission.contentSafetyFloor.minimum=Bogus`
  exits with `Error: execution error: admission.contentSafetyFloor.minimum
  must be one of Safe|Low|Medium|High`.
- No Rust / TS code changes; existing controller test suite (349
  tests) unchanged.

## Threat model

- **Author bypass.** The `dev-only` label is the only declared
  bypass; clusters that want strict prod-only enforcement remove
  the label-based exception in the policy file. Today, mirroring
  null-provider-block, dev-only is honored to keep dev-loop UX
  fast.
- **CEL evaluation safety.** All four severity checks short-circuit
  on `variables.devOnly` first; the lookups use chained-optional
  (`?` / `optMap`) so a missing `spec.contentSafety` block returns
  `-1` (sentinel) and is treated as "no override → floor applies
  via router default" rather than denied.
- **Failure-policy.** `failurePolicy: Fail` so a kube-apiserver →
  policy-evaluation glitch denies (not allows). Consistent with
  null-provider-block and pod-exec-ban.
- **Cluster operator escape hatch.** `enabled: false` in values.yaml
  disables the entire policy at chart deploy time.

## Existing implementation surveyed

- `deploy/helm/azureclaw/templates/admission-null-provider.yaml` —
  CEL pattern, dev-only opt-out convention, binding shape.
- `deploy/helm/azureclaw/templates/crd-inferencepolicy.yaml:86-118` —
  `contentSafety` schema with hate/selfHarm/sexual/violence string
  fields and the `Safe|Low|Medium|High` validation.
- `controller/src/inference_policy_compile.rs` — enum decode pass
  that today is the only enforcement of severity strings (read
  path; doesn't gate admission).

## §14.6 / §15 impact

- §10.4 #4 (VAP/MAP expansion beyond Phase 1 core set): partial
  closure — Content-Safety floor lands; remaining items
  (per-namespace floor, additional posture-downgrade denials)
  tracked for future slices.
- §15.2 #10 (S7 craftsmanship train): closes the explicit
  Content-Safety floor item; S7.C.2 (predicated informers) and
  S7.E.2 (reconcile histograms) remain.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
