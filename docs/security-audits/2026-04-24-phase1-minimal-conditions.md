# Security audit — Phase 1 · ClawSandbox Conditions + observedGeneration

Audit ID: `2026-04-24-phase1-minimal-conditions`
Scope reference: internal Phase 1 plan §7 item 7
(minimum §9 controller craftsmanship for migration UX) and §4.1 target
module layout (`controller/src/status/`).

## What landed

1. **`controller/src/status/` module tree** (new).
   * `mod.rs` — `build_running_status_patch(sandbox, sandbox_ns)` pure
     builder for the Running-phase status payload. Four unit tests
     asserting observedGeneration propagation, Ready-condition wire
     shape, foundryAgentId preservation, and timestamp preservation
     across same-status reconciles.
   * `conditions.rs` — wrapper over
     `k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition`:
     `new_condition`, `preserve_transition_time`, `set` (upsert by
     `type`), `find`, plus pinned vocabulary (`TYPE_READY`,
     `TYPE_PROGRESSING`, `TYPE_DEGRADED`, `reason::*`). 10 unit
     tests. Uses upstream `k8s_openapi::jiff::Timestamp` (the `Time`
     wrapper in k8s-openapi 0.27 switched from chrono to jiff).
2. **`controller/src/crd.rs`** — `ClawSandboxStatus` gains two fields:
   * `observed_generation: Option<i64>` (camelCase in JSON,
     `skip_serializing_if = "Option::is_none"` so absence never wipes a
     real value).
   * `conditions: Vec<Condition>` (camelCase in JSON,
     `skip_serializing_if = "Vec::is_empty"`). **+1 unit test** pins
     these serde omission guarantees — a regression here would cause
     the controller to blow away a populated status on every reconcile.
3. **`controller/src/reconciler.rs`** — Step 5 status-patch block
   delegates to the new builder. **Shrank 2383 → 2338** by also
   extracting the offload-slot-release logic into a
   `pairing::release_offload_slot` helper (§4.3 "touched code pays its
   decomposition debt").
4. **`controller/src/pairing.rs`** — gains
   `release_offload_slot(client, requester, sandbox_name)`. Lookup +
   decrement + clear activeSandbox. No-op on errors (deletion path
   must not block finalizer).
5. **`controller/src/main.rs`** — `mod status;`.

## STRIDE

| Category | Applies | Note |
|---|---|---|
| **Spoofing** | N/A | No identity surface. Status is a controller-side write; `patch_status` with a standard PatchParams. |
| **Tampering** | **Positive** | Adding `observedGeneration` lets consumers detect stale observations; adding conditions lets `kubectl wait --for=condition=Ready` behave correctly. These are anti-tampering signals, not new tampering surfaces. `Condition.lastTransitionTime` is controller-stamped using monotonic `Timestamp::now()` and preserved across same-status reconciles (unit tested). |
| **Repudiation** | **Positive** | Conditions + observedGeneration are durable, consumer-queryable state machine markers — they improve auditability of reconcile behaviour. |
| **Information Disclosure** | N/A | Fields added are controller-owned metadata (generation numbers, condition reasons, messages). No PII, no secret material. `message` strings are controller-authored constants; no user-provided data enters them on this PR. |
| **Denial of Service** | Low | `build_running_status_patch` runs in O(`n_conditions`) which is bounded (at most one per type — upsert semantics enforced by `conditions::set`). No recursion, no unbounded growth. |
| **Elevation of Privilege** | N/A | No RBAC changes. Controller already has `patch_status` on ClawSandbox. `release_offload_slot` uses pre-existing pairing-namespace access path. |

## Principle mapping

* §0.2 #1 — zero regressions: reconciler status payload is a superset
  of the prior payload. The Running phase still carries `phase`,
  `namespace`, `sandboxPod`, `inferenceEndpoint`, `pendingApprovals`,
  and `foundryAgentId` (preserved identically). Existing consumers
  reading those fields are unaffected.
* §0.2 #4 — LOC: `reconciler.rs` 2383 → 2338 (under phase0_cap 2350
  and below baseline per `touched_must_shrink: true`). New files under
  800 cap (status/mod.rs 207 LOC with tests, status/conditions.rs 297
  LOC with tests). crd.rs 412 → 440 (not budgeted, well under 800).
  pairing.rs 199 → 247 (not budgeted).
* §0.2 #5 — Rust for controller, preserved.
* §0.2 #8 — solid, not look-alike:
  * No custom time library: uses `k8s_openapi::jiff::Timestamp::now()`
    exactly as k8s-openapi 0.27's `Time` wrapper expects.
  * No custom Condition type: re-uses upstream
    `k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition`.
  * No stubs/TODOs in the new code.
  * `skip_serializing_if` on the new fields is **required** — omitting
    it would cause the controller to clobber populated conditions with
    an empty array on every reconcile. A `serde_json::to_value` test
    pins this behaviour so the guarantee can't regress silently.
  * `preserve_transition_time` is directly tested: (a) reuses
    timestamp on same-status, (b) stamps new timestamp on status
    transition, (c) stamps when prior is None, (d) stamps when prior
    type differs. Each path has its own unit test.
* §0.2 #9 — this audit doc.
* §0.2 #10 — references pinned:
  * KEP-1623 (standardize conditions):
    `github.com/kubernetes/enhancements/tree/master/keps/sig-api-machinery/1623-standardize-conditions`
    cited in module doc.
  * k8s-openapi 0.27 `Time(pub crate::jiff::Timestamp)` API: verified
    against local registry checkout of the installed crate version
    before landing (`k8s-openapi-0.27.1/src/v1_35/apimachinery/pkg/apis/meta/v1/time.rs`).

## What was **not** done (deliberate)

* No `Progressing` condition emission yet. We stamp `Ready=True` on
  completed reconciles; step-wise progress events are a Phase 2 item
  (plan §8 P0 "full Conditions matrix"). Landing a "Progressing" that
  always immediately flips to True would be the exact "pseudo-control"
  §0.2 #8 forbids.
* No `Degraded` condition on error paths. The error_policy function
  currently swallows errors with tracing logs; wiring condition
  emission into error paths is a follow-up that also needs the reason
  vocabulary (`reason::FAILED`, `reason::SPEC_INVALID`,
  `reason::DEPENDENCY_MISSING`, `reason::TIMED_OUT` — already pinned
  here).
* No conversion webhook for `v1alpha1` → `v1alpha2`. The new fields
  are backwards-compatible additions; existing `v1alpha1` clients are
  unaffected. A full v1alpha2 with field moves is plan §8 item 10.
* No ValidatingAdmissionPolicy on status transitions. Status is
  controller-owned; admission is for spec-validation.

## Re-audit triggers

* k8s-openapi bump past 0.27 — re-verify `Time`/`Timestamp` API.
* Phase 2 "full Conditions matrix" — revisit every condition type and
  reason to confirm alignment with downstream dashboards.
* CRD moves to `v1alpha2` with inline-spec migration — status
  subresource may need a conversion-webhook counterpart.
* Any future code path that writes a `message` sourced from
  user-provided data → re-audit Information-Disclosure row (today
  messages are controller-authored constants only).

## Verification

* `cargo test --all`: 330 passed (was 314; +16 new tests across
  status/conditions.rs, status/mod.rs, and crd.rs).
* `cargo clippy --all-targets --all-features -- -D warnings`: clean.
* Six CI gates PASS.
* `reconciler.rs` 2338 ≤ phase0_cap 2350, and < baseline 2383.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
