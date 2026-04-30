// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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

/// Total credential redactions by kind (AGT CredentialRedactor).
pub static AGT_REDACTIONS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_agt_redactions_total",
            "Credentials redacted from output"
        ),
        &["kind"]
    )
    .unwrap()
});

/// Response threats detected by type (AGT McpResponseScanner).
pub static AGT_RESPONSE_THREATS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_agt_response_threats_total",
            "Response threats detected (prompt injection, exfil, etc.)"
        ),
        &["type"]
    )
    .unwrap()
});

/// Per-tool rate limit denials (AGT McpSlidingRateLimiter).
pub static AGT_TOOL_RATE_LIMITS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_agt_tool_rate_limits_total",
            "Per-tool sliding window rate limit denials"
        ),
        &["tool"]
    )
    .unwrap()
});

/// Ed25519 message signature operations by action.
pub static AGT_MESSAGE_SIGNATURES: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_agt_message_signatures_total",
            "Ed25519 message signing and verification"
        ),
        &["action"]
    )
    .unwrap()
});

// ── Handoff metrics ────────────────────────────────────────────────────────

/// Pending-handoff lifecycle events.
///
/// Action labels:
/// - `created` — new pending request accepted.
/// - `rate_limited` — request rejected by cooldown.
/// - `confirmed` — confirmation token accepted.
/// - `invalid_token` — confirm called with a wrong token.
/// - `too_fast` — confirm called before `CONFIRMATION_MIN_DELAY_SECS`.
/// - `expired` — pending request aged past TTL before confirm.
/// - `no_pending` — confirm called with no outstanding request.
/// - `cancelled` — explicit cancel() call.
pub static HANDOFF_PENDING_EVENTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_handoff_pending_events_total",
            "Pending-handoff lifecycle events"
        ),
        &["action"]
    )
    .unwrap()
});

/// Handoff session phase transitions.
///
/// Labels:
/// - `from` / `to` — phase names (lowercase).
/// - `result` — `ok` or `rejected` (invalid transition).
pub static HANDOFF_PHASE_TRANSITIONS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_handoff_phase_transitions_total",
            "Handoff session phase transitions"
        ),
        &["from", "to", "result"]
    )
    .unwrap()
});

/// Upstream (Azure OpenAI) retry count, broken out by reason.
///
/// Labels:
/// - `sandbox` — sandbox name.
/// - `reason` — `transport` (connect/timeout) or `status` (502/503/504).
pub static UPSTREAM_RETRIES: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "azureclaw_upstream_retries_total",
            "Upstream Azure OpenAI retries on idempotent requests"
        ),
        &["sandbox", "reason"]
    )
    .unwrap()
});
