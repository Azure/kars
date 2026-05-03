# Phase F2b — TrustGraph per-sandbox projection mount

**Date:** 2026-05-03
**Branch:** `feat/trustgraph-mount`
**Closes:** §14.6 TrustGraph item (controller-side completion)
**Depends on:** F1 (`trustgraph_reconciler`) + F2a (router-side loader)
**Unblocks:** F2a production enablement (the audit doc for F2a explicitly
called out F2b as a prerequisite for non-test deployment).

## Summary

The F1 reconciler publishes one cluster-wide projection `ConfigMap` per
`TrustGraph` CR into the `azureclaw-system` namespace. Every projection
contains the **entire signed trust topology** for that CR — every edge,
every vertex pubkey. Mounting that blob into every sandbox would be a
LLM06 (Sensitive Information Disclosure) violation: a compromised
sandbox could exfiltrate the operator's trust intelligence.

F2b mints a **per-sandbox slice** in each sandbox's own namespace,
filtered to **outbound edges only** (edges whose `from == sandbox_name`),
and mounts it into the inference-router at
`/etc/azureclaw/trustgraph/graph.json` with
`TRUSTGRAPH_PROJECTION_PATH` set so the F2a loader picks it up.

## Design

| Aspect | Decision | Rationale |
|---|---|---|
| Filter rule | Outbound edges only (`from == sandbox_name`) | F2a's bootstrap path is `direct_edge(sandbox_name, peer)` — only outbound matters. Inbound edges are operator-private intel that must NOT leak into the sandbox. |
| Vertex set | Restricted to vertices referenced by surviving edges | Prevents the slice from disclosing pubkeys of unrelated identities. |
| Self-edge handling | Defence-in-depth filter (`from == to` dropped) | F1 already filters; we re-filter on the trust boundary. |
| Empty slice | Empty CM published anyway | Stable mount across pod restarts; absent file = router fail-closes. |
| No-source case | No CM written | Matches prior behaviour; router fail-closes on missing env var. |
| Field manager | `azureclaw-controller/trustgraph-mount` (NEW) | Keeps SSA ownership of cluster-wide projections (F1) separable from per-sandbox slices (F2b). |
| Mount path | `/etc/azureclaw/trustgraph/graph.json` | Matches existing `paths::*_DIR` convention in `governance_mounts`. |
| Env var | `TRUSTGRAPH_PROJECTION_PATH` | Already consumed by F2a `trust_graph_loader`. Drift would silently break consultation. Pinned in unit test `paths_match_router_loader_expectations`. |
| Error policy | Logged at `warn`, non-fatal | F2a loader fails closed → router behaves identically to pre-F2 if mount fails. |

## STRIDE delta vs F2a

| Threat | F2a posture (pre-F2b) | F2b mitigation |
|---|---|---|
| **Information Disclosure** — sandbox reads cluster-wide trust topology | Documented limitation; F2a doc said "non-production until F2b" | ✅ Per-sandbox slice. Outbound edges only. Vertex set pruned. |
| **Tampering** — operator overwrites the projection CM | Cluster-wide CM is in `azureclaw-system`; default ABAC blocks bystanders. Per-sandbox CM lives in the sandbox namespace where the sandbox SA has only read access (no write). | Source CM still requires admission policy (deferred — see "Deferred" below). Per-sandbox CM is owned by the controller via SSA field manager `trustgraph-mount`; conflicting writes fail. |
| **Spoofing** — fake TrustGraph CR injected | F1 verifies edge signatures against vertex pubkeys before publishing | F2b inherits F1's verification; no re-verify needed (signed payload already filtered out). |
| **Repudiation** | F1 reconcile emits Conditions; F2b emits `tracing::info!` per mount with version_hash + edge count | ✅ |
| **Denial of Service** — large projection causes OOM in router | F2a `MAX_PROJECTION_BYTES = 1 MiB` cap | ✅ Inherited; per-sandbox slice is by construction smaller than the source. |
| **Elevation of Privilege** — sandbox uses TrustGraph data to bypass AGT | TrustGraph bootstrap caps at score 500 (`min(500)`) and only fires on `is_new` interactions | ✅ Inherited unchanged. |

## OWASP-LLM mapping

| Risk | Status |
|---|---|
| LLM02 — Insecure Output Handling | N/A (no model output here) |
| LLM06 — Sensitive Information Disclosure | ✅ Mitigated. Slice filter is the primary control. |
| LLM07 — Insecure Plugin Design | N/A |
| LLM10 — Model Theft | N/A |

## Code changes

| File | Δ | Purpose |
|---|---|---|
| `controller/src/reconciler/trustgraph_mount.rs` | +389 LOC (new) | Pure filter + SSA helper. 8 unit tests. |
| `controller/src/reconciler/mod.rs` | +mod decl + ~45 LOC call site | Wires F2b into the sandbox reconcile flow after the existing governance mounts. |
| `controller/src/field_managers.rs` | +`TRUSTGRAPH_MOUNT` constant | New SSA field manager. |
| `tests/e2e/run.sh` | +per-sandbox-mount asserts in `test_crd_trustgraph_reconcile` | Verifies CM creation, slice contents, env var injection, volume mount. |

## Test results

```
cargo test -p azureclaw-controller   →  471 passed (+8 vs F2a baseline)
cargo clippy --all-targets -D warns  →  clean
cargo fmt --all                      →  clean
```

## Deferred (out of scope for this PR — tracked in plan)

- **Source CM admission policy** (Gatekeeper/Kyverno) restricting
  `update`/`patch` on `*-projection` CMs in `azureclaw-system` to the
  controller ServiceAccount. The F1 reconciler already owns these via
  SSA field manager, so an unprivileged operator cannot stomp them
  without first stealing the controller SA token. Adding the explicit
  admission policy is belt-and-braces and can ship in a follow-up.
- **Sandbox-namespace RBAC tightening**: today the sandbox SA inherits
  the namespace's default permissions. The F2b CM lives in the sandbox
  namespace with the controller as SSA owner, so the sandbox can read
  but not write. A formal `Role` restricting the sandbox SA to `get`
  on its own projection CM (denying `list` of others) is a follow-up.

## Verification commands

```bash
# Apply F1 fixture
kubectl apply -f fixtures/trustgraph/alpha-beta.yaml
# Apply a ClawSandbox named `alpha`
kubectl apply -f fixtures/sandbox/alpha.yaml
# Verify per-sandbox CM appears
kubectl get cm alpha-trustgraph-projection -n azureclaw-alpha -o yaml
# Verify env var on inference-router
kubectl get deploy alpha -n azureclaw-alpha -o jsonpath='{.spec.template.spec.containers[?(@.name=="inference-router")].env[?(@.name=="TRUSTGRAPH_PROJECTION_PATH")]}'
# Trigger router restart and check metric
kubectl rollout restart deploy alpha -n azureclaw-alpha
kubectl exec deploy/alpha -n azureclaw-alpha -c inference-router -- \
  curl -s localhost:9090/metrics | grep azureclaw_agt_trustgraph_bootstraps_total
```

## Co-authors

`Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`
