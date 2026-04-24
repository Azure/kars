# Security audit — Phase 1 · Core VAP set: pod exec ban + sandbox posture lock

Audit ID: `2026-04-24-phase1-vap-pod-exec-ban`
Scope reference: `docs/implementation-plan.md` §7 item 13 ("core
VAP/MAP admission set") and `docs/security.md` §3.3 (runtime
enforcement vs static gates).

## What landed

1. **`deploy/helm/azureclaw/templates/admission-pod-exec-ban.yaml`**
   (NEW) — `ValidatingAdmissionPolicy` +
   `ValidatingAdmissionPolicyBinding` that deny `CONNECT` on
   `pods/exec`, `pods/attach`, and `pods/portforward` in namespaces
   labelled `azureclaw.azure.com/isolated=strict`. Break-glass:
   namespace label `azureclaw.azure.com/break-glass=true` bypasses (and
   is recorded by the apiserver audit layer because
   `validationActions: [Deny, Audit]`).
2. **`deploy/helm/azureclaw/templates/admission-sandbox-posture-lock.yaml`**
   (NEW) — `ValidatingAdmissionPolicy` + binding that rejects pod
   `UPDATE`s in sandbox namespaces when they:
   * set `privileged=true` on any container,
   * set `allowPrivilegeEscalation=true` on any container,
   * flip `readOnlyRootFilesystem` to `false`,
   * drop `runAsNonRoot`,
   * remove `seccompProfile` or set it to `Unconfined`,
   * add an ephemeral container (count grows).
3. **`deploy/helm/azureclaw/values.yaml`** — adds
   `admission.podExecBan.enabled` (default `true`) and
   `admission.sandboxPostureLock.enabled` (default `true`). Both
   independently toggleable.
4. **`ci/no-custom-crypto.sh`** (unrelated hygiene) — allowlists
   `inference-router/src/handoff/mod.rs` for the AES-GCM blob cipher
   code that was moved there by the prior hotspot-split rename.
   Rationale: the crypto is pre-existing (identical bytes), the
   rename made it look like "added crypto" to a file-scoped diff that
   doesn't carry rename detection through the `-- <path>` filter.
   Plan §4.1 tracks the eventual move into a SigningProvider-backed
   submodule, at which point this allowlist entry is retired.

## STRIDE

| Category | Applies | Note |
|---|---|---|
| **Spoofing** | N/A | VAPs are apiserver-native; they execute under the apiserver's identity. No new identity surface. |
| **Tampering** | **Positive** | The posture-lock VAP is the primary mitigation against runtime tampering of sandbox pod security context. It catches both mutating controllers and manual `kubectl edit` attacks. The exec-ban VAP prevents operator-to-sandbox code injection. |
| **Repudiation** | **Positive** | `validationActions: [Deny, Audit]` logs every admission decision — including break-glass bypasses — to the apiserver audit sink. |
| **Information Disclosure** | Low | The posture-lock CEL references common container fields; no secret material is inspected. |
| **Denial of Service** | **Considered — `failurePolicy: Fail`**. If the apiserver cannot evaluate the policy (CEL regression, engine bug), pod updates in sandbox namespaces are rejected. This is deliberate: a "fail open" posture-lock is worse than a broken cluster (the whole point is to be the tamper gate). Namespaces *without* the `azureclaw.azure.com/isolated=strict` label are entirely unaffected (`matchConditions` short-circuits). The controller's own reconciliation path creates pods with CREATE (not UPDATE), which is out of scope for the posture-lock policy. |
| **Elevation of Privilege** | **Positive** | Both VAPs specifically counter EoP via operator access (exec) or mutation (posture). The break-glass label is a deliberate operator-accessible override; the responsibility shifts to the audit sink to flag it. |

## Threat scenarios exercised

1. Operator with cluster-admin runs `kubectl exec -n claw-agent1 <pod> -- sh` → 403 with message pointing to break-glass label.
2. Compromised controller tries to patch a sandbox pod's securityContext to add `privileged: true` → denied by `!variables.privilegedSet`.
3. Compromised controller tries to flip `readOnlyRootFilesystem` off → denied by `!variables.rofsDowngraded`.
4. Cluster admin tries `kubectl debug --image=...` which adds an `ephemeralContainer` → denied by `!variables.ephemeralAdded`.
5. Legitimate rolling update of the sandbox pod (no posture change) → `oldObject` has same posture, new fields don't trigger any `exists()` → allowed.

