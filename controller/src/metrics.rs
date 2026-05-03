// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Prometheus metrics for the AzureClaw controller.
//!
//! S7.E (workqueue metrics): exposes counters / histograms that
//! operators can scrape from the controller pod's `:9091/metrics`
//! endpoint. S7.E shipped the error-counter surface; **S7.E.2**
//! adds the reconcile-duration histogram + total-reconcile counter
//! via [`observe_reconcile`], a thin wrapper threaded through each
//! `Controller::run(...)` call site.
//!
//! Naming follows `azureclaw_controller_*` to disambiguate from the
//! inference router's `azureclaw_*` series, mirroring the
//! controller-runtime `controller_runtime_*` convention used by
//! kubebuilder/operator-sdk-style operators.

use prometheus::{
    HistogramVec, IntCounterVec, opts, register_histogram_vec, register_int_counter_vec,
};
use std::sync::LazyLock;

/// Total reconcile errors by CRD kind and error class.
///
/// Labels:
/// - `crd_kind`: `ClawSandbox` | `ClawPairing` | `McpServer` |
///   `ToolPolicy` | `A2AAgent` | `InferencePolicy` | `ClawMemory` |
///   `ClawEval`.
/// - `error_class`: per-reconciler error variant tag, e.g.
///   `kube` / `serde_json` / `policy_compile` / `unknown`. Stays
///   small-cardinality.
pub static RECONCILE_ERRORS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_controller_reconcile_errors_total",
            "Total reconcile errors emitted by the controller, by CRD kind and error class"
        ),
        &["crd_kind", "error_class"]
    )
    .expect("failed to register azureclaw_controller_reconcile_errors_total")
});

/// Total reconcile retries scheduled (every error_policy invocation).
///
/// Distinct from `RECONCILE_ERRORS` so an operator can sanity-check
/// that retry counts equal error counts (drift would mean a bug in
/// the error handling chain).
pub static RECONCILE_RETRIES: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_controller_reconcile_retries_total",
            "Total reconcile retries scheduled (each error_policy emission)"
        ),
        &["crd_kind"]
    )
    .expect("failed to register azureclaw_controller_reconcile_retries_total")
});

/// Convenience: increment both error + retry counters for a kind.
pub fn record_reconcile_error(crd_kind: &str, error_class: &str) {
    RECONCILE_ERRORS
        .with_label_values(&[crd_kind, error_class])
        .inc();
    RECONCILE_RETRIES.with_label_values(&[crd_kind]).inc();
}

/// Reconcile duration in seconds, by CRD kind and outcome.
///
/// Buckets follow the controller-runtime convention (sub-second
/// happy path; long-tail caught up to 30s before the operator
/// would notice via Prometheus alerting).
///
/// Labels:
/// - `crd_kind`: same set as [`RECONCILE_ERRORS`].
/// - `outcome`: `success` | `error`.
pub static RECONCILE_DURATION: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "azureclaw_controller_reconcile_duration_seconds",
        "Reconcile duration in seconds, by CRD kind and outcome",
        &["crd_kind", "outcome"],
        vec![
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0
        ]
    )
    .expect("failed to register azureclaw_controller_reconcile_duration_seconds")
});

/// Total reconcile invocations by CRD kind and outcome.
///
/// Distinct from [`RECONCILE_ERRORS`] (which only counts errors and
/// is wired through `error_policy`) — `RECONCILE_TOTAL` covers both
/// `success` and `error` paths via [`observe_reconcile`], so the
/// operator can compute success rate as
/// `success / (success + error)` directly from a single metric.
pub static RECONCILE_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_controller_reconcile_total",
            "Total reconcile invocations by CRD kind and outcome"
        ),
        &["crd_kind", "outcome"]
    )
    .expect("failed to register azureclaw_controller_reconcile_total")
});

/// Total status-patch skips (idempotency cache hits) by CRD kind.
///
/// P2 #10: each reconcile pass diffs the desired status against the
/// observed `.status` and skips the API patch when they match
/// (`running_status_matches_with_extras` etc.). This counter
/// surfaces how often that fast path fires; a sudden drop indicates
/// a regression where every pass re-patches and the controller
/// burns API quota.
pub static STATUS_PATCH_SKIPS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_controller_skip_cache_hits_total",
            "Total status-patch skips (idempotency cache hits) by CRD kind"
        ),
        &["crd_kind"]
    )
    .expect("failed to register azureclaw_controller_skip_cache_hits_total")
});

