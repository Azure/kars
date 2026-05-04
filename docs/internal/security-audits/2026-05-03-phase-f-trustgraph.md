# Security audit — Phase F1: TrustGraph CRD + reconciler

**Date:** 2026-05-03
**Slice:** Phase F1 of the §14.6 competitive closure plan.
**Closes (partially):** "TrustGraph 0 LOC → cluster-wide AGT trust topology" entry in `docs/competitive.md` §14.6.
**Scope:** Cluster-scoped `TrustGraph` CRD, in-process reconciler, Helm CRD template, CEL admission rules, and signed-edge verification via Ed25519. **Phase F2 (router-side consultation) is deferred to a follow-up PR.**

## Summary

Adds a `TrustGraph` resource that lets operators declare a cluster-wide
trust topology — vertices (agent identities, each with an Ed25519
public key) and edges (signed `from → to` attestations carrying a
trust score in the existing AGT 0–1000 domain). The reconciler:

1. Verifies every vertex's public-key shape (alg = `EdDSA`, 32 bytes
   after base64url decode).
2. Verifies every edge's signature against the declared `from`
   vertex's public key, using a domain-separated canonical signing
   payload (`trustgraph.v1\n` prefix).
3. Drops invalid edges from the projection but **keeps them in the
   spec** — the operator owns the spec, the reconciler owns the
   projection. Status counters (`validEdges` / `invalidEdges`) and
   per-edge log lines surface rejections to operators.
4. Publishes the verified subset as a `ConfigMap`
   `trustgraph-{name}-projection` in the `azureclaw-system`
   namespace, with the version-hash annotation that consumers
   (Phase F2 router) use for change detection.
5. Cleans up the projection on CR delete via a finalizer
   (`azureclaw.azure.com/trustgraph-cleanup`) — cluster-scoped CR →
   namespaced ConfigMap precludes ownerRef cascade, so the finalizer
   is the only correct cleanup mechanism.

## STRIDE delta

| Threat | Pre-F1 | Post-F1 | Notes |
|---|---|---|---|
| **S**poofing | An on-cluster attacker writing a `TrustGraph` could elevate any AMID to score=1000 | Operator-supplied edges must carry an Ed25519 signature from the declared `from` vertex; tampered edges are rejected and counted | Vertex public keys are still operator-trusted (no external attestation issuer in F1) — the threat model is "operator authoring mistake or compromised CR write" rather than "compromised vertex key" |
| **T**ampering | n/a (capability did not exist) | Edge signature verified with `ed25519_dalek::Verifier::verify` (constant-time) over a domain-separated canonical payload (`trustgraph.v1\n`) | Domain prefix prevents cross-protocol replay (e.g. an A2AAgent AgentCard signature cannot be replayed as a TrustGraph edge) |
| **R**epudiation | n/a | Per-edge `reason` field + version-hash + RFC3339 `lastReconciledAt` give an audit trail | Phase D Merkle audit chain (separate work) hashes the projection ConfigMap into the integrity tree |
| **I**nformation disclosure | n/a | Projection ConfigMap omits operator-rejected edges entirely (no partial leakage); rejection reasons are a closed enum logged via tracing (no operator-supplied strings interpolated into log records) | Closed-enum reject reasons in `EdgeRejectReason::as_str` prevent log-injection via crafted CR strings |
| **D**enial of service | n/a | Reconciler is bounded by spec size; `compile_trust_graph` is O(V+E) with HashMap lookup per edge; no recursive expansion in F1 (transitive closure is router-side / Phase F2) | API-server CEL admission cap on object size already applies; no per-edge crypto budget needed at this volume |
| **E**levation of privilege | An operator who can write any CR could mint trust scores | CEL admission caps `score ≤ 1000`, requires `alg == 'EdDSA'`, requires non-empty `vertices`. Reconciler additionally validates `notAfter ≥ issuedAt`, signature bytes shape, and HashMap-based vertex resolution before crypto verification | Unchanged: cluster-scoped CR → only cluster-admins can author. Existing RBAC unchanged; new resource added to controller ClusterRole |

## OWASP-LLM mapping

