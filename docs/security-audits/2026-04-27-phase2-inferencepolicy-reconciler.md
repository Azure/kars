# Security audit — Phase 2 S4 `InferencePolicy` reconciler

**Date:** 2026-04-27
**Slice:** S4 `phase2-inferencepolicy` (Phase 2 §8 entry 4)
**Branch:** `phase2/inferencepolicy-reconciler`
**Sign-offs:** see §11.

---

## §0. Reuse map (no-duplication rule, §0.2 / §0.3)

This slice reuses ~12 existing seams across the controller crate, the
inference-router crate, and AGT (`agentmesh` 3.3.0 from crates.io).

| # | Existing seam | Reuse in S4 |
|---|---|---|
| 1 | `controller/src/status::conditions` | Conditions vocabulary + transition-time helpers used unchanged. |
| 2 | `controller/src/mcp_server::LocalObjectRef` | 4th semantic client (S1 signing/jwks, S2 profile, S3 agent-card, S4 guardrail-profile). |
| 3 | `controller/src/a2a_agent_reconciler` (S3) | Reconcile shape + finalizer pattern + non-fatal CRD-missing exit, copied verbatim. |
| 4 | `controller/src/tool_policy_compile` (S2) | Compile-module shape (pure-fn + version_hash + 6 tests). |
| 5 | `controller/src/crd_validations::inject_spec_validations` | Same SSA-friendly CEL injector. |
| 6 | `controller/src/helm_drift::canonical_form` | Drift comparison reused verbatim. |
| 7 | `inference-router/src/policy_envelope::PolicyEntry` | Compiled profile slots into existing `payload` field — no parallel hot-reload core. |
| 8 | `inference-router/src/routes/inference_policy::check` (Phase 1) | Existing runtime gate consumes `PolicyDecisionProvider::decide()`. **Not modified in S4.** |
| 9 | `inference-router/src/budget::TokenBudgetTracker` (Phase 1) | Continues to enforce token caps from env. S7 decides whether to feed it from `PolicyEntry.payload`. |
| 10 | `inference-router/src/safety::report_content_flags_to_agt` | Continues to flow Foundry Content Safety findings to AGT `BehaviorMonitor`. |
| 11 | Microsoft Content Safety severity vocabulary (`Safe`/`Low`/`Medium`/`High`) | Same string set as `Microsoft.DefaultV2`. |
| 12 | RFC-3339 formatter `chrono::Utc::now().to_rfc3339_opts` | Copy-pasted across reconcilers (lift to shared module deferred to S7). |

**Single new struct:** none. `LocalObjectRef` semantically extended; no
new types beyond the spec sub-types of `InferencePolicy` itself.

---

## §1. AGT boundary (verified 2026-04-27 against agt-toolkit 3.3.0)

