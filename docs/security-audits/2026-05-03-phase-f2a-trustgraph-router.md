# Phase F2a — TrustGraph router consultation (bootstrap-only)

| Field | Value |
|-------|-------|
| Audit date | 2026-05-03 |
| Phase | F2a — router-side TrustGraph reader + AGT bootstrap hook |
| Author | Copilot (drafting) + Pal Lakatos-Toth (sign-off) |
| Branch | `feat/trustgraph-router-consultation` |
| Depends on | F1 — `TrustGraph` CRD + reconciler (PR #176, merged) |
| Blocks | F2b — controller-side per-sandbox projection mount |

## 1. Scope

This phase wires the `inference-router` to consume the `ProjectedGraph`
JSON document the controller-side `TrustGraph` reconciler publishes to
`ConfigMap[ns=azureclaw-system, name=trustgraph-<n>-projection]
.data["graph.json"]` (Phase F1).

A new module — `inference-router/src/a2a/trust_graph_projection.rs` —
parses the document into an in-memory lookup table. A loader sibling
— `inference-router/src/a2a/trust_graph_loader.rs` — reads it from the
filesystem path named by the `TRUSTGRAPH_PROJECTION_PATH` env var. A
single hook in `governance::trust_ops::update_trust` consults the
projection on the **bootstrap path only** (peers with zero AGT
interactions): if a controller-verified edge `<sandbox> → <peer>`
exists and is not expired, its score is used to seed the AGT trust
score, capped at the existing 500 maximum.

**Explicitly out of scope (defer to F2b):**

- Per-sandbox projection ConfigMap mounting (the controller change).
- Cross-namespace ConfigMap watch / refresh.
- Edge revocation channel (currently only `notAfter` time-based expiry).
- Transitive closure (multi-hop inference).
- Operator-facing endpoint to inspect the loaded projection.
- Re-verification of edge signatures inside the router (the controller
  is the trust root for verification — see §4.1).

## 2. Threat model delta vs. pre-F2

### 2.1 STRIDE delta

| Threat                       | Pre-F2 posture                      | Post-F2a posture                                   |
|------------------------------|--------------------------------------|----------------------------------------------------|
| **S**poofing                 | AGT KNOCK + Ed25519 peer identity    | Unchanged. Bootstrap does not bypass identity verification. |
| **T**ampering                | AGT trust file written under `/tmp/agt` | New input vector: projection JSON file. Default off. See §4.1. |
| **R**epudiation              | AuditLogger row per trust update     | Additional `trustgraph_bootstrap:<score>` audit row per bootstrap. |
| **I**nformation disclosure   | None                                 | An attacker with read access to the projection file learns the operator's trust topology. F2b narrows by per-sandbox slice. |
| **D**enial of service        | Foundry rate-limits + AGT rate-limiter | Loader is fail-closed (empty projection on parse error). 1 MiB cap. |
| **E**levation of privilege   | AGT cap of 500 on `requested_score`  | Cap unchanged. Bootstrap cannot exceed `min(500)`. |

### 2.2 OWASP-LLM mapping

- **LLM01 Prompt Injection** — n/a, this path is below the prompt layer.
- **LLM03 Supply Chain** — projection file is a new supply-chain link.
  Mitigated by RBAC on the source ConfigMap (controller field-manager
  guard) and by F2b's per-sandbox slicing. Default-off until F2b lands.
- **LLM06 Sensitive Information Disclosure** — the projection contains
  agent identities and (in F1's full form) the entire trust topology.
  F2b mitigates by mounting per-sandbox slices only.
- **LLM10 Model Theft** — n/a.

## 3. Fail-closed semantics

Every failure path in F2a yields `TrustGraphProjection::empty()`:

| Condition                                  | Behaviour                                  |
|--------------------------------------------|--------------------------------------------|
| `TRUSTGRAPH_PROJECTION_PATH` unset         | Empty projection. `tracing::info!`.        |
| File missing at the path                   | Empty projection. `tracing::info!`.        |
| File present but oversize (> 1 MiB)        | Empty projection. `tracing::warn!`.        |
| File present but malformed JSON            | Empty projection. `tracing::warn!`.        |
| Edge expired (`notAfter < now`)            | Edge silently dropped at lookup time.      |
| Edge `from == to` (self-attestation)       | Edge silently dropped (defence in depth).  |
| Peer has prior AGT interactions            | Bootstrap path skipped; AGT owns the score.|
| Edge score > 500                           | Capped to 500 by the existing AGT rule.    |

An empty projection is **functionally indistinguishable** from
pre-F2 behaviour: `update_trust` falls through to the original
`requested_score.min(500)` path.

## 4. Production readiness gates

### 4.1 Why the router does not re-verify edge signatures

The controller's `compile_trust_graph` Ed25519-verifies every edge
against the declared `from` vertex's public key, with a domain-prefixed
canonical payload (`b"trustgraph.v1\n…"`) that is locked at v1. Edges
that fail verification are dropped before publication; only verified
edges reach the projection ConfigMap. Re-verifying inside the router
would:

1. Require shipping all vertex public keys into every sandbox — an
   information-disclosure regression (LLM06).
2. Duplicate the canonical-payload code on the trust boundary, exactly
   what `ci/no-custom-crypto.sh` is designed to prevent.
3. Double the per-refresh cost.

**Required compensating control (production):** the source ConfigMap
in `azureclaw-system` MUST be writeable only by the controller
ServiceAccount. The existing SSA field-manager (`azureclaw-controller/trustgraph`)
provides last-write-wins protection; production deployments MUST
additionally apply a Gatekeeper / Kyverno admission policy that
restricts `update`/`patch` on
`configmaps` labeled `azureclaw.azure.com/artifact=trustgraph-projection`
to that ServiceAccount. **F2b will ship the policy template.**

### 4.2 Why the bootstrap fires only on `interactions == 0`

After even one observed interaction, AGT's TrustManager is the
authoritative source. Re-bootstrapping a peer whose live score has
fallen due to `record_failure` would erase the operational signal —
i.e. an attacker who triggers a content-flag penalty could regain
trust by causing the projection to be reloaded. The `is_new` branch
gate prevents that.

### 4.3 Default-off in production until F2b

The Helm chart MUST NOT set `TRUSTGRAPH_PROJECTION_PATH` until F2b
ships the per-sandbox projection mount. Tests rely on the env var
explicitly; production deployments rely on its absence. F2b will:

1. Add the per-sandbox projection ConfigMap to the sandbox reconciler.
2. Mount it at `/etc/azureclaw/trustgraph/graph.json`.
3. Set the env var in the router container.
4. Add the admission policy described in §4.1.

## 5. Observability

| Metric / signal                                     | Where                                            | Use                                                     |
|-----------------------------------------------------|--------------------------------------------------|---------------------------------------------------------|
| `azureclaw_agt_trustgraph_bootstraps_total`         | `inference-router/src/metrics.rs`                | Alert: rate exceeds expected provisioning cadence.      |
| `azureclaw_agt_trustgraph_projection_version{hash}` | `inference-router/src/metrics.rs`                | Confirm all sandboxes observe the same projection.      |
| Audit row: `trustgraph_bootstrap:<score>`           | AGT AuditLogger                                  | Forensic trail per bootstrap.                           |
| `tracing::info!` "TrustGraph projection loaded"     | `trust_graph_loader.rs`                          | Boot-time confirmation; one line per pod start.         |

**Recommended alert (operator-tunable):** `rate(azureclaw_agt_trustgraph_bootstraps_total[1h]) > 10`
on a single sandbox is a red flag — either an attacker has caused
mass peer enrollment, or a configuration drift pushed an updated
projection that the sandbox is treating as new peers.

## 6. Test coverage

- 14 unit tests in `a2a::trust_graph_projection::tests` —
  parse fixture, lookup hit/miss, self-edge filter, expiry, empty
  object, malformed JSON, oversize, unknown top-level field, duplicate
  edge dedup, optional-field round-trip, no-`notAfter` no-expiry.
- 4 integration tests in `governance::tests` —
  bootstrap from edge, no-override existing peer, no-op without
  projection, self-edge rejection.

Total: 18 new tests. Router suite: 632 passed (was 614).

## 7. Sign-off

| Role     | Name                  | Status                  |
|----------|-----------------------|-------------------------|
| Author   | Copilot               | ✅ Drafted               |
| Reviewer | Pal Lakatos-Toth      | _pending PR review_     |
| Security | Pal Lakatos-Toth      | _pending PR review_     |

## 8. References

- F1 audit doc: `docs/security-audits/2026-05-03-phase-f-trustgraph.md`
- AGT TrustManager: `agentmesh` crate (workspace dep)
- A2A module isolation: `ci/a2a-module-isolation.sh`
- No-custom-crypto gate: `ci/no-custom-crypto.sh`
- Plan §14.6 line item: TrustGraph 0 LOC → cluster-wide AGT trust topology
