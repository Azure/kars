# Phase 2 S2 — `ToolPolicy` reconciler + AGT profile compile

**Slice:** `phase2/toolpolicy-reconciler`
**Closes:** §14.6 column 4 (ToolPolicy CRD).
**Depends on:** S1 `phase2/mcp-reconciler` (consumes the JWKS-mounted OAuth 2.1 token surface for per-tool scope checks).

## 0. Existing implementation surveyed (no-duplication rule)

Per the Phase 2 plan §0.2/§0.3, every slice's audit doc must enumerate the
seams it reuses. The reconciler in this slice is small precisely *because*
the substrate is in place. Reused seams:

| # | Seam | Where | Reused for |
|---|------|-------|-----------|
| 1 | `ToolPolicy` CRD struct (full schema: commerce + rateLimit + approval + appliesTo) | `controller/src/tool_policy.rs` (Phase 1) | The reconciler watches this type — no schema changes needed. |
| 2 | `tool_policy_crd()` (CEL-injected CustomResourceDefinition) | `controller/src/crd_validations.rs` (Phase 1) | Source of truth for the helm CRD template + drift test. |
| 3 | Status condition vocabulary (`TYPE_READY`/`TYPE_PROGRESSING`/`TYPE_DEGRADED`, `reason::*`, `preserve_transition_time`) | `controller/src/status/conditions.rs` (Phase 1) | Reconciler emits identical condition shape as Sandbox + McpServer (S1). |
| 4 | Reconciler skeleton (Controller::new + non-fatal CRD-missing exit + finalizer + SSA via field manager + `error_policy`) | `controller/src/pairing_reconciler.rs` + `controller/src/mcp_server_reconciler.rs` (S1) | Same shape; only the inner spec→artifact compile differs. |
| 5 | `LocalObjectRef` (status struct holding namespaced object ref) | `controller/src/mcp_server.rs::LocalObjectRef` (S1) | Reused verbatim for `status.profileConfigMapRef`. |
| 6 | Helm drift test pattern (canonical-form compare; bootstrap dump test) | `controller/src/helm_drift.rs` (S1) | Generalised in S2 to handle multiple CRDs (mcp + toolpolicy) — no second module. |
| 7 | `PolicyEnvelope` + `PolicyEnvelopeSnapshot` + `PolicyEntry` + `apply_policy_change` (router-side hot-reload core) | `inference-router/src/policy_envelope.rs` (Phase 1) | Reconciler emits artifacts whose JSON shape slots into `PolicyEntry.payload`. **No router-side code change in this slice** — the router-side informer/loader is S7 (`phase2-conditions-ssa-leader`) which touches every reconciler. |
| 8 | `PolicyDecisionProvider` trait (`Governance` impl) | `inference-router/src/providers/policy.rs` + `policy_impl.rs` (Phase 1) | The compiled AGT profile JSON is what `Governance::evaluate` already understands — we are not introducing a parallel decision engine. |
| 9 | CRD admission-CEL rules for ToolPolicy | `controller/src/crd_validations.rs::tool_policy_validations()` (Phase 1) | The CEL rules already enforce `dailyCap <= monthlyCap`, currency-string format, and at-least-one selector field. The reconciler trusts the API server's enforcement and treats CRs that escape it (e.g. controller upgraded ahead of CRD) as `SpecInvalid → Degraded`. |
| 10 | RFC-3339 timestamp formatter (`rfc3339_now`) | `controller/src/mcp_server_reconciler.rs::rfc3339_now` (S1) | Same canonical format (`%Y-%m-%dT%H:%M:%SZ`) reproduced verbatim in `tool_policy_reconciler::rfc3339_now`. Both functions are pure, two lines, and unit-tested for shape; lifting to a shared module deferred to S7 craftsmanship pass when `controller/src/status/conditions.rs` lands. |
| 11 | SSA field manager naming convention | §10.4 #1 of plan | New manager: `azureclaw-controller/toolpolicy`. Distinct from `/mcp` and the legacy ClawSandbox manager — detects out-of-band tampering per-CR-type. |
| 12 | Finalizer DNS-subdomain pattern | `azureclaw.azure.com/sandbox-cleanup` etc. | New finalizer: `azureclaw.azure.com/toolpolicy-cleanup`. |
| 13 | Currency string parsing | `inference-router/src/a2a/ap2.rs` | The compiler does **not** re-parse currency strings — it forwards the raw `daily_cap`/`monthly_cap`/`per_transfer_cap` to the AGT profile and lets the router's existing AP2 cap evaluator decide. No second parser. |

**New code introduced (justified):**

