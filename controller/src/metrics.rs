//! Prometheus metrics for the AzureClaw controller.
//!
//! S7.E (workqueue metrics): exposes counters / histograms that
//! operators can scrape from the controller pod's `:9091/metrics`
//! endpoint. Phase 2 ships the error-counter surface; S7.E.2 will
//! add reconcile-duration histograms + success counters once the
//! call sites are factored to share a wrapper.
//!
//! Naming follows `azureclaw_controller_*` to disambiguate from the
//! inference router's `azureclaw_*` series, mirroring the
//! controller-runtime `controller_runtime_*` convention used by
//! kubebuilder/operator-sdk-style operators.

use prometheus::{IntCounterVec, opts, register_int_counter_vec};
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
}
