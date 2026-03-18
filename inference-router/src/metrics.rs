//! Prometheus metrics for inference routing.

use std::sync::LazyLock;
use prometheus::{IntCounterVec, HistogramVec, opts, register_int_counter_vec, register_histogram_vec};

/// Total inference requests by sandbox, model, and status.
pub static INFERENCE_REQUESTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!("azureclaw_inference_requests_total", "Total inference requests"),
        &["sandbox", "model", "status"]
    ).unwrap()
});

/// Inference request latency in seconds.
pub static INFERENCE_LATENCY: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "azureclaw_inference_latency_seconds",
        "Inference request latency",
        &["sandbox", "model"],
        vec![0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]
    ).unwrap()
});

/// Token usage by sandbox and model.
pub static TOKENS_USED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!("azureclaw_tokens_total", "Total tokens consumed"),
        &["sandbox", "model", "direction"] // direction: input | output
    ).unwrap()
});
