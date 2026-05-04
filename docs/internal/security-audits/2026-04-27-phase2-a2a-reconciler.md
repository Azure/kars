# Phase 2 S3 — `A2AAgent` reconciler + AgentCard compile

**Slice:** `phase2/a2aagent-reconciler`
**Closes:** §14.6 column 4 (A2A 1.2 + AP2 — schema + publication path).
**Depends on:** none structurally; aligns with S1 (JWKS Secret pattern) and S2 (compile-and-publish pattern).

## 0. Existing implementation surveyed (no-duplication rule)

Per the Phase 2 plan §0.2/§0.3, every slice's audit doc must enumerate the
seams it reuses. The reconciler in this slice is small precisely *because*
the substrate is already in place — both controller-side (S1 + S2) and
router-side (Phase 1 A2A modules). Reused seams:

| # | Seam | Where | Reused for |
|---|------|-------|-----------|
| 1 | Router-side `A2aAgentSpec` + `A2aAgentSigningKeySpec` projection | `inference-router/src/a2a/agent_projection.rs` (Phase 1) | The CRD's `spec.signingKeys[*]` shape (kid/alg/publicKeyB64u/notAfter) is **identical** to this projection — same field names, same camelCase serde — so the published AgentCard JSON traverses controller → ConfigMap → router with no transformation. |
| 2 | Trust-store rebuild orchestrator | `inference-router/src/a2a/snapshot_rebuild.rs` (Phase 1) | The eventual consumer (S7) of the ConfigMap published by this slice. No router-side wiring done in S3. |
| 3 | Trust-store / mandate-trust-store | `inference-router/src/a2a/trust_store.rs`, `mandate_trust_store.rs` (Phase 1) | Where projected `TrustAnchor` records will be installed in S7. |
| 4 | Status condition vocabulary (`TYPE_READY`/`TYPE_PROGRESSING`/`TYPE_DEGRADED`, `reason::*`, `preserve_transition_time`) | `controller/src/status/conditions.rs` (Phase 1) | A2AAgent emits the same condition shape as Sandbox + McpServer (S1) + ToolPolicy (S2). |
| 5 | Reconciler skeleton (Controller::new + non-fatal CRD-missing exit + finalizer + SSA via field manager + `error_policy`) | `controller/src/tool_policy_reconciler.rs` (S2) | Direct template; only the inner spec→artifact compile differs. |
| 6 | `LocalObjectRef` (status struct holding namespaced object ref) | `controller/src/mcp_server.rs::LocalObjectRef` (S1) | Reused verbatim for `status.agentCardConfigMapRef`. |
| 7 | Helm drift test pattern (canonical-form compare; bootstrap dump test) | `controller/src/helm_drift.rs` (S1, generalised in S2) | Extended in S3 to cover three CRDs (mcp + toolpolicy + a2aagent). No second module. |
| 8 | CRD admission-CEL injector (`inject_spec_validations`) + `ValidationRule` builder | `controller/src/crd_validations.rs` (Phase 1) | Three new CEL rules added (signingKeys non-empty, EdDSA-only, productionMode⇒https, federation in-cluster/external mutual exclusion). |
| 9 | RFC-3339 timestamp formatter (`rfc3339_now`) | `controller/src/mcp_server_reconciler.rs::rfc3339_now` (S1), `controller/src/tool_policy_reconciler.rs::rfc3339_now` (S2) | Same canonical format (`%Y-%m-%dT%H:%M:%SZ`) reproduced verbatim. Lifting to a shared module deferred to S7 craftsmanship pass — see same §10 follow-up logged in S2. |
| 10 | SSA field manager naming convention | §10.4 #1 of plan | New manager: `azureclaw-controller/a2aagent`. Distinct from `/mcp`, `/toolpolicy`, `/reconciler`, `/mesh`, `/pairing` — detects out-of-band tampering per-CR-type. |
| 11 | Finalizer DNS-subdomain pattern | `azureclaw.azure.com/sandbox-cleanup`, `…/mcp-cleanup`, `…/toolpolicy-cleanup` | New finalizer: `azureclaw.azure.com/a2aagent-cleanup`. |
| 12 | Error-class taxonomy (closed-set strings, log-injection-safe) | `mcp_server_reconciler::ReconcileError::class()`, `tool_policy_reconciler::ReconcileError::class()` | Same shape: `kube_api` / `serde`. |
| 13 | Upstream Microsoft AGT crate (`agentmesh` v3.1.0 from crates.io, **unmodified**) | `inference-router/src/providers/policy.rs` (in-process AGT consumer) | This slice **does not fork, patch, or parallel-implement AGT policy decisions**. The compiled AgentCard is data the router will hand to AGT TrustManager in S7; AGT decides. The vendored `vendor/` directory contains only AgentMesh transport (npm SDK + relay/registry from amitayks) and has zero overlap with this slice. |
| 14 | A2A 1.0 router code (existing routes, signing helpers, mandate verification) | `inference-router/src/a2a/*` (Phase 1) | Router-side surface for `/.well-known/agent.json` mount and `message/send` / `tasks/get` / `tasks/cancel` JSON-RPC endpoints **stays in S7**. S3 only emits the wire-format AgentCard JSON; it does not wire router routes. |

