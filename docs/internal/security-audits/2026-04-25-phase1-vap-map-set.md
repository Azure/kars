# Security Audit: `phase1/vap-map-set`

**Capability:** ships two new admission policies per implementation-plan
§7 entry 13. Both are namespace-scoped to sandbox namespaces only.

## 1. Summary

1. **`admission-dev-only-label-immutable.yaml`** — VAP that requires an
   audit-trail annotation when removing the
   `azureclaw.azure.com/dev-only=true` label from `ClawSandbox` /
   `McpServer` / `ToolPolicy`.
2. **`admission-seccomp-auto-stamp.yaml`** — MAP that auto-stamps
   `azureclaw-strict.json` Localhost seccomp profile onto pods in
   sandbox namespaces (label `isolated=strict`) when missing.

## 2. Threat model delta

**VAP — dev-only label immutability.** The dev-only label is the single
opt-out for fail-closed defaults across the platform. Without this VAP,
a compromised operator could silently flip a production tenant into
"dev mode," disabling Null-provider rejection and several future
posture checks. The VAP forces any removal to be audit-logged with an
explicit reason annotation; failure-mode is `Fail` (deny on policy
evaluation error), which is fail-closed.

**MAP — seccomp auto-stamp.** Defense-in-depth against future runtime
adapters omitting `securityContext.seccompProfile`. The reconciler
already sets it; this is a backstop. Failure-mode `Fail` blocks pod
creation if MAP can't run, which is acceptable for sandbox namespaces
(empty cluster: nothing to break).

## 3. Spec sources

- VAP: <https://kubernetes.io/docs/reference/access-authn-authz/validating-admission-policy/>
- MAP: <https://kubernetes.io/docs/reference/access-authn-authz/mutating-admission-policy/>
  (alpha in 1.32, beta in 1.34+; AKS requires K8s 1.34+ for this MAP).
- CEL `?` and `.orValue()`: optional types, K8s 1.31+.

## 4. Failure modes

| Path | Mode |
|------|------|
| VAP evaluation error | Deny (Fail policy) |
| MAP failure | Pod creation refused (Fail policy) |
| Annotation provided + label removed | Allow + audit |
| Pod already has seccompProfile | No-op (matchCondition gates) |

## 5. Tests

- YAML lint passes (helm templates are not unit-testable in isolation;
  full e2e covers via Phase 0 compat suite).
- Conformance corpus row "seccomp / Landlock / egress-guard" already
  asserts seccomp lands; this MAP makes that assertion robust against
  controller bugs.

## 6. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
