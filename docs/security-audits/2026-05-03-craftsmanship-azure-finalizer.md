# Security Audit — Phase G P1 #6: Azure Finalizer Hardening

**Date:** 2026-05-03
**Scope:** Federated identity credential deprovisioning on `ClawSandbox` deletion.
**Closes §14.6 line item:** "Controller craftsmanship — Azure finalizer".

## Change summary

Pre-PR, the `ClawSandbox` deletion finalizer treated federated
credential deprovisioning as **non-fatal**: a transient Microsoft
Graph / ARM error during `DELETE
.../federatedIdentityCredentials/...` was logged at WARN, and the
finalizer was removed regardless. The user-assigned managed
identity's federated credential was therefore left orphaned in
Azure until `fedcred_reaper` (a periodic backstop) collected it
on its next sweep.

This PR makes the deprovision step **blocking**: if the ARM call
fails with anything other than success or `404 Not Found`, the
reconciler returns `Action::requeue(30s ± jitter)` without
removing the finalizer. The deletion path retries every ~30s
until the credential is confirmed gone.

`fedcred_reaper` remains in place as the backstop for two paths
this PR cannot cover:
1. `kubectl delete --force` (which skips finalizers entirely).
2. Pre-finalizer CRs that existed before the cleanup logic landed.

## STRIDE delta

| Threat | Pre-change posture | Post-change posture |
|---|---|---|
| **D**enial of resource exhaustion — Graph quota for federated credentials per UAMI (max 20) | Could be exhausted by orphans accumulating across transient failures | Steady-state cleanup is synchronous; orphans bounded to force-delete + pre-finalizer cases (reaper still covers these) |
| **E**levation — orphan federated credential allows a re-created sandbox of the same name to inherit the prior identity | Possible during the reaper's collection window | Eliminated for the steady-state path; reaper window is the same as before for the bypass paths |
| **D**oS — controller spins on a permanently-failing fedcred deprovision | N/A (was non-blocking) | Bounded by Graph 5xx → existing retry-after backoff in `fedcred.rs::get_arm_token`; 30s jittered requeue prevents tight-loop |

## Fail-closed semantics

* `delete_federated_credential` already treats `404 Not Found` as
  success (the credential is the post-condition, not the API call).
  This PR preserves that behaviour: the finalizer is removed only
  when the credential is **confirmed absent**.
* Any other non-success status code surfaces as an `Err(String)`,
  which now triggers a requeue. The CR remains in `Terminating`
  state with the finalizer still attached — visible to operators
  via `kubectl get clawsandbox -o yaml | grep finalizers`.
* Namespace deletion (and CRB cleanup) still happens before the
  fedcred deletion attempt, so user-visible workload teardown is
  not blocked by Azure-side flakiness.

## OWASP-LLM mapping

Indirectly hardens **LLM02 (Insecure Output Handling)** and
**LLM10 (Model Theft)** posture: an orphan federated credential
attached to a long-lived UAMI grants Workload-Identity-bearer
tokens to the next pod that mounts it. Eliminating the orphan
window closes a small but real lateral-movement primitive.

## Test coverage

Existing unit tests in `controller/src/fedcred_reaper.rs` and
`controller/src/fedcred.rs` continue to cover the reaper +
delete-on-404 paths. The synchronous-finalizer behaviour itself
is exercised via the existing `test_sandbox_lifecycle` E2E (CR
delete → namespace cleanup) path, which on the kind cluster runs
without an `FedCredManager` configured (`ctx.fedcred = None`),
making the new `if let Some(...)` branch a no-op — same coverage
shape as before.

A dedicated integration test for the requeue behaviour requires a
mock Graph server and is deferred (tracked under Phase G P1 #6b).

## Scope deferrals

* **No additional Azure resources are deprovisioned** by this PR.
  Audit confirmed the only Azure-plane resource the controller
  itself provisions is the federated credential. Foundry agent
  IDs are written by the inference router and tied to the agent's
  Foundry project lifecycle, not the CR.
* `fedcred_reaper` retained as backstop for force-delete and
  pre-finalizer CRs.

## Verification commands

```sh
cargo test --package azureclaw-controller             # 429/429 pass
cargo clippy --package azureclaw-controller --all-targets -- -D warnings
cargo fmt --all -- --check
```
