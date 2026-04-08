//! Prometheus metrics for inference routing and AGT governance.

use prometheus::{
    Histogram, HistogramVec, IntCounterVec, IntGauge, opts, register_histogram,
    register_histogram_vec, register_int_counter_vec, register_int_gauge,
};
use std::sync::LazyLock;

/// Total inference requests by sandbox, model, and status.
pub static INFERENCE_REQUESTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_inference_requests_total",
            "Total inference requests"
        ),
        &["sandbox", "model", "status"]
    )
    .unwrap()
});

/// Inference request latency in seconds.
pub static INFERENCE_LATENCY: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "azureclaw_inference_latency_seconds",
        "Inference request latency",
        &["sandbox", "model"],
        vec![0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]
    )
    .unwrap()
});

/// Token usage by sandbox and model.
pub static TOKENS_USED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!("azureclaw_tokens_total", "Total tokens consumed"),
        &["sandbox", "model", "direction"] // direction: input | output
    )
    .unwrap()
});

// ── AGT Governance metrics ──────────────────────────────────────────────────

/// Total AGT policy evaluations by decision (allow, deny, requires_approval, rate_limited).
pub static AGT_POLICY_EVALUATIONS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_agt_policy_evaluations_total",
            "Total AGT policy evaluations"
        ),
        &["decision"]
    )
    .unwrap()
});

/// AGT policy evaluation latency in seconds.
pub static AGT_EVAL_LATENCY: LazyLock<Histogram> = LazyLock::new(|| {
    register_histogram!(
        "azureclaw_agt_eval_latency_seconds",
        "AGT policy evaluation latency",
        vec![0.000_01, 0.000_05, 0.000_1, 0.000_5, 0.001, 0.005, 0.01]
    )
    .unwrap()
});

/// Number of known agents in the trust store.
pub static AGT_KNOWN_AGENTS: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(opts!(
        "azureclaw_agt_known_agents",
        "Number of agents in the trust store"
    ))
    .unwrap()
});

/// Total AGT audit log entries.
pub static AGT_AUDIT_ENTRIES: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(opts!(
        "azureclaw_agt_audit_entries_total",
        "Total AGT audit log entries"
    ))
    .unwrap()
});

/// Total content safety flags reported to AGT.
pub static AGT_CONTENT_FLAGS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_agt_content_flags_total",
            "Content safety flags reported to AGT"
        ),
        &["category"]
    )
    .unwrap()
});

/// Total behavior anomaly alerts.
pub static AGT_BEHAVIOR_ALERTS: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(opts!(
        "azureclaw_agt_behavior_alerts_total",
        "Cumulative behavior anomaly alerts"
    ))
    .unwrap()
});

/// Number of loaded policy rules.
pub static AGT_POLICY_RULES: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(opts!(
        "azureclaw_agt_policy_rules",
        "Number of loaded AGT policy rules"
    ))
    .unwrap()
});
