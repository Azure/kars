# Security audit — Phase 2 S6 `ClawEval` reconciler

**Date:** 2026-04-27
**Slice:** S6 `phase2-claweval` (Phase 2 §8 entry 6)
**Branch:** `phase2-claweval`
**Sign-offs:** see §11.

---

## §0. Reuse map (no-duplication rule, §0.2 / §0.3)

S6 closes the §14.6 "five-CRD" set (column 12 — Governance as K8s
primitives) and is, by design, the **lightest** of the six Phase 2
reconcilers: it ships only a binding declaration over an external
service (Foundry Evals), the runtime path already exists in
`cli/src/commands/eval.ts`, and the threshold/regression actuator is
deliberately deferred to S7 where the Conditions matrix and
runtime-side SSA writer land cluster-wide.

| # | Existing seam | Reuse in S6 |
|---|---|---|
| 1 | `controller/src/status::conditions` | Conditions vocabulary + transition-time helpers used unchanged. |
| 2 | `controller/src/mcp_server::LocalObjectRef` | 6th semantic client (signing/jwks → profile → agent-card → guardrail-profile → memory-binding → eval-dataset). |
| 3 | `controller/src/claw_memory_reconciler` (S5) | Reconcile shape + finalizer pattern + non-fatal CRD-missing exit, copied verbatim. |
| 4 | `controller/src/claw_memory_compile` (S5) | Compile-module shape (pure-fn + version_hash + tests). |
| 5 | `controller/src/crd_validations::inject_spec_validations` | Same SSA-friendly CEL injector. |
| 6 | `controller/src/helm_drift::canonical_form` | Drift comparison reused verbatim. |
| 7 | `cli/src/commands/eval.ts` (Phase 1) | Existing Foundry Evals runtime: builds JSONL datasets, calls `/openai/evals` + `/evaluators` via the per-sandbox router. **Not modified in S6.** S7 wires the trigger that reads our binding ConfigMap. |
| 8 | `inference-router/src/routes/inference.rs` `/openai/evals` + `/evaluators` proxies (Phase 1) | The router holds the Workload Identity for Foundry calls; the controller has none. |
| 9 | RFC-3339 formatter `chrono::Utc::now().to_rfc3339_opts` | Copy-pasted across reconcilers (lift to shared module deferred to S7). |
| 10 | Phase 1 OAuth 2.1 verifier `Extension<VerifiedToken>` | Not consumed in S6 — eval runs are sandbox-internal, no cross-sandbox auth surface introduced. |
| 11 | `controller/src/status::ApplyOptions` (S5 SSA writer) | Reused unchanged for the binding ConfigMap apply. |
| 12 | sha256 → 16-byte hex `version_hash` (S2/S4/S5 pattern) | Identical implementation. Lift to shared util deferred to S7. |

**Single new struct:** none beyond `ClawEvalSpec`, `ClawEvalSuite`,
`ClawEvalDataset`, `ClawEvalThreshold`, `ClawEvalThresholdOp`,
`ClawEvalRegressionAction`, `SandboxRef`, `ClawEvalStatus`. No new
finalizer pattern, no new compile-module pattern, no new SSA
mechanism. `LocalObjectRef` semantically extended (6th client).

---

## §1. AGT boundary (verified 2026-04-27 against agt-toolkit 3.3.0)

§3 of the implementation plan demands that AzureClaw never duplicates
AGT scope.

**Result:** AGT has **no Eval module**, no Foundry-Evals binding type,
no scoring/threshold module. Eval orchestration is a pure Azure AI
Foundry concern. `ClawEval` is a K8s-native binding/provisioning
declaration over an external (Foundry) resource — outside AGT's
scope, exactly as the §3 non-compete table intends:

> `ClawEval` CRD is a **binding/provisioning resource over Foundry
> Evals** — it *configures* eval runs, it does not *run* the eval
> harness in-cluster. No in-cluster eval engine shipped.

S6 ships only the K8s primitive + compiled binding JSON. No AGT
integration, no parallel runtime path. The runtime path
(`cli/src/commands/eval.ts`) stays where it is; S7+ wires a
sandbox-side informer / cron actuator that reads
`claweval-{name}-binding` and triggers the existing `/openai/evals`
flow on schedule or manual trigger.

---

## §2. Threat model

### §2.1 Spoofing

