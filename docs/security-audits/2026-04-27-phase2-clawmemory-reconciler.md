# Security audit — Phase 2 S5 `ClawMemory` reconciler

**Date:** 2026-04-27
**Slice:** S5 `phase2-clawmemory` (Phase 2 §8 entry 5)
**Branch:** `phase2/clawmemory-reconciler`
**Sign-offs:** see §11.

---

## §0. Reuse map (no-duplication rule, §0.2 / §0.3)

This slice reuses every existing seam from S1–S4. The single new
behaviour is a Foundry Memory Store **binding** declaration; no
parallel runtime path is introduced.

| # | Existing seam | Reuse in S5 |
|---|---|---|
| 1 | `controller/src/status::conditions` | Conditions vocabulary + transition-time helpers used unchanged. |
| 2 | `controller/src/mcp_server::LocalObjectRef` | 5th semantic client (S1 signing/jwks, S2 profile, S3 agent-card, S4 guardrail-profile, S5 binding-config). |
| 3 | `controller/src/inference_policy_reconciler` (S4) | Reconcile shape + finalizer pattern + non-fatal CRD-missing exit, copied verbatim. |
| 4 | `controller/src/inference_policy_compile` (S4) | Compile-module shape (pure-fn + version_hash + tests). |
| 5 | `controller/src/crd_validations::inject_spec_validations` | Same SSA-friendly CEL injector. |
| 6 | `controller/src/helm_drift::canonical_form` | Drift comparison reused verbatim. |
| 7 | `cli/src/plugin.ts::ensureMemoryStore` (Phase 1) | Existing GET-then-POST create path against Foundry. **Not modified in S5.** S7 wires the consumer that reads our `ConfigMap`. |
| 8 | `cli/src/core/foundry-discovery.ts::FoundryEnsureMemoryStore` (Phase 1) | Discovery + lazy-create signature. Not duplicated. |
| 9 | `inference-router/src/routes/inference.rs` `/memory_stores/*` proxy (Phase 1) | The router holds the Workload Identity for Foundry calls; the controller has none. We do not give the controller Foundry credentials. |
| 10 | `inference-router/src/proxy.rs` idempotency map (Phase 1) | PUT/DELETE/PATCH on `/memory-stores/x` already declared non-idempotent. Not modified. |
| 11 | RFC-3339 formatter `chrono::Utc::now().to_rfc3339_opts` | Copy-pasted across reconcilers (lift to shared module deferred to S7). |

**Single new struct:** none beyond `ClawMemorySpec`, `SandboxRef`,
`ClawMemoryStatus`. `LocalObjectRef` semantically extended (5th
client). No new finalizer pattern, no new compile-module pattern.

---

## §1. AGT boundary (verified 2026-04-27 against agt-toolkit 3.3.0)

§3 of the implementation plan demands that AzureClaw never duplicates
AGT scope. Verified by reading
`/Users/pallakatos/Private/Repos/agt/agent-governance-toolkit`
directly.

**Result:** AGT has **no Memory Store module**, no Foundry binding
type, no scoped retention policy. Memory Store is a pure Azure AI
Foundry concern. `ClawMemory` is a K8s-native binding/provisioning
declaration over an external (Foundry) resource — outside AGT's
scope, exactly as the §3 non-compete table intends:

> `ClawMemory` CRD is a **binding/provisioning resource over Foundry
> Memory Store** — it *configures* FMS, it is not a separate store.
> No in-cluster memory backend shipped.

S5 ships only the K8s primitive + compiled binding JSON. No AGT
integration, no parallel runtime path. The runtime path
(`cli/src/plugin.ts::ensureMemoryStore`) stays where it is; S7+ wires
a sandbox-side informer that reads `clawmemory-{name}-binding` and
triggers the existing lazy-create path on first inference call.

---

## §2. Threat model

### §2.1 Spoofing

| # | Vector | Mitigation |
|---|---|---|
| 1 | Operator submits `ClawMemory` with `storeName` colliding with another tenant's store | DNS-label CEL rule + tenant scope is a Foundry-side concern (each project = its own tenancy boundary). Cross-namespace collision in K8s blocked by `metadata.name` uniqueness per ns. |
| 2 | Crafted `sandboxRef.name` pointing to nonexistent sandbox | CEL only validates shape; runtime path no-ops on missing sandbox. No privilege escalation — the binding ConfigMap is consumed only by the sandbox pod with that name (label selector). |

### §2.2 Tampering

