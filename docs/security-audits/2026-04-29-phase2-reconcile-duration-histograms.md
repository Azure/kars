# Security audit — Phase 2 / S7.E.2: reconcile-duration histograms + total counter

**Slice:** `phase2-reconcile-duration-histograms`
**Branch:** `phase2-reconcile-duration-histograms` → `dev`
**Date:** 2026-04-29
**Scope item:** `docs/implementation-plan.md` §9 P0 (operator craftsmanship — observability) — second half of S7.E. Counter-only surface shipped in PR #76; this slice adds the duration histogram + outcome-aware total counter that close out the workqueue-metrics gate.

## Summary

Adds `azureclaw_controller_reconcile_duration_seconds{crd_kind, outcome}` (Histogram) and `azureclaw_controller_reconcile_total{crd_kind, outcome}` (IntCounterVec) to the controller's `:9091/metrics` endpoint, threaded through every `Controller::run(...)` call site via a thin `metrics::observe_reconcile(crd_kind, fut)` wrapper. Operators now see both happy-path latency distribution and a single counter from which to compute success rate, complementing the existing error-only counters from S7.E.

## Existing implementation surveyed (per §0.2 #8 — no parallel-implementation)

- `controller/src/metrics.rs` (S7.E, PR #76) — `RECONCILE_ERRORS` / `RECONCILE_RETRIES` IntCounterVecs + `record_reconcile_error()` helper. Reused. `observe_reconcile` lives in the same module to keep the registry in one place.
- `controller/src/metrics_server.rs` (S7.E, PR #76) — axum `:9091` server + `/metrics` + `/healthz`. Untouched; new histograms surface automatically through `prometheus::default_registry()` gather.
- `inference-router/src/metrics.rs` `INFERENCE_LATENCY` — same `register_histogram_vec!` shape, same default-registry dependency, same labels-as-strict-set discipline. Bucket cadence intentionally adapted (controller reconcile is sub-second; router inference can be multi-second).
- All 8 reconcilers' `Controller::new(...).run(reconcile, error_policy, ctx)` call sites — untouched body; only the first arg wrapped with a closure that times via `observe_reconcile`.

No new dependency added. `prometheus` was already pulled in by S7.E.

## What this slice ships

1. **`metrics::RECONCILE_DURATION`** (`HistogramVec`, labels `crd_kind`, `outcome`):
   - Buckets: `0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0` seconds — covers fast in-memory reconciles (~1ms typical no-op) through tail latency from K8s API contention or Azure SDK calls (~10s+).
   - `outcome` ∈ {`success`, `error`} only — closed cardinality.
2. **`metrics::RECONCILE_TOTAL`** (`IntCounterVec`, same labels): single counter from which `irate(reconcile_total{outcome="success"}[5m]) / irate(reconcile_total[5m])` yields success rate without joining two counter families.
3. **`metrics::observe_reconcile<F, T, E>(crd_kind, fut)`** — generic wrapper consumed by all 8 reconcilers via a closure at the `Controller::run(...)` callsite. Records duration + total once on completion; passes through the `Result<T, E>` unchanged so `error_policy` keeps its existing semantics.
4. **8 wired call sites** — `reconciler/mod.rs` (ClawSandbox), `a2a_agent_reconciler.rs`, `claw_eval_reconciler.rs`, `claw_memory_reconciler.rs`, `inference_policy_reconciler.rs`, `mcp_server_reconciler.rs`, `pairing_reconciler.rs` (`ClawPairing`), `tool_policy_reconciler.rs`. Body of each `reconcile`/`reconcile_pairing` fn untouched.
5. **3 new unit tests** in `metrics.rs`:
   - `observe_reconcile_records_success_outcome` — Ok future increments success row of both metrics.
   - `observe_reconcile_records_error_outcome` — Err future increments error row only; success row untouched.
   - `observe_reconcile_renders_in_text_format` — both new metrics render in Prometheus text-format gather.

## Cardinality discipline

- `crd_kind` is one of 8 fixed values selected at the call site (string literal, never derived from a CR field).
- `outcome` is one of 2 fixed values.
- Total cardinality contribution: 8 × 2 = 16 series per metric. No CR `metadata.name` / `metadata.namespace` ever appears in labels.
- Bucket count: 13 — same order of magnitude as router's `INFERENCE_LATENCY` (8 buckets) and within Prometheus best-practice for per-CRD reconcile latency.

## Threat model deltas

None. Read-only Prometheus surface continues to bind on `0.0.0.0:9091` (S7.E behavior); cluster operators are expected to scope reachability via NetworkPolicy on the controller pod, same as S7.E shipped. No CR data is exposed in label values.

## Verification

```text
$ cargo build -p azureclaw-controller        # clean
$ cargo test  -p azureclaw-controller
   349 → 352 tests, all green
$ cargo clippy -p azureclaw-controller --all-targets -- -D warnings   # clean
$ cargo fmt --all -- --check                 # clean
```

## Deferred

- **OTel reconcile spans** — wrapping the same closure with a `tracing::info_span!("reconcile", crd_kind, ...)` is a single-line addition, but choosing the right propagation parent (incoming reconcile event vs. internal timer trigger) needs a dedicated audit on tracing-vs-metrics double-bookkeeping. Carved out as `phase2-reconcile-otel-spans`.
- **Workqueue depth gauge** — kube-rs's `Controller::stream_with_config` exposes the predicate-filtered stream but not workqueue depth directly; would need a custom reflector. Carved out alongside S7.C.2 predicated informers.

## Sign-offs

- Core: ✅
- Security: ✅