| # | Vector | Mitigation |
|---|---|---|
| 1 | Crafted `sandboxRef.name` pointing to nonexistent sandbox | CEL only validates shape; runtime path no-ops on missing sandbox. No privilege escalation — the binding ConfigMap is consumed only by the sandbox pod with that name (label selector). |
| 2 | Spoofed evaluator name routing eval calls to attacker URL | Foundry-side: evaluators are scoped to the project's AI Services account; the CR carries names, not URLs. Runtime path resolves them through `/evaluators` proxy. |

### §2.2 Tampering

| # | Vector | Mitigation |
|---|---|---|
| 3 | Tamper with `claweval-{name}-binding` ConfigMap | Owned by `azureclaw-controller/claweval` field manager via Server-Side Apply; S7 lifts SSA enforcement cluster-wide. RBAC must restrict CM write to the controller SA. |
| 4 | Tamper with `threshold.score` to never fail | Operator-controlled by design — admission has no semantic notion of "appropriate threshold." Phase 3 may add an org-wide policy CRD that floors thresholds; out of S6 scope. |
| 5 | Inline dataset stuffed with adversarial prompts | Capped at 64 entries by CEL. Larger datasets must use a `ConfigMap` (RBAC-gated, auditable). The eval runtime is sandboxed; adversarial prompts cannot escape Foundry's safety net. |

### §2.3 Repudiation

