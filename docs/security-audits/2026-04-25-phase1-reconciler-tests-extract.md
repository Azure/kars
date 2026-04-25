# Phase 1 — `controller/src/reconciler.rs` tests extraction

**Date:** 2026-04-25
**Slug:** `phase1-reconciler-tests-extract`
**Branch:** `phase1/reconciler-tests-extract`
**Capability author:** Pal Lakatos-Toth
**Independent reviewer:** Pal Lakatos-Toth (Phase 1 single-reviewer carry-over;
see `docs/security-audits/2026-04-25-phase1-spawn-docker-extract.md` §11)

## 1. Summary

Decomposes `controller/src/reconciler.rs` (2326 LOC) — controller-side
reconcile loop for `ClawSandbox` — into a module directory:

- `reconciler/mod.rs` (1464 LOC) — production code: `Context`, `reconcile`,
  `error_requeue_duration`, `error_policy`, `run`, plus all helpers
  (`build_pod_security_context`, namespace / SA / CRB / NetworkPolicy /
  container builders, etc.). **Under the Phase 1 LOC cap of 1500.**
- `reconciler/tests.rs` (872 LOC) — pre-existing `#[cfg(test)] mod tests {…}`
  body, lifted verbatim. Carries a `// ci:loc-ok` override comment because
  splitting cohesive test groups across multiple files would harm
  reviewability for no security benefit (every test continues to exercise
  exactly the same crate-private helper it did before).

`include_str!` paths in `mod.rs` were updated from `../../cli/...` to
`../../../cli/...` to reflect the new directory depth (only operational
delta in this PR).

This change is **structure-only**. No production code path, control flow,
admission policy, K8s API call, conditional branch, error variant, or
emitted span has been altered. Test count, test names, and assertion
bodies are byte-identical to the merged `reconciler.rs` at `423110c`.

## 2. Threat model delta

None. Reconciler still owns the same K8s objects (Namespace, ServiceAccount,
ClusterRoleBinding, NetworkPolicy, Deployment, Service, ConfigMap), still
applies the same Pod Security Admission labels, still emits the same SSA
manager (`azureclaw-controller`), still propagates the same conditions
(`Ready`, `Degraded`) per KEP-1623. STRIDE surface unchanged.

## 3. OWASP mapping

- **MCP-Top10 / Excessive-Trust:** unchanged — the seccomp profile for
  `confidential` / `enhanced` / `standard` isolation is computed by the same
  `build_pod_security_context` function in `mod.rs` and still asserted by
  the same six tests in `tests.rs`.
- **LLM-Top10 / LLM07 Insecure-Plugin:** unchanged — default-egress
  NetworkPolicy still pinned to DNS:53 + IMDS + HTTPS-excluding-private +
  mesh-namespace + relay-namespace.

## 4. AuthN / AuthZ path

Unchanged. Workload Identity annotation, RBAC binding, and PSA labels are
emitted by the same code in `mod.rs`. The reconciler reads no agent input
and authenticates only via the controller's own Kubernetes API token.

## 5. Secret + key custody

Unchanged. Reconciler does not handle agent secrets; it only writes the
`*-credentials` Secret references into Deployment `envFrom` blocks. UID 1000
agent cannot read controller-side state.

## 6. Egress surface delta

None. The default NetworkPolicy contents are byte-identical (verified by
the `default_egress_*` tests in `tests.rs`).

## 7. Audit events emitted

Unchanged. Same `tracing` call sites, same fields, same K8s `Events`
emission. The reconcile span and its attributes (`sandbox`, `phase`,
`generation`) are emitted by the same code in `mod.rs`.

## 8. Failure mode

Fail-closed semantics preserved end-to-end. `error_policy` returns the same
`Action::requeue(...)` durations; `error_requeue_duration` table is
identical. Validation-failure exits still stamp `Degraded=True` /
`Ready=False` with `observedGeneration` per the existing KEP-1623 wiring.

## 9. Negative-test coverage

All previously merged negative cases continue to run from `tests.rs` —
isolation-mode confusion, non-root enforcement, empty SELinux context
suppression, NetworkPolicy missing-rule, expired pairing, malformed
`SerdeJson`. 136 controller bin tests pass post-split (unchanged from pre-
split count).

## 10. Vendored / third-party dependency delta

None. No new crates; no vendored patch touched.
`ci/vendored-patch-audit.sh` clean.

## 11. Sign-offs

Per Phase 0 carry-over policy (single-reviewer permitted for pure
structural moves with zero behaviour delta and zero new attack surface).

Signed-off-by: Pal Lakatos-Toth <pallakatos@github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@github.com>
