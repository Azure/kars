# Security Audit — Phase G P2 #13: CEL validations on ClawSandbox

**Date:** 2026-05-03
**Scope:** Spec-level admission CEL on the flagship `ClawSandbox`
CRD. Adds three new `x-kubernetes-validations` rules covering
cross-field invariants that were previously enforced only at the
reconciler layer (or not at all).
**Closes §14.6 line item:** "CEL validations embedded in CRD
YAML (P2 #13)".

## Change summary

`deploy/helm/azureclaw/templates/crd.yaml` ClawSandbox `spec`:

1. **BYO ⊕ Foundry-agent mutual exclusion.**
   ```cel
   self.runtime.kind != 'BYO' || !has(self.agent)
   ```
   BYO containers own their own inference + agent loop and bypass
   the controller-managed Foundry prompt agent
   (`reconciler/mod.rs` ~line 1017 silently no-ops the agent block
   on BYO). The CEL turns the silent no-op into an immediate
   `kubectl apply` rejection so the operator can't be misled into
   thinking their `instructions` block is being honoured.

2. **`governance.toolPolicyRef.name` is same-namespace only.**
   ```cel
   !has(self.governance) || !has(self.governance.toolPolicyRef) || !has(self.governance.toolPolicyRef.name) || (!self.governance.toolPolicyRef.name.contains('/') && !self.governance.toolPolicyRef.name.contains(':'))
   ```
   The schema-level `pattern: "^[a-z0-9](...)"` regex already
   rejects `ns/name` and `ns:name` — but it rejects them with a
   generic "does not match pattern" message that doesn't tell the
   operator *why*. The new CEL gives a precise message naming the
   security invariant (`docs/crd-precedence.md`: cross-namespace
   refs forbidden because they would be a privilege-escalation
   vector).

3. **`governance.trustThreshold` ∈ [0, 1000].**
   ```cel
   !has(self.trustThreshold) || (self.trustThreshold >= 0 && self.trustThreshold <= 1000)
   ```
   The trust-score domain is documented as 0–1000 (see
   `crd.rs::GovernanceConfig`). Values outside the range are
   silently clamped at the inference-router boundary; without the
   CEL the operator sees no error and the agent silently runs at
   the clamp value.

## STRIDE delta

| Threat | Pre-PR posture | Post-PR posture |
|---|---|---|
| **E**levation via cross-namespace `toolPolicyRef` | Already blocked by schema pattern, but with a generic error | Same block **with operator-actionable error message** — operator can't accidentally retry with a different ns syntax |
| **T**ampering — operator sets `agent` on a BYO sandbox expecting it to take effect | Silently ignored at reconcile time (no Foundry agent created); operator sees `Running` but no agent | Rejected at apply time with explicit message |
| **D**oS — operator sets `trustThreshold: 999999` accidentally | Silently clamped; trust-score check effectively disabled (or always-deny depending on clamp direction) | Rejected at apply; explicit range message |

## Fail-closed semantics

* All three rules are written so that a missing nested field is
  treated as "rule does not apply" (`!has(...) || ...`) — they
  are guards, not requirements. The ClawSandbox `required` list
  is unchanged.
* On CEL compilation failure the API server fails closed
  (rejects all writes to the CRD) — so a typo in this YAML would
  surface immediately on `helm upgrade` rather than silently
  disabling the rule.

## OWASP-LLM mapping

* **LLM01 (Prompt Injection):** Rule #1 prevents a BYO sandbox
  from carrying a stale `agent.instructions` block that an
  operator might assume is being delivered. Eliminates a class of
  "I set the system prompt but the agent isn't following it"
  incidents that often get debugged as prompt-injection.
* **LLM06 (Sensitive Information Disclosure):** Rule #2 makes
  the same-namespace invariant explicit; cross-tenant
  toolPolicyRef would be the canonical privilege-escalation
  primitive.

## Test coverage

`tests/e2e/run.sh` (the §14.6 single-most-important gate):

* `test_clawsandbox_cel_rejects_byo_with_agent`
* `test_clawsandbox_cel_rejects_trust_threshold_out_of_range`
* `test_clawsandbox_cel_rejects_cross_namespace_toolpolicy_ref`

Each test attempts a `kubectl apply` of a deliberately-bad
ClawSandbox and asserts that the apply fails. A regression that
removes the CEL would let the apply succeed and the test would
fail.

## Scope deferrals

* No changes to the Rust-side controller `crd_validations.rs`
  module (covers the 6 sub-CRDs via kube-rs derive + injection).
  ClawSandbox CRD is hand-written in Helm and is **not** part of
  the `helm_drift` test scope (`controller/src/helm_drift.rs`
  covers MCP / ToolPolicy / A2AAgent / InferencePolicy /
  ClawMemory / ClawEval only). The Helm CRD remains the single
  source of truth for ClawSandbox; this PR is purely additive.
* No new Rust unit tests — CEL is enforced by the K8s API server,
  not by the controller. The E2E gate is the correct place to
  cover regressions.

## Verification commands

```sh
# Local kind run:
make test-e2e

# CI: same gate runs in `Kind E2E` job.
```