- **LLM06 — Sensitive Information Disclosure.** Projection only contains operator-supplied data; no model output is fed back into the projection. Invalid edges are *omitted*, not partially copied — eliminates a class where a bad signature could nevertheless leak the `score` claimed. (Verified by `test_crd_trustgraph_reconcile`'s "projection excludes rejected edge" assertion.)
- **LLM07 — Insecure Plugin Design.** Phase F1 ships only the CRD + reconciler; the router-side trust consumer is deferred to F2, behind a separate audit and review. No router code paths consult the projection in this PR — the new code surface is isolated to the controller.

## Fail-closed semantics

- **Bad signature.** Edge is dropped from projection; counted in `status.invalidEdges`; reconciler still reports `Ready=True` (operator-data error, not controller error). Router consumers (Phase F2) MUST default to "no trust" when an edge is absent.
- **Apiserver write failure.** Reconciler enters `Degraded=True` with reason `ProjectionWriteFailed`; consumers reading the stale projection are protected by the version-hash annotation (the projection bytes don't change on a write failure).
- **CRD missing on startup.** `trust_graph_reconciler::run` parks indefinitely (`std::future::pending`) on `kube::Error` — consistent with `a2a_agent_reconciler` behaviour. Other reconcilers continue working; the controller does not crashloop.
- **Empty graph.** CEL rejects at admission (`size(self.vertices) > 0`). The reconciler will never see a valid CR with zero vertices.

## Scope deferrals (explicit)

- **No external attestation issuers.** Edge signatures are verified
  locally against vertices in the same CR. No call to Sigstore Rekor,
  OIDC IdPs, or any network endpoint. (User mandate: "no public
  posting".)
- **No transitive trust closure in F1.** The reconciler validates
  direct edges only. Multi-hop trust queries (`trust_score(a, c)`
  via `a → b → c`) belong to the router consumer in Phase F2.
- **No edge-revocation list.** Operators rotate trust by removing
  edges from the spec or setting a past `notAfter`. A separate
  revocation channel is not in F1's scope.
- **No router-side consumer.** `inference-router` does not yet read
  the projection. Phase F2 will add `trust_graph_projection.rs`
  (mirroring `a2a/agent_projection.rs`) and wire it into the existing
  AGT peer-trust path.

## Verification commands

```sh
# Unit tests (21 new, including signature-tamper and replay-prevention):
cargo test --package azureclaw-controller trust_graph

# Helm-drift gate:
cargo test --package azureclaw-controller helm_trustgraph

# Full controller suite:
cargo test --package azureclaw-controller    # 463 passed (was 440)

# Lint + fmt:
cargo clippy --package azureclaw-controller --all-targets -- -D warnings
cargo fmt --all --check

# Local E2E (kind):
make test-e2e   # exercises test_crd_trustgraph_reconcile
```

## Files changed

- `controller/src/trust_graph.rs` — CRD types (cluster-scoped).
- `controller/src/trust_graph_compile.rs` — pure verify step + 13 unit tests covering signature spoofing, expiry inversion, alg pinning, key-length, domain-separator replay, deterministic version hash.
- `controller/src/trust_graph_reconciler.rs` — reconcile loop + finalizer + 5 unit tests.
- `controller/src/crd_validations.rs` — `trust_graph_validations()` + `trust_graph_crd()` factories with 4 CEL rules.
- `controller/src/field_managers.rs` — `TRUST_GRAPH` SSA field manager.
- `controller/src/helm_drift.rs` — drift test for the new helm CRD.
- `controller/src/main.rs` — module wiring + `tokio::spawn` of the reconciler + `tokio::select!` arm.
- `deploy/helm/azureclaw/templates/crd-trustgraph.yaml` — Helm CRD mirror.
- `deploy/helm/azureclaw/templates/rbac.yaml` — `trustgraphs` + `trustgraphs/status` added to the controller ClusterRole.
- `tests/e2e/run.sh` — `test_crd_trustgraph_reconcile` (asserts CR → ConfigMap, validEdges=1, invalidEdges=1, version-hash annotation, projection excludes tampered edge, CEL rejects empty vertices, finalizer cleanup).
- `docs/security-audits/2026-05-03-phase-f-trustgraph.md` — this file.

## Sign-offs

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