| # | Vector | Mitigation |
|---|---|---|
| 6 | "I never bound that eval" | Reconciler emits structured log `ClawEvalReconciled` with name, namespace, suite, sandbox_ref, version_hash, generation. Phase 3 signed audit chain (§10.4 #8) makes this verifiable. |

### §2.4 Information disclosure

| # | Vector | Mitigation |
|---|---|---|
| 7 | Inline dataset leaks PII via `kubectl get claweval -o yaml` | Dataset is operator-supplied; the CR is opaque from the controller's perspective. RBAC `get/list claweval` should be gated on namespace. Operators are advised to use `dataset.configMapRef` for sensitive data; CEL caps inline at 64 entries. |
| 8 | Eval results land on the CR (`status.lastScore`) | By design — score is a numeric, not the dataset. The runtime-owned status fields are explicitly declared in the schema (`lastRunAt`, `lastScore`, `lastPass`) and patched by a distinct field manager `azureclaw-router/claweval` (S7). No raw outputs land on the CR. |

### §2.5 Denial of service

| # | Vector | Mitigation |
|---|---|---|
| 9 | Reconcile loop on Foundry 5xx | **Not exposed** — controller never calls Foundry. The runtime path (`/openai/evals`) runs in the sandbox/router and has its own retry policy. |
| 10 | Many `ClawEval` CRs scheduled simultaneously | Each runs in its own sandbox; concurrency is bounded by sandbox count. CRD admission only validates shape. Schedule fan-out is a Foundry-side rate-limit concern. |
| 11 | Cron expression with absurd density (e.g. every second) | CEL validates 5-or-6 token shape and length cap (256 chars). Full cron parsing happens at runtime; an invalid cron line fails the runtime cron parser, not admission. |

### §2.6 Elevation of privilege

| # | Vector | Mitigation |
|---|---|---|
| 12 | Operator cross-namespace data access via spoofed `sandboxRef.name` | `ClawEval` is namespaced; `sandboxRef.name` resolves within the same namespace. Cross-namespace requires distinct CR. |
| 13 | Controller gains Foundry access | **Not granted.** Controller has zero Foundry credentials. All Foundry traffic flows through the per-sandbox router using Workload Identity. |
| 14 | `regressionAction: Suspend` weaponised to mass-suspend sandboxes | Action takes effect only when the runtime-side actuator (S7) decides the eval failed; the controller never sets `ClawSandbox.spec.suspend`. The actuator must be RBAC-scoped to `patch claw-sandboxes` in the same namespace. |

---

## §3. Out of scope (deferred to S7+)

1. **Runtime trigger.** S6 publishes the binding ConfigMap; S7 ships
   the cron-actuator / on-demand trigger that reads it and invokes
   `/openai/evals`.

2. **Threshold pass/fail computation.** Score comparison against the
   `threshold.score` + `threshold.op` happens runtime-side; the
   controller does not interpret eval output.

3. **Regression actuator.** `regressionAction: Suspend` is materialised
   in the binding (always emitted with default `Suspend` even when
   omitted in spec), but no controller mutates `ClawSandbox` based on
   eval outcome. The runtime-side actuator (S7+) will own the
   `claweval` finalizer step that flips `spec.suspend`.

4. **Status `phase` matrix beyond `Ready` / `Degraded`.** Full S7
   matrix (`Pending`, `Reconciling`, `Suspended`) lands cluster-wide
   in S7; S6 emits the same minimal vocabulary as S2–S5.

5. **Runtime-owned status fields.** `lastRunAt`, `lastScore`,
   `lastPass` are declared in the CRD schema (so admission accepts
   them) and the controller's status patch sets them to `None`. SSA
   leaves them untouched once the runtime-side writer applies them
   under field manager `azureclaw-router/claweval`. The
   `field_manager_distinct_from_runtime_writer` unit test documents
   this contract.

6. **`promptfoo` and `inspect-ai` runtime adapters.** Suite values are
   accepted at admission for forward compatibility, but only
   `foundry-evals` has a runtime path today (mirrors S5's pattern of
   declaring the shape ahead of the consumer).

7. **Cross-namespace `sandboxRef`.** Out of scope by design — keeps
   admission boundary aligned with K8s namespace tenancy.

8. **AGT chain emission of eval outcomes.** Phase 3 (§10.4 #8 emit
   half) will include eval pass/fail in the signed reconcile audit
   chain.

---

## §4. Implementation surface

| File | Lines | Purpose |
|---|---|---|
| `controller/src/claw_eval.rs` | ~280 | CRD struct, sub-types, CustomResource derive, runtime-owned status field documentation. |
| `controller/src/claw_eval_compile.rs` | ~290 | Pure-function compile + version_hash + 9 unit tests. |
| `controller/src/claw_eval_reconciler.rs` | ~430 | Reconcile, finalizer, conditions, ConfigMap publish + 7 unit tests including `field_manager_distinct_from_runtime_writer`. |
| `controller/src/crd_validations.rs` | +~85 / +~75 tests | `claw_eval_validations()` (8 CEL rules) + `claw_eval_crd()` injector + 5 unit tests. |
| `controller/src/helm_drift.rs` | +~25 | `CLAWEVAL_HELM_CRD_PATH` const + dumper + drift test. |
| `controller/src/main.rs` | +6 | Module registration + reconciler spawn in `tokio::select!`. |
| `deploy/helm/azureclaw/templates/crd-claweval.yaml` | 271 | Generated via `DUMP_CLAWEVAL_CRD_YAML=1` dumper. |

**Test count delta:** controller 238 → 264 (+26).

**No file moved past its Phase 2 cap** (§4.2). All new modules are
fresh files; touched files (`crd_validations.rs`, `helm_drift.rs`,
`main.rs`) are well under their budgets.

---

## §5. CEL rules and rationale

| Rule | Rationale |
|---|---|
| `sandboxRef.name` non-empty (1-253 chars) | Mirrors K8s `metadata.name` length. Keeps admission decoupled from `ClawSandbox` lookups. |
| `evaluators` required and non-empty when `suite == "foundry-evals"` | Foundry Evals require at least one evaluator; other suites (forward-compat) accept empty. |
| Each `evaluators` entry 1-256 chars | Bounds attack surface for evaluator-name spoofing; mirrors §5 of S5. |
| `schedule`, when set, 5-or-6 cron-token shape (1-256 chars) | Admission rejects obviously malformed cron lines without doing full cron parse (defer to runtime). |
| `threshold.score` in `[0.0, 1.0]` when set | Foundry Evals primary score is normalised to `[0,1]`. |
| `dataset.configMapRef` and `dataset.inline` mutually exclusive | Avoids ambiguous "which dataset wins" semantics. |
| `dataset.inline` capped at 64 entries | Keeps the CR small; large datasets must use `ConfigMap` (RBAC-auditable). |
| `displayName`, when set, 1-256 chars | Matches S2/S4/S5 displayName cap. |

CEL coverage = 8 rules, 5 tests in `crd_validations.rs`, all green.

---

## §6. SSA + field manager

Field manager: `azureclaw-controller/claweval` — distinct from
mcp/toolpolicy/a2aagent/inferencepolicy/clawmemory. SSA via
`Patch::Apply` with `force()` for the binding ConfigMap and the
status subresource.

**Field-manager split** (forward contract for S7):

- **Controller-owned** (`azureclaw-controller/claweval`): `phase`,
  `observedGeneration`, `conditions` (Ready/Progressing/Degraded),
  `bindingConfigMapRef`, `versionHash`, `lastReconciledAt`.
- **Runtime-owned** (`azureclaw-router/claweval`, S7): `lastRunAt`,
  `lastScore`, `lastPass` plus an `EvalsPassed` condition appended
  via SSA.

The controller's status patch sets the three runtime-owned fields to
`None`. SSA arbitrates per-field ownership: once the runtime-side
writer applies a value, subsequent controller reconciles leave it
untouched. The unit test
`field_manager_distinct_from_runtime_writer` documents and asserts
this contract.

---

## §7. Failure modes and recovery

| Failure | Reconciler behaviour |
|---|---|
| K8s API transient (5xx, throttling) | `error_policy` requeues 60s; condition stays last-known. |
| ConfigMap apply fails | `Degraded=True` with reason `BindingWriteFailed`; requeue 60s. |
| CR deleted with finalizer present | Deletes the binding ConfigMap (404 tolerated as success), strips finalizer, exits. |
| CRD missing at startup | Reconciler logs warning and parks (matches S1–S5 pattern). |
| Foundry unavailable at runtime | **Not the controller's problem.** Runtime path (`/openai/evals`) handles its own retries. |

REQUEUE intervals: success 300s, failure 60s — matches S5.

---

## §8. Operator concerns and migration

- This is a new optional CRD. No `ClawSandbox` change required.
- A sandbox without any `ClawEval` continues to use the Phase 1
  CLI-driven eval flow (`azureclaw eval`).
- The S7 actuator that consumes `claweval-{name}-binding` is
  additive — until S7 lands, the binding ConfigMap is published but
  not yet read by the cron actuator. This is intentional: S6 ships
  the K8s primitive; S7 wires the consumer.

---

## §9. Verification matrix

| Gate | Result |
|---|---|
| `cargo build --workspace` | green |
| `cargo test --workspace` | green (controller: 264/264; router + integration unchanged) |
| `cargo clippy --workspace --all-targets -- -D warnings` | green |
| `cargo fmt --all -- --check` | green |
| `ci/no-stubs.sh` | green |
| `ci/no-custom-crypto.sh` | green |
| `ci/check-loc.sh` | green |
| Helm drift test (`helm_claweval_crd_matches_rust_schema`) | green (Rust ↔ helm parity verified) |

---

## §10. References

- `docs/implementation-plan.md` §3 (non-compete with AGT) —
  establishes ClawEval as binding-only.
- `docs/implementation-plan.md` §8 entry 6 (Phase 2 plan, §10.5 #6).
- `docs/competitive.md` §14.6 column 12 — S6 closes the
  Governance-as-K8s-primitives column with the fifth differentiator
  CRD.
- `cli/src/commands/eval.ts` (Phase 1) — the runtime path this
  slice declaratively configures.
- Azure AI Foundry Evals API: `/openai/evals`, `/evaluators` —
  proxied through `inference-router/src/routes/inference.rs`.

---

## §11. Sign-offs

- ☑ Author — `phase2-claweval` branch implementor.
  AGT boundary verified against AGT 3.3.0 — confirmed AGT carries no
  eval module. K8s primitive + compiled binding JSON only; runtime
  enforcement stays on the Phase-1 `cli/src/commands/eval.ts` path.
  Controller never calls Foundry — credentialing boundary preserved.
  Field-manager split documented and asserted via unit test for S7
  forward compatibility.

- ☑ Reviewer — implementation matches §0 reuse map; STRIDE residual
  risks are documented; out-of-scope set is explicit and
  cross-referenced to S7. CEL rule count (8) covers
  sandboxRef/evaluators/suite-coupling/cron-shape/threshold-bounds/
  dataset-mutex/inline-cap/displayName. Runtime-owned status fields
  are declared with explicit `None` writes from the controller so SSA
  preserves runtime updates — the
  `field_manager_distinct_from_runtime_writer` test makes this
  contract executable. `regressionAction` always materialises with
  default `Suspend` so the runtime actuator has a deterministic
  default to read.
