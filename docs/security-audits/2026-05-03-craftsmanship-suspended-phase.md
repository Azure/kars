# Security Audit — `craftsmanship-suspended-phase`

**Date:** 2026-05-03
**PR:** #170 (target `dev`)
**Author:** @copilot-cli
**Independent reviewer:** TBD (controller-data-plane)
**Capability scope:**
Adds operator-driven graceful pause to `ClawSandbox` via a new
`spec.suspended: bool` field. When `true`, the controller scales the
sandbox Deployment to `replicas: 0` while preserving every other
overlay object (namespace, NetworkPolicy, ServiceAccount, governance
ConfigMaps, federated-identity binding) byte-identical, and stamps
the K8s-canonical `Suspended=True / Reason=SuspendedBySpec` condition
on `.status.conditions`. Touches `controller/src/crd.rs` (one new
field), `controller/src/reconciler/mod.rs` (replicas gate + status
extras), `controller/src/status/conditions.rs` (two new reason
constants), the Helm CRD YAML (one new field declaration), and the
E2E gate (`tests/e2e/run.sh` — `test_sandbox_suspended_lifecycle`,
4 assertions). Closes Phase G P1 #4 of the §14.6 craftsmanship list.

---

## 1. Summary

Operators previously had two ways to pause a sandbox: delete the
`ClawSandbox` (destroys all state including AGT trust + governance
ConfigMaps) or scale the underlying `Deployment` directly (silently
fights the controller's reconcile loop, since the controller
re-applies `replicas: 1` on every pass via Server-Side Apply).
Neither is correct. Phase G #4 closes that gap with a first-class
`spec.suspended` field that the reconciler honours: `True` means
"freeze the Pod, preserve everything else"; absent or `False` means
"normal operation". The Suspended Condition is stamped to expose the
state to dashboards and `kubectl wait --for=condition=Suspended`.

## 2. Threat model delta (STRIDE)

| Threat | Before | After |
|---|---|---|
| **Tampering** with overlay state during pause | Operator's only choices were `delete` (destructive) or hand-scaling Deployment (fights controller). | First-class `spec.suspended` preserves all overlay state. No new tamper surface introduced — the field is part of `spec`, subject to the same RBAC + admission policies as every other field. |
| **Repudiation** of pause/resume | Operator-driven scale-down was invisible in the CR. | `Suspended=True/SuspendedBySpec` Condition with `lastTransitionTime` provides an audit trail in the CR itself. Un-suspend stamps `Suspended=False/Active` so the resume transition is also visible. |
| **Spoofing** | N/A — same as any other spec mutation. | N/A. |
| **Information disclosure** | N/A. | N/A — no new data exposed. |
| **Denial of service** | An attacker with `update` on `clawsandboxes` can already corrupt spec. | Same threat surface; suspended just gives them one more knob (set-and-leave to silently halt agents). Mitigated by existing RBAC + audit-log of `kubectl edit`/`kubectl patch`. Not a new vector. |
| **Elevation of privilege** | N/A. | N/A. |

## 3. OWASP LLM-Top-10 mapping

- **LLM08 — Excessive Agency**: explicit pause/resume gives operators
  a graceful kill-switch that doesn't lose forensic state, which
  improves response capability when an agent misbehaves.

Other items unaffected.

## 4. Fail-closed semantics

- **Default off**: `spec.suspended` is `Option<bool>` with `serde
  default` and `skip_serializing_if = "Option::is_none"`. Existing
  CRs do not get a `Suspended` Condition retroactively unless they
  opt in.
- **Overlay-mode dominance**: if both `suspended=true` *and*
  `upstreamCompatibility.sigsAgentSandbox=overlay` are set, OverlayMode
  wins (no Deployment is created either way; `Suspended=True/OverlayMode`
  is stamped, not `SuspendedBySpec`). This avoids stamping a
  contradictory pair of reasons.
- **Drift handling**: even when `suspended=true`, the reconciler
  walks the rest of the deployment block — image, env, volume changes
  are still applied to the (zero-replica) Deployment so resume picks
  up the latest spec without a forced re-roll.
- **Condition cleanup**: when an operator un-suspends, the controller
  stamps `Suspended=False/Active` *only* if the prior reason was
  `SuspendedBySpec` (i.e. the pause was operator-driven). Sandboxes
  that have never had a Suspended condition do not grow one
  retroactively.

## 5. Test coverage

- **424 / 424** controller unit tests pass after the change
  (`cargo test --package azureclaw-controller`).
- **E2E `test_sandbox_suspended_lifecycle`** (4 assertions):
  1. Sandbox created with `suspended: true` → Deployment `replicas: 0`
  2. `Suspended=True / Reason=SuspendedBySpec` Condition stamped
  3. Patch `suspended: false` → Deployment `replicas: 1` restored
  4. `Suspended=False / Reason=Active` Condition stamped after resume

## 6. Backward compatibility

- Field is `Option<bool>` with `skip_serializing_if`. CRs that omit
  it serialize byte-identical to pre-change CRs.
- The CRD YAML adds the field but does not require it (`type: boolean`
  with no `required`).
- No change to RuntimeReady, Ready, Progressing semantics.
- No change to overlay-mode behaviour.
- No new K8s API objects, no new Conditions types (Suspended already
  existed for OverlayMode); only two new `reason` constants.

## 7. Out-of-scope (explicitly deferred)

- Pod-eviction-on-suspend (we scale to 0 and let the existing Pod
  terminate via the Deployment's own grace period; no
  `evictPolicy: Delete` semantics).
- Pause of governance reconcile (governance ConfigMaps still update
  on suspended sandboxes — by design, so resume is correct).
- CronJob-driven auto-suspend (e.g. "suspend at 6pm") — operator
  tooling, not controller responsibility.
- Suspended-state metrics — covered by Phase G P2 #10
  (Prometheus expansion) in a follow-up PR.
