# Security audit — CEL admission validations on McpServer + ToolPolicy CRDs

**Date:** 2026-04-25
**PR branch:** `phase1/crd-cel-validations`
**Capability owner:** AzureClaw Phase 1 — admission control

## 1. Summary

Adds `controller/src/crd_validations.rs` — a post-processing helper
that injects `x-kubernetes-validations` (CEL) rules onto the
kube-rs-generated `CustomResourceDefinition`s for `McpServer` and
`ToolPolicy`.

This closes Phase 1 §7 entry 12: every new CRD ships with admission
CEL coverage. Without this, malformed CRs commit to etcd and the
reconciler is left to choke on them, leaving operators with
`status.phase: Pending` and no actionable error.

## 2. Threat model delta

### Asset gaining new exposure
None. Tightens admission rather than expanding the surface.

### STRIDE

- **Spoofing (S)** — admission rejects `productionMode: true` CRs that
  lack `oauth.issuer` or use a plaintext `http://` URL. An attacker
  who somehow obtains `apply` privilege on `McpServer` cannot smuggle
  in a "production-mode" server that isn't actually OAuth-gated.
- **Tampering (T)** — `commerce.dailyCap > monthlyCap` and negative
  caps are rejected before they reach the AP2 ledger. A spec saying
  "spend $1B/day, $1/month" is impossible to interpret safely; CEL
  fails the apply.
- **Elevation of privilege (E)** — `appliesTo.matchLabels` empty is
  rejected. An empty selector matches every sandbox in the namespace
  — a common authoring mistake that would silently broaden a
  ToolPolicy's reach.

## 3. OWASP mapping

- **OWASP MCP01 — Misconfiguration:** the `productionMode → https`
  and `productionMode → oauth.issuer` rules turn the most dangerous
  misconfigurations into apply-time errors with a human-readable
  message.
- **OWASP MCP05 — Authentication and Authorization Bypass:** the
  invariant `productionMode == true → oauth.issuer != ""` makes it
  syntactically impossible to declare a "production" MCP server
  without naming an issuer.
- **OWASP LLM10 — Unbounded Consumption:** non-negative cap rule and
  daily ≤ monthly rule prevent operator typos from disabling AP2
  enforcement.

## 4. AuthN / AuthZ path

Not applicable directly — these rules run on the kube-apiserver as
part of admission, before any AzureClaw component sees the CR. The
existing K8s RBAC for `apply` on `McpServer`/`ToolPolicy` continues
to gate who can submit CRs at all.

## 5. Secret + key custody

No secrets handled. The rules only inspect public spec fields.

## 6. Egress surface delta

None.

## 7. Audit events emitted

None — admission rejection is surfaced to the `kubectl apply` caller
as a structured error. The controller never sees rejected CRs and
therefore never logs them. Cluster auditors who want a record of
rejected applies should rely on the apiserver's audit log
(`requestObject` + `responseStatus.reason: Invalid`).

## 8. Failure mode

CEL is fail-closed at admission: a rule that returns `false` rejects
the apply with the rule's `message`. There is no fall-through to the
reconciler.

| Input | Behaviour |
|---|---|
| `McpServer{productionMode: true, oauth: nil}` | rejected: "productionMode requires spec.oauth.issuer to be set" |
| `McpServer{productionMode: true, url: "http://..."}` | rejected: "productionMode requires spec.url to begin with https://" |
| `McpServer{oauth: {pkce: "plain"}}` | rejected: "spec.oauth.pkce, when set, must be 'S256' (RFC 7636 §4.2)" |
| `ToolPolicy{commerce: {dailyCap: 100, monthlyCap: 50}}` | rejected: "dailyCap must be <= monthlyCap" |
| `ToolPolicy{commerce: {dailyCap: -1}}` | rejected: "must be non-negative" |
| `ToolPolicy{appliesTo: {matchLabels: {}}}` | rejected: "must contain at least one label" |

## 9. Negative-test coverage

Eleven in-tree tests in `controller/src/crd_validations.rs`:

- Each rule list is non-empty.
- Every rule has both `rule` body and `message` populated.
- Injection round-trips through `CustomResourceDefinition` and
  `serde_yaml::to_string` (the format `kubectl apply -f -` consumes).
- The serialized YAML contains `x-kubernetes-validations` and the
  expected field names — locks against accidental schema-key drift.
- Rules mention the canonical invariant strings (`productionMode` +
  `oauth`, `dailyCap` + `monthlyCap`) — locks against future PRs that
  silently weaken the rule set.
- `inject_spec_validations` returns `None` (not panic) when handed a
  malformed schema tree — the only programmer-error exit, gracefully
  surfaced.

The tests do **not** evaluate the CEL rules themselves; that
requires a CEL evaluator (`cel-cpp`/`cel-go`) which is a kube-apiserver
concern. The tests assert the rules are **shipped** with the CRD;
the evaluator's correctness is the apiserver's responsibility.

## 10. Vendored / third-party dependency delta

None. `k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::ValidationRule`
is already on the workspace (used elsewhere in the controller).
`serde_yaml` is already a workspace dep.

Sources consulted:

- KEP-2876 (CEL admission CRD validations) —
  <https://github.com/kubernetes/enhancements/tree/master/keps/sig-api-machinery/2876-crd-validation-expression-language>.
- RFC 7636 §4.2 (PKCE methods) —
  <https://datatracker.ietf.org/doc/html/rfc7636#section-4.2>.
- `controller/src/mcp_server.rs` doc-comments referencing "admission
  CEL" requirements (lines 47, 58 and 90 of the prior file).

## 11. Sign-offs

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
