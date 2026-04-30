# Phase 2 — S7.C: controller-wide leader election

**Date:** 2026-04-29
**Slice:** `phase2-leader-election` (sub-slice S7.C of S7 craftsmanship train)
**Author:** AzureClaw maintainers
**Sign-offs:** `@maintainer-1`, `@maintainer-2`

## Scope

Add a controller-wide Kubernetes Lease (`coordination.k8s.io/v1`) so that
exactly one of the controller Deployment's `replicas: 2` pods reconciles
at a time. Closes the doubled-write / doubled-event / doubled
Foundry-agent-create gap that the SSA `fieldManager` registry from S7.A
left open.

- New module **`controller/src/leader_election.rs`** with:
  - Pure decision function `evaluate_lease(spec, identity, now) -> LeaseAction` (Acquire / Renew / Yield(holder)).
  - Async I/O loop `acquire_and_hold(client, cfg, ready_tx) -> Result<()>` — creates or patches the Lease, signals readiness on first acquisition, returns an error on renew failure to force pod restart (standard fail-stop pattern; mirrors `kube-controller-manager`).
  - `LeaderElectionConfig::from_env` — reads `POD_NAMESPACE` (downward API), `POD_NAME` / `HOSTNAME`, optional `LEADER_ELECTION_LEASE_NAME`. Falls back to `azureclaw-system` namespace and a pid-based identity for dev/kind clusters where the downward API isn't wired.

- **`controller/src/main.rs`** wires the leader gate ahead of the
  reconciler bundle:
  - Default-on (opt-out via `LEADER_ELECTION_ENABLED=false`).
  - `oneshot::channel` blocks reconciler spawn until the lease is
    acquired; if `acquire_and_hold` exits before signalling readiness
    its `ready_tx` drops and the await observes `RecvError`, which
    propagates the leader task's underlying error.
  - The leader handle is added to the final `tokio::select!` so
    leadership loss (renew failure) terminates the process, the pod
    restarts, and a healthy replica re-elects.

## Out of scope

- **Predicated informers** — kube-rs `Controller::watches` predicate to
  skip events that don't change `metadata.generation`. Originally
  bundled with leader election in the plan but split out as **S7.C.2**
  to keep this PR's blast radius surgical. Predicated informers do not
  depend on leader election; either can ship first.
- **Mesh-peer's existing Lease (`agentmesh-mesh-peer-leader`)** is
  intentionally preserved unchanged. Its ownership semantics differ:
  every replica still keeps a relay client running so leader handover
  doesn't drop in-flight pairings; only one *connects* to the relay.
  Collapsing both leases into one would require redesigning that
  fine-grained behaviour, which is outside S7.C scope.
- **Helm chart RBAC additions** — the controller's `ClusterRole`
  already grants `coordination.k8s.io/leases` `[get, create, update,
  patch]` (added in Phase 1 for mesh-peer's lease). No manifest
  changes.
- **Downward-API env var injection** in `controller-deployment.yaml`
  for `POD_NAMESPACE` / `POD_NAME`. The fallbacks (`azureclaw-system` +
  pid-based identity) keep the machinery functional without the env
  vars; injecting them is a manifest-only follow-up that can land
  whenever convenient and is not required for correctness on
  single-replica deployments.

## Hard-rule checklist (`docs/implementation-plan.md` §0.2)

| # | Rule | Status |
|---|------|--------|
| 1 | No fork; no upstream re-implementation | ✓ — uses k8s-openapi `Lease` directly; no new crate |
| 3 | No file grew past Phase 2 cap | ✓ — new module 397 LOC, well under any cap; `main.rs` 181 → 261 (no cap) |
| 8 | No custom-crypto / framing | ✓ — N/A |
| 9 | Audit doc with two sign-offs | ✓ — this doc |
| 10 | Verify, don't guess; cite sources | ✓ — KEP-589 (Lease semantics); k8s-openapi 0.27 LeaseSpec; mesh_peer prior art at `mesh_peer/mod.rs:490–597` |

## Test coverage

7 new unit tests on `evaluate_lease` covering all branches:
- Missing spec → `Acquire`.
- We hold a fresh lease → `Renew`.
- We hold an expired lease → `Renew` (we still own it; just re-extend).
- Other holder, fresh lease → `Yield(other_identity)`.
- Other holder, expired lease → `Acquire` (take over).
- Missing `renewTime` → treated as expired → `Acquire`.
- Empty holder identity with fresh renewTime → `Yield(empty)` (defensive — don't race a malformed lease).

Controller bin tests: 329 → 336 (+7). Clippy `-D warnings` clean.
`cargo fmt --check` clean. The async I/O loop in `acquire_and_hold`
is integration-test surface and is exercised in production by every
controller startup; introducing a fake K8s API client to unit-test it
would require ~500 LOC of test fixture for marginal gain. The pure
decision function unit tests cover the branching.

## Threat model

- **Doubled writes from a stale leader after network partition.**
  The renew-failure → process-exit pattern means a partitioned leader
  cannot keep writing once it loses connectivity to the API server
  long enough to miss `lease_duration_secs / renew_period_secs ≈ 3`
  consecutive renewals. The new leader takes over after the lease
  expires; the old leader's pod has already exited. Window of
  potential overlap: ≤ `lease_duration_secs` (15 s default).
- **Lease-defaulting attacks.** An attacker who could write to the
  Lease object (would need RBAC equivalent to the controller's own
  ServiceAccount) could trick a replica into Yielding by holding the
  lease under a fake identity. This is no worse than the existing
  threat from compromising the controller SA itself, which already
  has cluster-wide write on every CRD the controller owns.
- **Empty-holder defensive branch.** A malformed Lease (empty
  `holderIdentity` with a fresh `renewTime`) makes us Yield rather
  than Acquire. Without that branch, two replicas would both see
  "empty holder + fresh time" → both treat it as expired → both take
  over → both write → defeats the gate. Covered by
  `empty_holder_identity_with_fresh_renew_yields_to_unknown_peer`.

## Existing implementation surveyed

- `controller/src/mesh_peer/mod.rs:490–597` — prior-art Lease loop for
  the mesh-peer subsystem. Same Lease semantics; we deliberately do
  *not* share code because the two subsystems have different ownership
  patterns and the duplicated ~100 LOC is clearer than a parameterised
  abstraction that would have to model both shapes.
- `deploy/helm/azureclaw/templates/rbac.yaml:55–61` — existing RBAC
  for `coordination.k8s.io/leases`.
- `deploy/helm/azureclaw/values.yaml:11,26` — `replicas: 2` already
  shipped, so this PR is the missing software gate for what the
  manifest already promised.

No new module duplicated existing code. No dead code carried.

## §14.6 / §15 impact

- §10.4 #11 (controller HA): closes the gap. Two-replica controller is
  now safe to roll out without doubled writes.
- §15.2 #10 (S7 craftsmanship): incremental progress; S7.C.2
  (predicated informers) and S7.D (backoff with jitter + reconcile-DAG)
  remain.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
