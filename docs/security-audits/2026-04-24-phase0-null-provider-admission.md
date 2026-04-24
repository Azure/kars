# Security audit — Null-provider admission block (VAP)

**Date:** 2026-04-24
**Capability:** ValidatingAdmissionPolicy + Binding rejecting provider values of `null`, `noop`, `disabled`, or `none` on non-dev tenants
**Branch:** `phase0/null-provider-admission`
**Plan section:** `docs/implementation-plan.md` §6 item 6 + §0.2 principle 9

## 1. Summary

Ship the runtime mirror of `ci/no-null-provider-prod.sh` as a Kubernetes
ValidatingAdmissionPolicy + Binding (K8s ≥ 1.30 GA). Any `ClawSandbox`,
`McpServer`, or `ToolPolicy` whose `spec.agt.providers.*` or `spec.provider`
field equals `null`/`noop`/`disabled`/`none` is denied at admission time
unless the object carries `metadata.labels.azureclaw.azure.com/dev-only:
"true"`.

Files:

- `deploy/helm/azureclaw/templates/admission-null-provider.yaml` — VAP + Binding.
- `deploy/helm/azureclaw/values.yaml` — `admission.nullProviderBlock.enabled` toggle (default `true`).
- `tests/compat/fixtures/null-provider-devonly-ok.yaml` — positive fixture.
- `tests/compat/fixtures/null-provider-prod-denied.yaml` — negative fixture (driver strips the dev-only label at apply time).

## 2. Threat model

Principal threat: a well-meaning operator or a compromised GitOps pipeline
silently disables governance by setting the provider to `null` in production.
Without this policy, the controller tolerates `null`/`noop`/`disabled`
(for YAML-ergonomic reasons) and the pod starts with no policy decision
path, no audit sink, or no signing authority — a fail-open path that
principle §0.2 #8 ("solid not look-alike") exists to prevent.

| STRIDE | Applies? | Control |
|---|---|---|
| Spoofing | N/A | No identity surface added. |
| Tampering | Yes | CRD-spec tampering that disables AGT path is now caught pre-persist. |
| Repudiation | Yes | Without audit provider, events wouldn't land in AuditLogger. VAP prevents the bypass. |
| Information disclosure | N/A | |
| Denial of service | Low | VAP is cheap (4 CEL expressions, string-set membership). `failurePolicy: Fail` means an API-server bug blocks CR writes; see §7. |
| Elevation of privilege | Yes | Mirrors a privilege boundary: prod tenants cannot opt out of AGT governance silently. |

**OWASP LLM Top 10:** LLM06 Excessive Agency — preventing governance
bypass removes the class of excessive-agency condition where an agent
runs without policy gating. **OWASP MCP Top 10:** M05 Missing Access
Controls (indirect).

## 3. AuthN / AuthZ path

The VAP is cluster-scoped, binds to all namespaces
(`matchResources.namespaceSelector: {}`), and denies on the above
condition. `RBAC` for the VAP itself is managed by the K8s API server —
the admission controller runs in-process, no extra credentials. Opt-out
label is checked verbatim (case-sensitive per K8s label rules).

Outage mode: `failurePolicy: Fail` — if the VAP CEL compiler crashes or
the admission layer is unavailable, CR writes are denied. This is the
fail-closed default required by principle §0.2 #8. An operator running
a dev cluster who wants fail-open must set
`admission.nullProviderBlock.enabled: false` in their Helm values
(documented in values.yaml comment), not bypass via `failurePolicy:
Ignore`.

## 4. Secret / key custody

None. Policy is stateless.

## 5. Egress delta

None.

## 6. Audit events

VAP denials are recorded in the K8s API-server audit log (operator's
K8s audit-policy controls retention). No AGT AuditSink emission added —
the CR never persists, so there is nothing for our controller to forward.
AGT gets the *downstream* signal when the tenant re-submits with a valid
provider; that path is already covered.

## 7. Failure mode

- VAP absent / Binding absent (chart flag off): gate `ci/no-null-
  provider-prod.sh` still scans YAML in PRs, so static regressions are
  caught even without runtime enforcement.
- VAP present but K8s < 1.30: chart apply fails on the `admission
  registration.k8s.io/v1` VAP resource; the operator must either upgrade
  the cluster or disable the flag. Documented in values.yaml.
- CEL expression fault: `failurePolicy: Fail` denies the request; CR
  author sees a clear error message pointing at the dev-only-label
  escape hatch.
- Operator attempts to delete the dev-only label after applying a null
  provider: not covered by this PR. Plan §7 item 13 lists a separate VAP
  ("deny removal of azureclaw.azure.com/dev-only label once applied")
  landing in Phase 1.

## 8. Negative-test coverage

- `tests/compat/fixtures/null-provider-prod-denied.yaml` — e2e driver
  strips the `dev-only` label and applies; expects admission `Deny`.
- `tests/compat/fixtures/null-provider-devonly-ok.yaml` — applied
  as-is; expects admission `Accept`.
- `ci/no-null-provider-prod.sh` — green against current diff (verified
  this run).
- `helm lint` + `helm template` — both pass.

## 9. Dependency delta

None. VAP is a first-class K8s resource (no new CRD, no operator
controller required).

## 10. Internal-boundary posture

Consume-not-compete with the K8s admission surface; no overlap with AGT
(which owns policy *evaluation* — we're gating *configuration*).

## 11. Sign-offs

- Author: `Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>`
- Reviewer sign-off: pending user review per local-only workflow rule.

### Re-audit triggers

- CRD schema grows `spec.agt.providers` with non-string types → revisit
  CEL field-access expressions.
- K8s version floor changes (current: ≥ 1.30) → revisit VAP version.
- New CRD type carries a `provider` field → extend `matchConstraints.
  resourceRules` and add fixtures.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