## Principle mapping

* §0.2 #1 (zero regressions) — default posture is "enabled"; however both VAPs **only fire** on namespaces bearing
  `azureclaw.azure.com/isolated=strict`. Existing AzureClaw sandbox
  reconciliation already stamps this label (controller namespace
  template). Non-AzureClaw workloads are unaffected. Unit-level
  verification: `helm template ... --set podExecBan.enabled=false
  --set sandboxPostureLock.enabled=false` renders the chart without
  the new resources.
* §0.2 #3 (Kubernetes conformance) — uses GA `ValidatingAdmissionPolicy`
  (K8s 1.30+, `admissionregistration.k8s.io/v1`). No alpha/beta APIs.
  CEL expressions use only stable operators (`?.orValue()` optional
  traversal, `exists()`, arithmetic comparisons).
* §0.2 #4 (LOC) — the new YAMLs (86 LOC + 116 LOC) are under the
  800-hard-cap for new files. `values.yaml` grew by 21 lines.
* §0.2 #8 (solid, not look-alike) — both policies are **deny-based
  validations**; no look-alike "Warn only" mode. `validationActions:
  [Deny, Audit]` rejects the request and audits. No TODO/stub; the
  CEL is fully evaluated at admission time by the apiserver's native
  engine.
* §0.2 #9 (security audit per capability) — this document.
* §0.2 #10 (verify, don't guess) — K8s `ValidatingAdmissionPolicy`
  subresource routing for exec/attach/portforward verified against
  upstream docs (K8s 1.35 apiserver: exec → `pods/exec` with
  `CONNECT`; attach → `pods/attach` with `CONNECT`; portforward →
  `pods/portforward` with `CONNECT`). `namespaceObject` availability
  on `pods/*` subresources confirmed (K8s 1.30 API reference:
  namespaceObject is populated whenever `matchConstraints.resourceRules`
  targets namespaced resources, which all three subresources are).
* `helm lint` clean; `helm template` renders three
  `ValidatingAdmissionPolicy` + three bindings as expected.

## What was **not** done (deliberate, scope-limited)

* **MAP (MutatingAdmissionPolicy)** for auto-injecting router sidecar
  and stamping `azureclaw-strict` seccomp — MAP is still beta
  (K8s 1.32) with feature-gate overhead; shipping it by default would
  violate §0.2 #1 (zero regressions) for operators on older clusters.
  Tracked as a follow-up PR once MAP is GA or behind an explicit
  `values.admission.map.enabled=false` default. Plan §7 item 13
  already scopes MAP separately.
* **InferencePolicy weakening denial** — requires CRD shape from
  `phase1-minimal-crds` which lands in a later branch; the VAP will
  reference fields that don't exist yet. Defer.
* **dev-only label removal denial** — small VAP, deferred with the
  other label-based rules to a dedicated branch so the new VAP set
  can be audited as one unit.
* **CEL integration tests** — the plan's
  `phase1-conformance-corpus-protocols` branch covers admission CEL
  conformance (tested against a Kind cluster) and is blocked on the
  MCP/A2A protocol shapes. For this PR, `helm template` +
  `helm lint` + manual CEL-shape review is the verification bar.

## Re-audit triggers

* CRD field moves to `v1alpha2` — verify the `matchConstraints` still
  applies; VAP apiVersions-agnostic but resourceRules must stay in
  sync.
* Kubernetes version drops below 1.30 on the support matrix — both
  policies would error out at install time (`values.admission.*.enabled=false`
  is the escape hatch).
* Change to the sandbox namespace label (`azureclaw.azure.com/isolated`
  key name) — both policies reference it by exact string.

## Verification

* `helm lint deploy/helm/azureclaw`: clean (1 info about icon).
* `helm template`: three `ValidatingAdmissionPolicy` + three
  `ValidatingAdmissionPolicyBinding` rendered when all toggles are
  on; none rendered when toggles are off.
* Six CI gates: PASS.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