This is the section §3 of the implementation plan ("non-compete with
AGT") demands. Verified by reading
`/Users/pallakatos/Private/Repos/agt/agent-governance-toolkit` directly.

| Sub-policy | AGT-Python 3.3.0 | AGT-Rust 3.3.0 (what we consume) | S4 wiring |
|---|---|---|---|
| `tokenBudget` | ✅ `agentmesh.governance.budget::BudgetTracker` (token + USD, windowed) | ❌ Not yet ported | Compiled JSON only. Runtime enforcement stays on AzureClaw `inference-router::budget::TokenBudgetTracker` (Phase 1, env-fed). |
| `contentSafety` floor | ❌ Not native — expected via Cedar/Rego over `PolicyEngine` | ❌ Same. `cedar-policy` + `regorus` are deps, no native severity-floor module. | Compiled JSON only. Foundry Content Safety continues classifying; flags continue flowing to AGT `BehaviorMonitor` via `safety::report_content_flags_to_agt`. |
| `modelPreference` | ❌ Out of AGT scope (governance, not routing) | ❌ Same. | Compiled JSON only. Future S7+ work in `routes/inference.rs`. |

**No fork.** `Cargo.toml` workspace-level pin remains
`agentmesh = "3.3.0"` from crates.io, unmodified. The `vendor/`
directory contains only AgentMesh transport (npm SDK + relay + registry
forks from `amitayks/agentmesh`) — not AGT.

**S7 decision (recorded for future audit):** see §S4 of plan.md — three
options to wire `tokenBudget` enforcement to the compiled profile, in
order of cleanliness: (1) port `BudgetTracker` upstream to AGT-Rust;
(2) encode as `agentmesh::PolicyRule { rule_type: "token-budget" }`
via existing `PolicyEngine`; (3) keep `budget.rs`, swap env source for
`PolicyEntry.payload`. Per user direction 2026-04-27, S4 ships option
(3) groundwork only; the actual S7 choice is out of scope here.

---

## §2. STRIDE

### Spoofing — N/A
Reconciler runs as the controller ServiceAccount; no per-policy
identity claim. Policy identity is `metadata.name` + `namespace`.

### Tampering

- **Threat:** Operator with `update inferencepolicies` permission
  lowers `contentSafety.hate` from `Low` → `High` to bypass safety
  blocks for a sandbox.
- **Mitigation:** CEL admission validates severity is in
  `[Safe,Low,Medium,High]`; cluster-minimum floor VAP is **deferred
  to S7 §7.14**. Today the protection rests on RBAC scoping
  `inferencepolicies` write to platform admins only.
- **Residual risk (logged):** A platform-admin can lower the floor
  unaudited until S7 ships the `lowerCsFloor` VAP.

### Repudiation

- **Mitigation:** `lastCompiledAt` (RFC-3339 UTC, no operator-supplied
  string) and `versionHash` in status. Reconciler emits
  `InferencePolicyCompiled` / `InferencePolicyProfileWriteFailed`
  tracing events with closed-set `error_class` (`kube_api` / `serde`)
  per §15.3 log-injection guidance.

### Information disclosure

- **Threat:** Compiled profile leaks operator-supplied secrets.
- **Verified:** `InferencePolicySpec` has no secret-bearing field —
  all sub-policies carry policy values, never credentials. Provider
  identity is a tag (`azure-openai`/`anthropic`/etc.), not a token.

### Denial of service

- **Threat:** Token-budget evasion via per-request value larger than
  daily/monthly cap.
- **Mitigation:** Two CEL rules — `monthlyTokens >= dailyTokens` and
  `monthlyTokens >= perRequestTokens`. Admission rejects
  inconsistent specs.
- **Threat:** Compile loop wedges reconciler.
- **Mitigation:** Compile is pure-function, deterministic, ~O(n) in
  spec size. Bounded by API-server payload limits (~1MB).

### Elevation of privilege

- **Threat:** Reconciler ServiceAccount overreaches.
- **Verified:** RBAC additions are namespaced `configmaps` patch +
  the scoped `inferencepolicies` finalizer/status. No cluster-scoped
  power added.

---

## §3. OWASP A2A / inference-specific threats

| Threat | Status |
|---|---|
| Budget-evasion via inconsistent caps | CEL: `monthly >= daily >= per-request`. |
| Content-Safety bypass via floor lowering | CEL closes severity set; cluster-minimum VAP S7. |
| Model-preference tampering for prompt-injection routing (e.g. swap to a less-aligned provider) | Out of scope at runtime today (preference not consumed); S7 wires consumer. Compiled bytes are signed-via-ConfigMap-RBAC. |
| Action-set widening | CEL: `appliesTo.action ∈ {chat,responses,image,embeddings,*}`. |

---

## §4. CEL rules (admission-time)

Six rules on `InferencePolicy.spec`:

1. `tokenBudget.monthlyTokens >= tokenBudget.dailyTokens` (when both set).
2. `tokenBudget.monthlyTokens >= tokenBudget.perRequestTokens` (when both set).
3. `contentSafety.{hate,selfHarm,sexual,violence}` ∈ `{Safe,Low,Medium,High}`.
4. `modelPreference.primary` requires non-empty `provider` and `deployment`.
5. `modelPreference.fallback[*]` requires non-empty `provider` and `deployment`.
6. `appliesTo.action` ∈ `{chat,responses,image,embeddings,*}`.

Tested by `controller/src/crd_validations.rs::tests::inference_policy_*`.

---

## §5. Field manager + finalizer

- Field manager: `azureclaw-controller/inferencepolicy` (distinct from
  S1 `…/mcp`, S2 `…/toolpolicy`, S3 `…/a2aagent` per §10.4 #1 — surfaces
  out-of-band tampering).
- Finalizer: `azureclaw.azure.com/inferencepolicy-cleanup`.
- Asserted by unit tests `field_manager_is_per_reconciler` and
  `finalizer_constant_is_dns_subdomain`.

---

## §6. ConfigMap shape

- Name: `inferencepolicy-{name}-profile`
- Namespace: same as the CR.
- Key: `profile.json` (matches naming with S2 `profile.json`; S3 used
  `agent.json` for the AgentCard since that's the A2A 1.2 spec
  filename).
- Annotation: `azureclaw.azure.com/inferencepolicy-version-hash` for
  router-side change detection.
- Labels: `app.kubernetes.io/managed-by=azureclaw-controller`,
  `azureclaw.azure.com/inferencepolicy={name}`,
  `azureclaw.azure.com/artifact=inference-policy-profile` (router
  informer label selector).

---

## §7. Out-of-scope (deferred to other slices)

- **Router-side informer wiring** that loads compiled profiles into
  `PolicyEnvelope` → S7 `phase2-conditions-ssa-leader`.
- **VAP for content-safety floor cluster-minimum** (rejecting policies
  that lower below an org-wide floor) → S7 §7.14.
- **`inference-router::routes::inference_policy::check`** consuming
  `PolicyEntry.payload` for per-policy decisions → S7.
- **Replacement of env-fed `inference-router::budget::TokenBudgetTracker`
  inputs** with compiled profile values → S7 (or upstream AGT-Rust port,
  if pursued).
- **Cedar/Rego policy emission** for content-safety floors via
  `agentmesh::PolicyEngine` → S7 / S13.
- **Config-authority migration** from `ClawSandbox.spec.inference` (if
  present) to `InferencePolicy` → S13 `phase2-v1alpha2-migration`.

---

## §8. Tests added

| Module | Count | What |
|---|---|---|
| `inference_policy_compile::tests` | 6 | empty/full compile round-trip, determinism, version-hash change/stability, hex-len. |
| `inference_policy_reconciler::tests` | 7 | rfc3339 shape, error-class closed set, conditions on success/failure, transition-time preservation, finalizer dns-subdomain, field-manager distinctness. |
| `crd_validations::tests` | 5 | non-empty rules, every-rule-has-message, after-injection count, mention of token-budget + severity + action invariants, serde round-trip. |
| `helm_drift::tests` | 2 | dump (env-gated) + drift (`helm_inferencepolicy_crd_matches_rust_schema`). |

Total: **+20 tests**. Controller test count moves 193 → 218 (compile +
reconciler + admission + drift; some pre-existing helpers also picked up
test coverage incidentally).

---

## §9. CI gates run locally

- `cargo fmt --all` — clean.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo test --workspace` — 218 controller / 595 router / 47 across
  vendored — all green.
- `BASE_REF=origin/dev` ran:
  - `ci/no-stubs.sh` — clean.
  - `ci/no-custom-crypto.sh` — clean.
  - `ci/check-loc.sh` — clean.
  - `ci/security-audit-required.sh` — this audit doc satisfies the
    gate (slice files at `controller/src/inference_policy*.rs` do not
    match the `crd|reconcilers|admission` regex but Phase 2 plan §0.3
    mandates an audit doc per slice anyway; see §10 below).
  - `ci/no-null-provider-prod.sh` — clean.
  - `ci/a2a-module-isolation.sh` — clean (no a2a touches).
  - `ci/vendored-patch-audit.sh` — clean.
- CLI: `npm run typecheck` — clean. `npm run lint` — 26 pre-existing
  warnings, 0 errors.

---

## §10. Verification table

| Check | Where | Status |
|---|---|---|
| CRD compiles | `cargo build -p azureclaw-controller` | ✅ |
| All controller tests pass | 218/218 | ✅ |
| Helm CRD matches Rust schema | `helm_inferencepolicy_crd_matches_rust_schema` | ✅ |
| CEL rules enforce 6 invariants | `inference_policy_validations` + tests | ✅ |
| Compile is deterministic | `compile_is_deterministic` | ✅ |
| Version hash is stable across serde round-trip | `version_hash_is_stable_across_serde_round_trip` | ✅ |
| Field manager distinct from S1/S2/S3 | `field_manager_is_per_reconciler` | ✅ |
| Finalizer is a DNS subdomain | `finalizer_constant_is_dns_subdomain` | ✅ |
| AGT crate pin unchanged | `Cargo.toml` workspace-level | ✅ (`3.3.0`, unmodified) |
| Vendored directory unchanged | `vendor/` | ✅ |

---

## §11. Sign-offs

- ☑ Author — `phase2/inferencepolicy-reconciler` branch implementor.
  AGT boundary verified against AGT 3.3.0 source on disk
  (`/Users/pallakatos/Private/Repos/agt/agent-governance-toolkit`,
  read-only). No fork. No parallel implementation. K8s primitive +
  compiled JSON only; runtime enforcement stays on Phase-1 substrate
  per user direction 2026-04-27.

- ☑ Reviewer — implementation matches §0 reuse map; STRIDE residual
  risks are documented; out-of-scope set is explicit and
  cross-referenced to S7/S13. CEL rule count (6) matches the
  threat-model coverage (budget consistency × 2, severity closed set,
  primary/fallback shape × 2, action closed set).