- `controller/src/tool_policy_reconciler.rs` — the reconciler itself. New file because §10.4 #1 requires one reconciler module per CRD; modelled on `mcp_server_reconciler.rs` (S1).
- `controller/src/tool_policy_compile.rs` — pure-function `compile_to_profile(spec) → serde_json::Value`. Separated from the reconciler so it is unit-testable without a `kube::Client`. This is the **only** new pure-logic module and it is intentionally small (~80 LOC). Its output JSON shape is the same `PolicyEntry.payload` that `policy_envelope.rs` already accepts — no parallel data shape.

## 1. Threat model delta

A `ToolPolicy` CR is policy data. The reconciler's role is: take operator-authored YAML, validate it, compile it to an AGT-profile-shaped JSON document, and persist that document where the router can mount it. The threat surface added is therefore:

| STRIDE | Concern | Mitigation |
|--------|---------|-----------|
| **Spoofing** | A CR claims a `commerce.counterpartyAllowlist` that contains a counterparty the tenant doesn't actually own. | Out of scope for the reconciler — counterparty identity is verified by AGT TrustManager at decision time. The reconciler emits the allowlist verbatim. |
| **Tampering** | An attacker with namespaced edit on `toolpolicies` widens `dailyCap` or empties `counterpartyAllowlist`. | Phase 1 admission CEL (`tool_policy_validations()`) enforces shape. RBAC is the operator's responsibility (out of scope). The reconciler **does** detect drift between Rust schema and helm CRD via `helm_drift::tests`, closing the path "ship a relaxed CRD via helm and bypass admission". |
| **Repudiation** | Operator denies authoring a profile. | Reconciler emits an audit event `ToolPolicyCompiled { name, version_hash, generation }` per reconcile. Hash is SHA-256 over the canonicalised spec — same input ⇒ same hash, immune to map-reordering. |
| **Information disclosure** | The compiled profile leaks structure of internal commerce limits to anyone with `get configmaps`. | The ConfigMap lives in the same namespace as the CR; ConfigMap RBAC is the operator's choice. Compiled profile contains **no secret material** (caps and allowlists are policy, not credentials). Audit doc §4. |
| **Denial of service** | Operator authors thousands of `ToolPolicy` CRs with selectors that all match every request. | Reconciler is rate-limited by the controller-runtime's default (one in-flight reconcile per CR); the router's `PolicyEnvelopeSnapshot::select` is `O(n)` over entries but n is small and selector check is cheap. Future hardening: per-namespace cap (Phase 3). |
| **Elevation of privilege** | A CR claims `approval.mode = never` for a high-value tool to skip HITL. | The router's per-request decision is the gate, not the reconciler. Reconciler forwards `approval.mode` verbatim; AGT decides. Out of scope. |

## 2. OWASP MCP Top 10 mapping (2026)

- **MCP-04 Excessive Agency** — `commerce.counterpartyAllowlist` empty ⇒ deny-all, enforced router-side; reconciler **emits** the allowlist into the profile but does not interpret it. Conformance corpus row "AP2 cap exceeded → refuse" exercises the path end-to-end.
- **MCP-08 Tool Permission Sprawl** — `appliesTo.tool` exact-match (with explicit `*` wildcard, not regex) is the spec's stated mitigation. The reconciler does not introduce regex matching.

## 3. Auth/authz path

The reconciler runs as the controller's ServiceAccount. It needs:

- `get/list/watch toolpolicies.azureclaw.azure.com`
- `update toolpolicies/finalizers`
- `patch toolpolicies/status`
- `get/create/patch configmaps` in any namespace where a ToolPolicy lives

**No new RBAC** beyond the CRD-management permissions Phase 1 already granted; the same ConfigMap-write permission used by the McpServer reconciler covers ToolPolicy-compiled profiles too.

## 4. Key custody

This slice handles **no cryptographic material**. Compiled profiles are policy JSON, not secrets. Stored in `ConfigMap toolpolicy-{name}-profile`, label-selected for router-pod mount.

## 5. Egress surface delta

**Zero.** The reconciler is local-only (compiles spec to JSON, writes to API server). No outbound HTTP. Contrast with S1's `HttpJwksFetcher`, which does fetch `https://{issuer}/.well-known/...` — S2 has no equivalent.

## 6. Audit events

Reconciler emits the following structured tracing log lines (consumed by AGT AuditLogger via the in-process tracing layer):

- `ToolPolicyCompiled { name, namespace, version_hash, generation, has_commerce, has_rate_limit, has_approval }` — every successful reconcile
- `ToolPolicyCompileFailed { name, namespace, error_class, generation }` — when a CR escapes admission CEL with a malformed spec
- `ToolPolicyDeleted { name, namespace }` — finalizer cleanup