**New code introduced (justified):**

- `controller/src/a2a_agent.rs` — the CRD struct + status + sub-types. New file because §10.4 #1 requires one CRD module per kind (already the pattern: `mcp_server.rs`, `tool_policy.rs`).
- `controller/src/a2a_agent_compile.rs` — pure-function `compile_agent_card(spec, namespace, name) → serde_json::Value` + `version_hash`. Separated from the reconciler so it is unit-testable without a `kube::Client`. Output JSON is the wire-format A2A 1.2 AgentCard the router will serve verbatim once S7 mounts `/.well-known/agent.json`. No parallel AgentCard schema.
- `controller/src/a2a_agent_reconciler.rs` — the reconciler itself. Modelled on S2's `tool_policy_reconciler.rs`.

## 1. Threat model delta

An `A2AAgent` CR declares this agent's external A2A 1.2 identity: endpoint URL, federation peers, signing public keys, trust thresholds. The reconciler's role is: validate → compile to wire-format AgentCard JSON → publish to a ConfigMap. Threat surface:

| STRIDE | Concern | Mitigation |
|--------|---------|-----------|
| **Spoofing** (peer impersonation) | A federation peer claims an `agentRef` to an in-cluster A2AAgent the tenant doesn't actually own. | Out of scope for the reconciler — peer identity is verified by AGT TrustManager at decision time using the published signing keys. The reconciler emits the federation list verbatim. |
| **Spoofing** (key substitution) | An attacker with namespaced edit on `a2aagents` swaps `signingKeys[*].publicKeyB64u`. | Phase 1 admission CEL + S3-new CEL (`alg == 'EdDSA'`, `signingKeys` non-empty) enforce shape. RBAC is the operator's responsibility. The reconciler's helm drift test (`helm_a2aagent_crd_matches_rust_schema`) closes the path "ship a relaxed CRD via helm and bypass admission". |
| **Tampering** (replay across federation) | Replayed A2A messages between federated peers. | Out of scope for S3 — replay protection is a router-side concern (nonce/timestamp on `message/send`), already implemented in `inference-router/src/a2a/` Phase 1. The published AgentCard does not weaken this. |
| **Tampering** (kid collision across CRs) | Two `A2AAgent` CRs in different namespaces declare the same `kid`. | Phase 1 admission CEL ensures `kid` uniqueness **within a single CR**. Cluster-wide `kid` uniqueness is **deferred to S7**, where the trust-store informer will dedup at ingest. The router-side trust store keys on `(namespace, name, kid)` triples, so a collision is at worst a key-shadowing degradation, not an authentication bypass. |
| **Repudiation** | Operator denies authoring an AgentCard. | Reconciler emits an audit event `A2aAgentCompiled { name, namespace, version_hash, generation }` per reconcile. Hash is SHA-256 over the canonicalised AgentCard JSON — same input ⇒ same hash, immune to map-reordering. |
| **Information disclosure** | The published AgentCard leaks structure of internal federation peers to anyone with `get configmaps`. | The ConfigMap lives in the same namespace as the CR. AgentCards are **public objects by design** — the A2A 1.2 spec mandates `/.well-known/agent.json` is fetched unauthenticated by federated peers. The published JSON contains **no secret material** (only public signing keys, endpoint URLs, peer references). The signing-key Secret (private half) is **not generated in this slice** — see §9. |
| **Denial of service** | Operator authors thousands of `A2AAgent` CRs. | Reconciler is rate-limited by the controller-runtime's default (one in-flight reconcile per CR). Future hardening: per-namespace cap (Phase 3). |
| **Elevation of privilege** | An external federation peer is allowed to impersonate an in-cluster trust anchor. | Federation CEL rule forbids cross-namespace `agentRef` and requires `external` peers to ship an `endpointUrl` + `pinnedKid` pair. The router validates pinned kid at decision time (S7 wires this; the projection in `inference-router/src/a2a/agent_projection.rs` already handles pinned-kid records). |

