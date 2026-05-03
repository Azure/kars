# Security Audit — Phase G P1 #5: Secondary Resource Watches

**Date:** 2026-05-03
**Scope:** Controller reconcile-on-child-change wiring (`ClawSandbox` watches its child `Deployment`).
**Closes §14.6 line item:** "Controller craftsmanship — secondary resource watches".

## Change summary

The controller previously reacted to child-resource drift only on the
periodic 5-minute requeue. Manual mutations such as
`kubectl scale deploy --replicas=N` therefore lingered until the next
poll, leaving a window in which the actual cluster state diverged
from the desired (`spec`-derived) state.

This change adds a `Controller::watches(Deployment, ...)` arm with a
synchronous label-based mapper that maps a child Deployment back to
its parent `ClawSandbox` so the reconciler runs within seconds of
any child change.

### Cross-namespace constraint

Kubernetes does **not** permit `ownerReferences` across namespaces.
`ClawSandbox` lives in any namespace (commonly `azureclaw-system`)
while its Deployment is created in `azureclaw-{name}`. Therefore
`Controller::owns()` (which presumes same-namespace parent/child) is
not usable, and existing finalizer-based cascade cleanup
(`namespace-cleanup`) is the authoritative deletion path.

### Mapper contract

The mapper `deployment_to_sandbox_ref` produces an
`Option<ObjectRef<ClawSandbox>>` from the Deployment's labels alone
(no async API calls — kube-rs forbids them in this context). It
requires three labels:

| Label | Role |
|---|---|
| `azureclaw.azure.com/component=sandbox` | Filters out unrelated Deployments. |
| `azureclaw.azure.com/sandbox=<name>` | Parent CR name. |
| `azureclaw.azure.com/parent-namespace=<ns>` | Parent CR namespace. |

`parent-namespace` is **new** in this PR. Pre-PR Deployments lack it
and are silently skipped by the mapper; on the next periodic
requeue the reconciler re-applies the Deployment with the new label,
after which subsequent edits trigger the watch.

## STRIDE delta

| Threat | Pre-change posture | Post-change posture |
|---|---|---|
| **T**ampering — operator/attacker scales Deployment up to siphon traffic via additional pods | Drift persists up to 5min until requeue | Reverted within ≤2s (typical) of the change |
| **T**ampering — attacker forges labels on an unrelated Deployment to trigger spurious reconciles on a real CR | N/A (no watch) | Bounded blast radius: mapper enqueues a reconcile of the named CR; reconcile is idempotent and reads its own desired state from the CR `spec`. No mutation by the attacker's payload. |
| **D**oS — flood of Deployment-update events | N/A | kube-rs `Controller` deduplicates by ObjectRef; existing per-reconcile rate-limit governs. |
| **I**nformation disclosure | unchanged | unchanged (mapper consults only labels we ourselves authored) |

## Fail-closed semantics

* If the mapper cannot identify the parent (missing/empty labels), it
  returns `None`. The Controller does **not** enqueue. The periodic
  5-minute requeue remains the safety net — same behaviour as before
  this PR for those Deployments.
* If `Api::<Deployment>::all()` cannot list (RBAC, API server
  outage), kube-rs surfaces watcher errors but the primary
  `ClawSandbox` reconciler is unaffected. No silent success path is
  introduced.

## OWASP-LLM mapping

Not directly applicable (controller-plane change). Indirectly hardens
**LLM05 (Improper Output Handling)** posture by ensuring the agent
runtime continues to run with the operator-declared replica count
rather than an attacker-injected one.

## RBAC

The controller already has `list/watch` on `apps/v1.Deployments`
cluster-wide (granted in
`deploy/helm/azureclaw/templates/rbac.yaml`). No new RBAC required.

## Test coverage

* **Unit (5 new tests, `controller/src/reconciler/mod.rs`):**
  * `mapper_returns_ref_for_well_labeled_deployment`
  * `mapper_skips_unlabeled_deployment`
  * `mapper_skips_wrong_component`
  * `mapper_skips_pre_pr_deployment_without_parent_namespace`
  * `mapper_skips_empty_label_values`
* **E2E (`tests/e2e/run.sh::test_secondary_resource_watch`):**
  apply CR → wait for `replicas=1` → verify `parent-namespace` label
  → `kubectl scale --replicas=5` → assert restored to `1` within
  40s.

## Scope deferrals

* `.watches(NetworkPolicy)` and `.watches(ConfigMap)` deliberately
  **not** added in this PR. Same mapper pattern would apply, but
  drift on those is less urgent (NP changes don't yield extra pods)
  and they would multiply the watch-event surface. Tracked as a
  follow-up under Phase G P1 #5b.
* Out-of-band changes to a Deployment that don't carry the
  parent-namespace label (i.e. those created before this PR) remain
  on the periodic-requeue safety net for one cycle. Documented above.

## Verification commands

```sh
cargo test --package azureclaw-controller --bin azureclaw-controller \
    reconciler::watch_tests
cargo test --package azureclaw-controller    # 429 pass
cargo clippy --package azureclaw-controller --all-targets -- -D warnings
make test-e2e                                # extends to test_secondary_resource_watch
```
