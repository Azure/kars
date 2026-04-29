# Phase 2 — S7.E: controller workqueue metrics

**Date:** 2026-04-29
**Slice:** `phase2-controller-metrics` (sub-slice S7.E of S7 craftsmanship train)
**Author:** AzureClaw maintainers
**Sign-offs:** `@maintainer-1`, `@maintainer-2`

## Scope

Expose Prometheus metrics from the controller pod so operators can
SLO and alert on reconcile health without scraping logs.

- New module `controller/src/metrics.rs` — registers two
  `IntCounterVec`s on the global `prometheus::default_registry()`:
  - `azureclaw_controller_reconcile_errors_total{crd_kind, error_class}`
  - `azureclaw_controller_reconcile_retries_total{crd_kind}`

  Helper `record_reconcile_error(crd_kind, error_class)` increments
  both. Naming uses the `azureclaw_controller_*` prefix to
  disambiguate from the inference-router's `azureclaw_*` series; the
  `_total` suffix matches Prometheus naming conventions for
  monotonic counters.

- New module `controller/src/metrics_server.rs` — minimal axum HTTP
  server exposing `/metrics` (Prometheus text-format encoded by
  `prometheus::TextEncoder`) and `/healthz`. Bind address from
  `CONTROLLER_METRICS_ADDR` (default `0.0.0.0:9091`). Empty string
  or literal `disabled` opts out.

- All eight controller `error_policy` functions wired to call
  `record_reconcile_error(...)`. Sandbox + pairing use an inline
  `match` over their typed `ReconcileError` variants (no helper
  method to avoid a wider API surface change); the six other
  reconcilers reuse their existing `class()` impls.

- Helm chart: `controller-deployment.yaml` declares
  `containerPort: 9091, name: metrics`. Standard scrape annotation
  not added in this slice (cluster operators typically configure
  scrape via ServiceMonitor / PodMonitor; we don't ship those yet).

## Out of scope

- **Reconcile-duration histogram** — `azureclaw_controller_reconcile_duration_seconds_bucket{crd_kind}`. Wiring it requires a wrapper around each reconciler's reconcile function (start `Instant::now()`, observe on exit) and is more invasive than a single-call-site error counter. Deferred to S7.E.2; can land alongside reconcile-spans in OTel.
- **Workqueue-depth gauge** — kube-rs's `Controller` doesn't expose the workqueue length directly; would need our own queue or a custom predicate. Deferred to S7.E.2.
- **OTel reconcile spans** — separate concern from Prometheus metrics; lands when we have a clear span-emit story in the controller (today only the inference router emits OTel GenAI spans).
- **ServiceMonitor / PodMonitor CRDs** — clusters running Prometheus Operator can author their own; we don't take a Helm dep on Prometheus Operator types.

## Hard-rule checklist (`docs/implementation-plan.md` §0.2)

| # | Rule | Status |
|---|------|--------|
| 1 | No fork | ✓ — uses upstream `prometheus` 0.14 + `axum` 0.8 |
| 3 | No file grew past Phase 2 cap | ✓ — two new small modules |
| 8 | No custom-crypto / framing | ✓ — N/A |
| 9 | Audit doc with two sign-offs | ✓ — this doc |
| 10 | Verify, don't guess | ✓ — pattern mirrors `inference-router/src/metrics.rs` and `inference-router/src/routes/mod.rs::metrics()`; cited |

## Test coverage

4 new unit tests:
- `metrics::tests::record_reconcile_error_increments_both`.
- `metrics::tests::separate_error_classes_increment_independently`.
- `metrics::tests::metrics_render_in_text_format` (round-trip through `TextEncoder`).
- `metrics_server::tests::bind_addr_from_env_default` (read-only env probe; safe under parallel test execution).

Controller bin tests: 345 → 349 (+4). Clippy `-D warnings` clean.
`helm lint` clean.

## Threat model

- **Metrics endpoint exposes counts, not contents.** Counter labels
  are bounded enums (`crd_kind` in {ClawSandbox, ClawPairing,
  McpServer, ToolPolicy, A2AAgent, InferencePolicy, ClawMemory,
  ClawEval}; `error_class` in {kube_api, serde, dns, tls, timeout,
  http_status, invalid_jwks_format, …}). No CR names, namespaces,
  or user-supplied strings appear in label values, so cardinality
  stays bounded and label-leakage of CR identifiers is impossible.
- **Bind address default `0.0.0.0:9091`.** Reachable from any pod
  in the cluster but not externally (no Service of type
  `LoadBalancer` ships). Operators wanting strict exposure can set
  `CONTROLLER_METRICS_ADDR=127.0.0.1:9091` to limit to localhost
  + a sidecar scraper, or `disabled` to opt out entirely.
- **No new RBAC.** The metrics module needs only in-process state.
- **DOS surface.** The `/metrics` handler's cost is dominated by
  `prometheus::gather()` + `TextEncoder::encode()`. Both are
  bounded by the static counter set we register; an unauthenticated
  caller cannot expand the counter cardinality.

## Existing implementation surveyed

- `inference-router/src/metrics.rs` — registers ~16 metric families
  via `LazyLock`; we reuse the pattern.
- `inference-router/src/routes/mod.rs:321-328` — exact precedent for
  `/metrics` handler using `prometheus::TextEncoder` + `gather()`.
- `controller/Cargo.toml:29` — `prometheus.workspace = true` already
  declared (Phase 1 added it for the mesh-peer; was unused on the
  controller-binary side until now).

## §14.6 / §15 impact

- §10.4 #13 (workqueue metrics on `/metrics`): partial closure —
  error/retry counters land; duration histograms + queue depth
  gauges remain for S7.E.2.
- §15.2 #10 (S7 craftsmanship): incremental progress.