## 2. OWASP A2A mapping (2026)

- **A2A-01 Cross-realm impersonation** — federation peers must declare `kind: external` and ship a pinned key. CEL forbids hybrid records.
- **A2A-03 Agent-card tampering** — AgentCard publication path is controller-only; ConfigMap ownership reference + finalizer + SSA field manager catch out-of-band edits. Cosign-signed AgentCards are Phase 3 (§10.4 #7 SLSA-on-CRs).
- **A2A-05 Trust anchor sprawl** — the projection-layer dedup keying `(namespace, name, kid)` bounds blast radius of duplicates.

## 3. Auth/authz path

The reconciler runs as the controller's ServiceAccount. It needs:

- `get/list/watch a2aagents.azureclaw.azure.com`
- `update a2aagents/finalizers`
- `patch a2aagents/status`
- `get/create/patch configmaps` in any namespace where an A2AAgent lives

**No new RBAC** beyond the CRD-management permissions Phase 1 already granted; the same ConfigMap-write permission used by the McpServer (S1) and ToolPolicy (S2) reconcilers covers A2AAgent-compiled AgentCards too.

## 4. Key custody

This slice handles **no private cryptographic material**. The CRD declares **public** signing keys (operator-supplied; format: base64url Ed25519 32-byte raw point). Private signing keys live in a Secret that is **not generated in this slice** — operators bring their own (the CRD's `spec.signingKeys[*].publicKeyB64u` is the public half they pass in). Auto-generation of the signing-key Secret is queued for S7.

The published AgentCard ConfigMap is `a2aagent-{name}-card`, key `agent.json`, label-selected for router-pod mount.

## 5. Egress surface delta

**Zero.** The reconciler is local-only (compiles spec to JSON, writes to API server). No outbound HTTP. Contrast with S1's `HttpJwksFetcher` — S3 has no equivalent.

## 6. Audit events

Reconciler emits the following structured tracing log lines (consumed by AGT AuditLogger via the in-process tracing layer):

- `A2aAgentCompiled { name, namespace, version_hash, generation, signing_key_count, federation_peer_count }` — every successful reconcile
- `A2aAgentCompileFailed { name, namespace, error_class, generation }` — when a CR escapes admission CEL with a malformed spec
- `A2aAgentDeleted { name, namespace }` — finalizer cleanup

`error_class` is one of a closed set (`kube_api` / `serde`), never a raw operator-supplied value (avoids log injection per §15.3).

## 7. Failure modes

| Failure | Reconciler behaviour |
|---------|---------------------|
| ConfigMap write fails (5xx) | `Degraded=True/CardWriteFailed`, requeue 60s. Router keeps last-known AgentCard; trust-store rebuild is generation-counted (Phase 1), missing artifact ⇒ no overwrite. |
| Spec malformed (e.g. CEL bypass) | `Degraded=True/SpecInvalid`, requeue 60s. ConfigMap is **not** written — router never observes a half-compiled AgentCard. |
| Finalizer cleanup fails (ConfigMap delete 5xx) | `Degraded=True`, requeue 60s. Finalizer not removed; CR stays in `Terminating` until cleanup succeeds. Mirrors S1 + S2 + Phase 1 sandbox reconciler. |
| CR deleted between list and reconcile | Standard kube-rs reconcile-loop semantics; the next event drives cleanup. |
| Concurrent reconciler replicas | SSA via field manager `azureclaw-controller/a2aagent` keeps the writes idempotent + conflict-detectable. Leader election is S7. |

## 8. Negative-test coverage

Unit tests assert:

1. `compile_minimal_spec_yields_protocol_version_and_endpoint` — minimal valid CR ⇒ AgentCard with `protocolVersion: "1.2"` + `endpointUrl`.
2. `compile_full_spec_round_trips` — every A2AAgent field surfaces in the compiled AgentCard exactly once.
3. `compile_is_deterministic` — same spec ⇒ same JSON byte-for-byte (canonicalised key order).
4. `version_hash_changes_on_spec_change` — modifying `endpointUrl` flips the hash; modifying nothing keeps it.
5. `version_hash_includes_namespace_and_name` — same spec in two different `(namespace, name)` produces distinct hashes (defends against cross-namespace AgentCard substitution).
6. `version_hash_is_16_bytes_hex` — closed-set assertion on hash shape.
7. `version_hash_is_stable_across_serde_round_trip` — JSON deserialisation key order does not affect the hash.
8. `error_class_is_closed_set` — every `ReconcileError` variant maps to one of `kube_api` / `serde`. (Log-injection prevention.)
9. `condition_matrix_emits_all_three_types` — Ready + Progressing + Degraded all present after every reconcile.
10. CEL gates: `a2a_agent_validations_are_non_empty`, `every_a2a_agent_rule_has_message_and_rule`, `a2a_agent_crd_has_spec_validations_after_injection`, `a2a_agent_rules_mention_signing_keys_and_eddsa_invariants`, `a2a_agent_crd_is_serde_round_trippable`.
11. `field_manager_is_per_reconciler` — confirms `azureclaw-controller/a2aagent` is distinct from sibling reconcilers.
12. Helm drift: `helm_a2aagent_crd_matches_rust_schema` — cargo-test-time gate against schema drift between Rust source and helm chart.

## 9. Out of scope (deferred — explicit list)

- **Signing-key Secret auto-generation** — S7. CRD `spec.signingKeys[]` is operator-supplied (CEL: `size(self.signingKeys) > 0`).
- **Router-side `/.well-known/agent.json` mount** — S7. The published ConfigMap is consumable; the router-side serve path is wired in S7.
- **Trust-store informer wiring** (ConfigMap → `Vec<TrustAnchor>` ingest into router) — S7. The pure-function projection already lives at `inference-router/src/a2a/agent_projection.rs`.
- **JSON-RPC `message/send` / `tasks/get` / `tasks/cancel` route mounts** — S7.
- **AgentCard JWS signing** (vs. plain JSON publication) — S7.
- **Cluster-wide kid uniqueness enforcement** — S7 informer-side dedup (current per-CR uniqueness via projection-layer `(namespace, name, kid)` keying).
- **Cross-namespace federation `agentRef`** — explicitly forbidden by CEL; use `kind: external` with a full URL+pin instead.
- **Cosign-signed AgentCards on admission** — Phase 3 (§10.4 #7 SLSA-on-CRs).

## 10. Verification

| Gate                                  | Result   |
| ------------------------------------- | -------- |
| `cargo fmt --all -- --check`          | ✅ pass |
| `cargo build --all`                   | ✅ pass |
| `cargo test --workspace`              | ✅ 193 controller (incl. 16 new for S3) / 595 router / 26 controller integration / 0 failures |
| `cargo clippy --all-targets -- -D warnings` | ✅ pass |
| `ci/check-loc.sh`                     | ✅ pass (BASE_REF=origin/dev) |
| `ci/no-stubs.sh`                      | ✅ pass |
| `ci/no-custom-crypto.sh`              | ✅ pass |
| `ci/security-audit-required.sh`       | ✅ pass |
| `ci/no-null-provider-prod.sh`         | ✅ pass |
| `ci/a2a-module-isolation.sh`          | ✅ pass |
| `ci/vendored-patch-audit.sh`          | ✅ pass |
| Helm CRD drift test (a2aagent)        | ✅ `helm_drift::tests::helm_a2aagent_crd_matches_rust_schema` passes |
| CLI `npm run typecheck`               | ✅ pass |
| CLI `npm run lint`                    | ✅ pass (pre-existing warnings, 0 errors) |

## Sign-offs

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