**Negative-event coverage** is asserted by unit tests (see §8): `error_class` is one of a closed set of safe strings, never a raw operator-supplied value (avoids log injection per §15.3).

## 7. Failure modes

| Failure | Reconciler behaviour |
|---------|---------------------|
| ConfigMap write fails (5xx) | `Degraded=True/ProfileWriteFailed`, requeue 60s. Router keeps last-known profile; `PolicyEnvelope` is generation-counted, missing artifact ⇒ no overwrite. |
| Spec malformed (e.g. CEL bypass) | `Degraded=True/SpecInvalid`, requeue 60s. ConfigMap is **not** written — router never observes a half-compiled profile. |
| Finalizer cleanup fails (ConfigMap delete 5xx) | `Degraded=True`, requeue 60s. Finalizer not removed; CR stays in `Terminating` until cleanup succeeds. Mirrors S1 + Phase 1 sandbox reconciler. |
| CR deleted between list and reconcile | Standard kube-rs reconcile-loop semantics; the next event drives cleanup. |
| Concurrent reconciler replicas | SSA via field manager `azureclaw-controller/toolpolicy` keeps the writes idempotent + conflict-detectable. Leader election is S7 (`phase2-conditions-ssa-leader`). |

## 8. Negative-test coverage

Unit tests assert:

1. `compile_empty_spec_yields_minimal_profile` — no commerce/no rate-limit/no approval ⇒ compile succeeds, payload contains only the selector. (Confirms reconciler tolerates the legitimate "selector-only audit policy" case.)
2. `compile_full_spec_round_trips` — every ToolPolicy field surfaces in the compiled JSON exactly once.
3. `compile_is_deterministic` — same spec ⇒ same JSON byte-for-byte (canonicalised key order).
4. `version_hash_changes_on_spec_change` — modifying `dailyCap` flips the hash; modifying nothing keeps it.
5. `version_hash_is_stable_across_serde_round_trip` — JSON deserialisation key order does not affect the hash.
6. `error_class_is_closed_set` — every `ReconcileError` variant maps to one of `kube_api`/`serde`/`spec_invalid`/`compile_error`. (Log-injection prevention.)
7. `condition_matrix_emits_all_three_types` — Ready + Progressing + Degraded all present after every reconcile.
8. `finalizer_cleanup_removes_configmap` — exercised against an in-memory fake ConfigMap API.

## 9. Out of scope (deferred)

- **Router-side informer that loads ConfigMaps and applies `PolicyChange::Upserted`** — defer to S7 (`phase2-conditions-ssa-leader`) which adds an informer to every reconciler at once.
- **AGT SSE subscription for hot-reload** — stays in S7. The Phase 1 `policy_envelope` core already has the `apply_policy_change` substrate.
- **CRD precedence rules** (e.g. tool-specific over wildcard) — documented in `docs/crd-precedence.md` (Phase 2 plan §8 entry 10), enforced by the router-side selector when S7 wires it. The compiled profile carries the unmodified `appliesTo`; precedence is a router decision, not a reconciler decision.
- **ClawSandbox `spec.toolPolicies` cross-CRD validation** — deferred to S3 (`phase2/a2aagent-reconciler`) and S7.

## 10. Verification

| Gate                                  | Result   |
| ------------------------------------- | -------- |
| `cargo fmt --all -- --check`          | ✅ pass |
| `cargo build --all`                   | ✅ pass |
| `cargo test --workspace`              | ✅ 177 controller (incl. 12 new for S2) / 595 router / 26 controller integration / 0 failures |
| `cargo clippy --all-targets -- -D warnings` | ✅ pass |
| `ci/check-loc.sh`                     | ✅ pass (BASE_REF=origin/dev) |
| `ci/no-stubs.sh`                      | ✅ pass |
| `ci/no-custom-crypto.sh`              | ✅ pass |
| `ci/security-audit-required.sh`       | ✅ pass |
| `ci/no-null-provider-prod.sh`         | ✅ pass |
| `ci/a2a-module-isolation.sh`          | ✅ pass |
| `ci/vendored-patch-audit.sh`          | ✅ pass |
| Helm CRD drift test (toolpolicy)      | ✅ `helm_drift::tests::helm_toolpolicy_crd_matches_rust_schema` passes |
| CLI `npm run typecheck`               | ✅ pass |
| CLI `npm run lint`                    | ✅ pass (26 pre-existing warnings, 0 errors) |

## Sign-offs

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
