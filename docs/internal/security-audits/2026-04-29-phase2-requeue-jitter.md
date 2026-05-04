# Phase 2 — S7.D: bounded jitter on requeue durations

**Date:** 2026-04-29
**Slice:** `phase2-requeue-jitter` (sub-slice S7.D of S7 craftsmanship train)
**Author:** AzureClaw maintainers
**Sign-offs:** `@maintainer-1`, `@maintainer-2`

## Scope

Add ±20% multiplicative jitter to every `Action::requeue` duration the
controller emits from an `error_policy` (or analogous error-requeue
helper). Without jitter, every CR seen during a single watcher resync
schedules its retry for *exactly* the same wall-clock instant, creating
a thundering-herd burst against the API server every `N` seconds. With
even 200+ CRs this shows up as periodic API-server CPU spikes and
degraded reconcile latency.

- New module **`controller/src/backoff.rs`** with the pure jitter math
  `apply_jitter_factor(base, factor, sample) -> Duration` plus two
  ergonomic helpers `with_jitter(Duration)` and
  `requeue_secs_with_jitter(u64)` that use `rand::rng()` for the
  sample. The ±20% default follows the Kubernetes ecosystem convention
  used by `k8s.io/apimachinery/pkg/util/wait` and most operator SDKs.
- All seven `error_policy` functions now route their requeue duration
  through `backoff::requeue_secs_with_jitter(...)`:
  - `controller/src/reconciler/mod.rs::error_requeue_duration`
    (ClawSandbox — 30s for transient `Kube`, 300s for deterministic
    `SerdeJson`).
  - `controller/src/pairing_reconciler.rs` (ClawPairing — 30s).
  - `controller/src/mcp_server_reconciler.rs` (McpServer — 30s).
  - `controller/src/tool_policy_reconciler.rs` (ToolPolicy — 30s).
  - `controller/src/a2a_agent_reconciler.rs` (A2AAgent — 30s).
  - `controller/src/inference_policy_reconciler.rs` (InferencePolicy — 30s).
  - `controller/src/claw_memory_reconciler.rs` (ClawMemory — 30s).
  - `controller/src/claw_eval_reconciler.rs` (ClawEval — 30s).

## Out of scope

- **Exponential backoff with state tracking** for repeated errors on
  the same CR. That requires per-key state (RetryCount in a sidecar
  map) and changes the reconcile-loop shape; deferred to S7.D.2 if
  operator demand warrants it. Bounded uniform jitter ±20% on a fixed
  base already eliminates the lockstep thundering-herd which is the
  primary practical problem.
- **Success-path requeues** (the long-poll `Action::requeue(300s)`
  pattern at the end of each happy reconcile). Those don't lockstep
  because the success requeue clock starts at *each individual CR's*
  reconcile completion, which is already naturally desynchronised
  by the workqueue. Adding jitter would not change real behaviour.
- **`Controller::trigger_backoff(...)`** integration — kube-rs
  3.1 supports a `Backoff` trait for the *trigger* stream's reconnect
  semantics. That's a different concern (informer reconnect) from
  reconciler retry pacing; can land alongside S7.E (workqueue metrics).

## Hard-rule checklist (`docs/implementation-plan.md` §0.2)

| # | Rule | Status |
|---|------|--------|
| 1 | No fork; no upstream re-implementation | ✓ — uses `rand` (already a workspace dep) |
| 3 | No file grew past Phase 2 cap | ✓ — new module 159 LOC; per-reconciler edit is 1-line |
| 8 | No custom-crypto / framing | ✓ — N/A |
| 9 | Audit doc with two sign-offs | ✓ — this doc |
| 10 | Verify, don't guess; cite sources | ✓ — k8s.io/apimachinery `wait.Jitter()` defaults; kube-rs 3.1 reconciler API confirmed |

## Test coverage

9 new unit tests in `backoff::tests`:
- `jitter_zero_factor_returns_base` (no jitter when factor=0).
- `jitter_zero_sample_subtracts_full_factor` (sample=0 → -factor).
- `jitter_max_sample_adds_full_factor` (sample=1 → +factor).
- `jitter_midpoint_sample_returns_base` (sample=0.5 → 1.0× multiplier).
- `jitter_factor_above_one_is_clamped` (defensive: factor=2 → clamp to 1).
- `jitter_negative_factor_is_clamped` (factor=-0.5 → clamp to 0).
- `jitter_never_returns_negative_duration` (multiplier never negative).
- `requeue_secs_with_jitter_stays_within_default_band` (50 RNG samples within ±20%).
- `jitter_distribution_is_not_constant` (32 RNG samples produce ≥3 distinct values — guards against a stub returning the base unchanged).

Controller bin tests: 336 → 345 (+9). Clippy `-D warnings` clean.
`cargo fmt --check` clean.

## Threat model

- **No new attack surface.** Jitter is a property of the requeue
  duration, observable only via the controller's local timing. It
  doesn't expose new state to any other principal.
- **Effective behavior delta.** A reconciler that previously retried
  *exactly* every 30s after error now retries every `30 ± 6` seconds.
  At worst this defers a single retry by 6s, which is well under
  every existing SLA on the affected paths (Foundry agent provisioning
  is minutes-scale; CRD validation is seconds-scale). At best it
  prevents API-server saturation under correlated transient failures.
- **No measurable impact on cold-start tail latency.** The first
  reconcile attempt is unaffected; jitter only applies to retries
  *after* the first failure, when the user is already waiting on a
  retry anyway.

## Existing implementation surveyed

- 7 `error_policy` functions across the 9 reconcilers (sandbox + pairing
  + 7 single-CRD reconcilers; mesh-peer and fedcred-reaper don't use
  Controller-style error_policy hooks).
- `Cargo.toml:67` — `rand = "0.9"` already in workspace deps; controller
  Cargo.toml line `rand.workspace = true` already wired.

## §14.6 / §15 impact

- §10.4 #11 (controller reliability under correlated failures): closed.
- §15.2 #10 (S7 craftsmanship): incremental progress; S7.E (workqueue
  metrics + reconcile spans) and S7.F (VAP/MAP expansion) remain. S7.C.2
  (predicated informers) deferred — would require enabling the unstable
  `unstable-runtime-stream-control` feature and refactoring all 9
  reconcilers to use `Controller::for_stream` with a manual reflector.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