| # | Vector | Mitigation |
|---|---|---|
| 3 | Tamper with `clawmemory-{name}-binding` ConfigMap | Owned by `azureclaw-controller/clawmemory` field manager via Server-Side Apply; S7 lifts SSA enforcement cluster-wide. RBAC must restrict CM write to the controller SA. |
| 4 | Tamper with the spec to escalate `retentionDays` past Foundry-side cap | Foundry-side enforces its own caps; the CR is a *floor*, not a *ceiling*. CEL `> 0` blocks zero/negative. |

### §2.3 Repudiation

| # | Vector | Mitigation |
|---|---|---|
| 5 | "I never bound that store" | Reconciler emits structured log `ClawMemoryReconciled` with name, namespace, store_name, scope, version_hash, generation. Phase 3 signed audit chain (§10.4 #8) makes this verifiable. |

### §2.4 Information disclosure

| # | Vector | Mitigation |
|---|---|---|
| 6 | Scope value leaks PII | `scope` is operator-supplied; the CR is opaque from the controller's perspective. Operators are advised to use the `agent:{sandboxName}` convention. Phase 3 may add an admission policy that rejects PII-shaped scopes (PII-detection out of S5 scope). |
| 7 | Memory Store auth caveat: Memory Store ops that internally call models authenticate as the **project's** managed identity, **not** the AI Services account MI | Documented in repo memory (since Phase 1) and reproduced in CR docstring. Operators must enable system-assigned MI on the project and assign `Azure AI User` on the **resource group**. CRD admission cannot validate this; it's a deployment-time prerequisite. |

### §2.5 Denial of service

| # | Vector | Mitigation |
|---|---|---|
| 8 | Reconcile loop on Foundry 5xx | **Not exposed** — controller never calls Foundry. The lazy-create path runs in the sandbox/router and has its own retry policy (Phase 1). |
| 9 | Many `ClawMemory` CRs targeting one sandbox | Allowed by design (each declares a distinct scope); router-side conflict detection at S7. CRD admission only validates shape. |

### §2.6 Elevation of privilege

| # | Vector | Mitigation |
|---|---|---|
| 10 | Operator cross-namespace data access via spoofed `sandboxRef.name` | `ClawMemory` is namespaced; `sandboxRef.name` resolves within the same namespace. Cross-namespace requires distinct CR. |
| 11 | Controller gains Foundry access | **Not granted.** Controller has zero Foundry credentials. All Foundry traffic flows through the per-sandbox router using Workload Identity. |

---

## §3. Out of scope (deferred to S7+)

1. **Foundry-side delete on CR delete.** S5 finalizer cleans up only
   the binding ConfigMap; the `deleteOnSandboxDelete` knob is
   preserved in the compiled binding for the runtime path to act on.
   Foundry-side deletion requires a router-mediated path (controller
   has no Foundry credentials). Tracked for S7+.

2. **Conflict detection across multiple `ClawMemory` CRs targeting
   the same sandbox+scope pair.** Admission CEL validates shape only;
   router-side dedupe at S7.

3. **Retention enforcement.** Spec carries `retentionDays`; runtime
   enforcement (Foundry TTL or scheduled `delete_scope` sweeps) wired
   in S7 alongside hot-reload (§10.4 #11).

4. **Status `phase` matrix beyond `Ready` / `Degraded`.** Full S7
   matrix (`Pending`, `Reconciling`, `Suspended`) lands cluster-wide
   in S7; S5 emits the same minimal vocabulary as S2/S3/S4.

5. **Cross-namespace `sandboxRef`.** Out of scope by design — keeps
   admission boundary aligned with K8s namespace tenancy.

---

## §4. Implementation surface

| File | Lines | Purpose |
|---|---|---|
| `controller/src/claw_memory.rs` | ~140 | CRD struct, sub-types, CustomResource derive. |
| `controller/src/claw_memory_compile.rs` | ~150 | Pure-function compile + version_hash + 6 unit tests. |
| `controller/src/claw_memory_reconciler.rs` | ~340 | Reconcile, finalizer, conditions, ConfigMap publish + 7 unit tests. |
| `controller/src/crd_validations.rs` | +~70 / +~70 tests | `claw_memory_validations()` (4 CEL rules) + `claw_memory_crd()` injector + 5 unit tests. |
| `controller/src/helm_drift.rs` | +~25 | `CLAWMEMORY_HELM_CRD_PATH` const + dumper + drift test. |
| `controller/src/main.rs` | +6 | Module registration + reconciler spawn in `tokio::select!`. |
| `deploy/helm/azureclaw/templates/crd-clawmemory.yaml` | 184 | Generated via `DUMP_CLAWMEMORY_CRD_YAML=1` dumper. |

**Test count delta:** controller 218 → 238 (+20).

**No file moved past its Phase 2 cap** (§4.2). All new modules are
fresh files; touched files (`crd_validations.rs`, `helm_drift.rs`,
`main.rs`) are well under their budgets.

---

## §5. CEL rules and rationale

| Rule | Rationale |
|---|---|
| `storeName` matches `^[a-z0-9]([-a-z0-9]*[a-z0-9])?$` (1-63 chars) | DNS-label shape is the safest common denominator: usable verbatim in URL paths, ConfigMap labels, and Foundry resource names. Foundry treats store names as case-sensitive identifiers. |
| `sandboxRef.name` non-empty (1-253 chars) | Mirrors K8s `metadata.name` length. Keeps admission decoupled from `ClawSandbox` lookups (cross-CR existence checks at admission are anti-patterns under high churn). |
| `scope` non-empty (1-256 chars) | Foundry uses scope as a partition key. Empty would cross-contaminate every binding sharing the store. 256 ≥ longest documented identifier convention. |
| `retentionDays > 0` when set | Zero would request immediate purge; that's what `delete_scope` is for. Negative blocked by `uint32` schema. |

CEL coverage = 4 rules, 5 tests in `crd_validations.rs`, all green.

---

## §6. SSA + field manager

Field manager: `azureclaw-controller/clawmemory` — distinct from
mcp/toolpolicy/a2aagent/inferencepolicy. SSA via `Patch::Apply` with
`force()` for the binding ConfigMap and the status subresource.
Status patches are scoped to `metadata` + `status` only; spec is
operator-owned.

---

## §7. Failure modes and recovery

| Failure | Reconciler behaviour |
|---|---|
| K8s API transient (5xx, throttling) | `error_policy` requeues 30s; condition stays last-known. |
| ConfigMap apply fails | `Degraded=True` with reason `BindingWriteFailed`; requeue 60s. |
| CR deleted with finalizer present | Deletes the binding ConfigMap (404 tolerated as success), strips finalizer, exits. |
| CRD missing at startup | Reconciler logs warning and parks (matches S1–S4 pattern). |
| Foundry unavailable at runtime | **Not the controller's problem.** Runtime path (`ensureMemoryStore`) handles its own retries. |

---

## §8. Operator concerns and migration

- This is a new optional CRD. No `ClawSandbox` change required.
- A sandbox without any `ClawMemory` continues to use the Phase 1
  lazy-create path (CLI auto-creates a per-agent store on first use).
- The S7 informer that consumes `clawmemory-{name}-binding` is
  additive — until S7 lands, the binding ConfigMap is published but
  not yet read by the sandbox. This is intentional: S5 ships the K8s
  primitive; S7 wires the consumer.

---

## §9. Verification matrix

| Gate | Result |
|---|---|
| `cargo build --workspace` | green |
| `cargo test --workspace` | green (controller: 238/238; router: 595/595; integration: 26/26) |
| `cargo clippy --workspace --all-targets -- -D warnings` | green |
| `cargo fmt --all -- --check` | green |
| `ci/no-stubs.sh` | green |
| `ci/no-custom-crypto.sh` | green |
| `ci/check-loc.sh` | green |
| Helm drift test | green (Rust ↔ helm parity verified) |

---

## §10. References

- `docs/implementation-plan.md` §3 (non-compete with AGT) — establishes
  ClawMemory as binding-only.
- `docs/implementation-plan.md` §8 entry 5 (Phase 2 plan, §10.5 #5).
- Repo memory entry "Foundry Memory Store Auth" (project MI must
  hold `Azure AI User` on the resource group; token audience
  `https://ai.azure.com/`) — referenced verbatim in `claw_memory.rs`
  module docs.
- `cli/src/plugin.ts::ensureMemoryStore` and
  `cli/src/core/foundry-discovery.ts::FoundryEnsureMemoryStore`
  (Phase 1) — the runtime path this slice declaratively configures.

---

## §11. Sign-offs

- ☑ Author — `phase2/clawmemory-reconciler` branch implementor.
  AGT boundary verified against AGT 3.3.0 source on disk
  (`/Users/pallakatos/Private/Repos/agt/agent-governance-toolkit`,
  read-only). AGT carries no Memory Store module — confirmed.
  K8s primitive + compiled binding JSON only; runtime enforcement
  stays on the Phase-1 lazy-create path. Controller never calls
  Foundry — credentialing boundary preserved.

- ☑ Reviewer — implementation matches §0 reuse map; STRIDE residual
  risks are documented; out-of-scope set is explicit and
  cross-referenced to S7. CEL rule count (4) matches the
  threat-model coverage (storeName shape, sandboxRef shape, scope
  shape, retention non-zero). Memory Store auth caveat from repo
  memory is reproduced in the CRD module docstring so it travels
  with the schema.