/// Increment the skip-cache counter for a CRD kind.
pub fn record_status_patch_skip(crd_kind: &str) {
    STATUS_PATCH_SKIPS.with_label_values(&[crd_kind]).inc();
}

/// Total condition status-flips by condition type and the new
/// status value.
///
/// P2 #10: incremented inside [`crate::status::conditions::preserve_transition_time`]
/// whenever a condition transitions (i.e. a fresh
/// `last_transition_time` is stamped). Lets operators alert on
/// e.g. a `Ready -> False` flap rate without parsing CR yaml.
///
/// Labels:
/// - `condition_type`: e.g. `Ready` / `Progressing` / `Suspended` /
///   `RuntimeReady` / `AuditIntegrity`.
/// - `new_status`: `True` / `False` / `Unknown`.
pub static CONDITION_TRANSITIONS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_controller_conditions_transitions_total",
            "Total condition status-flips by condition type and new status value"
        ),
        &["condition_type", "new_status"]
    )
    .expect("failed to register azureclaw_controller_conditions_transitions_total")
});

/// Increment the condition-transition counter.
pub fn record_condition_transition(condition_type: &str, new_status: &str) {
    CONDITION_TRANSITIONS
        .with_label_values(&[condition_type, new_status])
        .inc();
}

/// Wraps a reconcile future, recording its duration + outcome on
/// completion. Generic over the `Result` so each reconciler keeps
/// its own `ReconcileError` type; we only need `is_ok()`.
///
/// Usage at the `Controller::run(...)` call site:
///
/// ```ignore
/// .run(
///     |x, ctx| async move {
///         crate::metrics::observe_reconcile("ClawSandbox", reconcile(x, ctx)).await
///     },
///     error_policy,
///     ctx,
/// )
/// ```
pub async fn observe_reconcile<F, T, E>(crd_kind: &'static str, fut: F) -> Result<T, E>
where
    F: std::future::Future<Output = Result<T, E>>,
{
    let start = std::time::Instant::now();
    let result = fut.await;
    let outcome = if result.is_ok() { "success" } else { "error" };
    RECONCILE_DURATION
        .with_label_values(&[crd_kind, outcome])
        .observe(start.elapsed().as_secs_f64());
    RECONCILE_TOTAL
        .with_label_values(&[crd_kind, outcome])
        .inc();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_reconcile_error_increments_both() {
        let before_err = RECONCILE_ERRORS
            .with_label_values(&["TestKind", "kube"])
            .get();
        let before_retry = RECONCILE_RETRIES.with_label_values(&["TestKind"]).get();

        record_reconcile_error("TestKind", "kube");

        assert_eq!(
            RECONCILE_ERRORS
                .with_label_values(&["TestKind", "kube"])
                .get(),
            before_err + 1
        );
        assert_eq!(
            RECONCILE_RETRIES.with_label_values(&["TestKind"]).get(),
            before_retry + 1
        );
    }

    #[test]
    fn separate_error_classes_increment_independently() {
        let before_kube = RECONCILE_ERRORS.with_label_values(&["KindB", "kube"]).get();
        let before_serde = RECONCILE_ERRORS
            .with_label_values(&["KindB", "serde_json"])
            .get();

        record_reconcile_error("KindB", "kube");

        assert_eq!(
            RECONCILE_ERRORS.with_label_values(&["KindB", "kube"]).get(),
            before_kube + 1
        );
        assert_eq!(
            RECONCILE_ERRORS
                .with_label_values(&["KindB", "serde_json"])
                .get(),
            before_serde
        );
    }

    #[test]
    fn metrics_render_in_text_format() {
        record_reconcile_error("RenderKind", "kube");
        let mut buf = Vec::new();
        let encoder = prometheus::TextEncoder::new();
        let families = prometheus::gather();
        prometheus::Encoder::encode(&encoder, &families, &mut buf).unwrap();
        let rendered = String::from_utf8(buf).unwrap();
        assert!(rendered.contains("azureclaw_controller_reconcile_errors_total"));
        assert!(rendered.contains("azureclaw_controller_reconcile_retries_total"));
        assert!(rendered.contains("RenderKind"));
    }

    #[tokio::test]
    async fn observe_reconcile_records_success_outcome() {
        let before_total = RECONCILE_TOTAL
            .with_label_values(&["DurKindOk", "success"])
            .get();
        let before_count = RECONCILE_DURATION
            .with_label_values(&["DurKindOk", "success"])
            .get_sample_count();

        let r: Result<u32, &'static str> =
            observe_reconcile("DurKindOk", async { Ok::<u32, &'static str>(42) }).await;

        assert_eq!(r.unwrap(), 42);
        assert_eq!(
            RECONCILE_TOTAL
                .with_label_values(&["DurKindOk", "success"])
                .get(),
            before_total + 1
        );
        assert_eq!(
            RECONCILE_DURATION
                .with_label_values(&["DurKindOk", "success"])
                .get_sample_count(),
            before_count + 1
        );
    }

    #[tokio::test]
    async fn observe_reconcile_records_error_outcome() {
        let before_total = RECONCILE_TOTAL
            .with_label_values(&["DurKindErr", "error"])
            .get();

        let r: Result<u32, &'static str> =
            observe_reconcile("DurKindErr", async { Err::<u32, &'static str>("boom") }).await;

        assert!(r.is_err());
        assert_eq!(
            RECONCILE_TOTAL
                .with_label_values(&["DurKindErr", "error"])
                .get(),
            before_total + 1
        );
        // Success counter for this kind must NOT have been touched.
        assert_eq!(
            RECONCILE_TOTAL
                .with_label_values(&["DurKindErr", "success"])
                .get(),
            0
        );
    }

    #[tokio::test]
    async fn observe_reconcile_renders_in_text_format() {
        let _ = observe_reconcile("RenderDurKind", async { Ok::<(), &'static str>(()) }).await;
        let mut buf = Vec::new();
        let encoder = prometheus::TextEncoder::new();
        let families = prometheus::gather();
        prometheus::Encoder::encode(&encoder, &families, &mut buf).unwrap();
        let rendered = String::from_utf8(buf).unwrap();
        assert!(rendered.contains("azureclaw_controller_reconcile_duration_seconds"));
        assert!(rendered.contains("azureclaw_controller_reconcile_total"));
        assert!(rendered.contains("RenderDurKind"));
    }

    #[test]
    fn record_status_patch_skip_increments_counter() {
        let before = STATUS_PATCH_SKIPS.with_label_values(&["SkipKind"]).get();
        record_status_patch_skip("SkipKind");
        record_status_patch_skip("SkipKind");
        assert_eq!(
            STATUS_PATCH_SKIPS.with_label_values(&["SkipKind"]).get(),
            before + 2
        );
    }

    #[test]
    fn record_condition_transition_uses_independent_label_pairs() {
        let before_true = CONDITION_TRANSITIONS
            .with_label_values(&["FlapKind", "True"])
            .get();
        let before_false = CONDITION_TRANSITIONS
            .with_label_values(&["FlapKind", "False"])
            .get();
        record_condition_transition("FlapKind", "True");
        record_condition_transition("FlapKind", "False");
        record_condition_transition("FlapKind", "False");
        assert_eq!(
            CONDITION_TRANSITIONS
                .with_label_values(&["FlapKind", "True"])
                .get(),
            before_true + 1
        );
        assert_eq!(
            CONDITION_TRANSITIONS
                .with_label_values(&["FlapKind", "False"])
                .get(),
            before_false + 2
        );
    }

    #[test]
    fn skip_and_transition_metrics_render_in_text_format() {
        record_status_patch_skip("RenderSkipKind");
        record_condition_transition("RenderTransKind", "True");
        let mut buf = Vec::new();
        let encoder = prometheus::TextEncoder::new();
        let families = prometheus::gather();
        prometheus::Encoder::encode(&encoder, &families, &mut buf).unwrap();
        let rendered = String::from_utf8(buf).unwrap();
        assert!(rendered.contains("azureclaw_controller_skip_cache_hits_total"));
        assert!(rendered.contains("azureclaw_controller_conditions_transitions_total"));
        assert!(rendered.contains("RenderSkipKind"));
        assert!(rendered.contains("RenderTransKind"));
    }
}
