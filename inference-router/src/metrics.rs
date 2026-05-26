// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Prometheus metrics for inference routing and AGT governance.

use prometheus::{
    Histogram, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec, opts, register_histogram,
    register_histogram_vec, register_int_counter_vec, register_int_gauge, register_int_gauge_vec,
};
use std::sync::LazyLock;

/// Total inference requests by sandbox, model, and status.
pub static INFERENCE_REQUESTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!("kars_inference_requests_total", "Total inference requests"),
        &["sandbox", "model", "status"]
    )
    .unwrap()
});

/// Inference request latency in seconds.
pub static INFERENCE_LATENCY: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "kars_inference_latency_seconds",
        "Inference request latency",
        &["sandbox", "model"],
        vec![0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]
    )
    .unwrap()
});

/// Token usage by sandbox and model.
pub static TOKENS_USED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!("kars_tokens_total", "Total tokens consumed"),
        &["sandbox", "model", "direction"] // direction: input | output
    )
    .unwrap()
});

// ── AGT Governance metrics ──────────────────────────────────────────────────

/// Total AGT policy evaluations by decision (allow, deny, requires_approval, rate_limited).
pub static AGT_POLICY_EVALUATIONS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "kars_agt_policy_evaluations_total",
            "Total AGT policy evaluations"
        ),
        &["decision"]
    )
    .unwrap()
});

/// AGT policy evaluation latency in seconds.
pub static AGT_EVAL_LATENCY: LazyLock<Histogram> = LazyLock::new(|| {
    register_histogram!(
        "kars_agt_eval_latency_seconds",
        "AGT policy evaluation latency",
        vec![0.000_01, 0.000_05, 0.000_1, 0.000_5, 0.001, 0.005, 0.01]
    )
    .unwrap()
});

/// Number of known agents in the trust store.
pub static AGT_KNOWN_AGENTS: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(opts!(
        "kars_agt_known_agents",
        "Number of agents in the trust store"
    ))
    .unwrap()
});

/// Total AGT audit log entries.
pub static AGT_AUDIT_ENTRIES: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(opts!(
        "kars_agt_audit_entries_total",
        "Total AGT audit log entries"
    ))
    .unwrap()
});

/// Total content safety flags reported to AGT.
pub static AGT_CONTENT_FLAGS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "kars_agt_content_flags_total",
            "Content safety flags reported to AGT"
        ),
        &["category"]
    )
    .unwrap()
});

/// Total AGT mesh messages sent by this router (egress to relay). The `sandbox`
/// label is added at scrape time by the PodMonitor relabeling, so the metric
/// itself has no extra labels — one counter per router process. Used by the
/// Headlamp Mesh Topology view + operator CLI for per-sandbox traffic.
pub static AGT_MESH_MESSAGES_SENT: LazyLock<prometheus::IntCounter> = LazyLock::new(|| {
    prometheus::register_int_counter!(opts!(
        "kars_mesh_messages_sent_total",
        "Total AGT mesh messages sent by this router to the relay"
    ))
    .unwrap()
});

/// Total AGT mesh messages received by this router (ingress from relay).
/// Same labelling story as `AGT_MESH_MESSAGES_SENT`.
pub static AGT_MESH_MESSAGES_RECEIVED: LazyLock<prometheus::IntCounter> = LazyLock::new(|| {
    prometheus::register_int_counter!(opts!(
        "kars_mesh_messages_received_total",
        "Total AGT mesh messages received by this router from the relay"
    ))
    .unwrap()
});

/// Total TrustGraph-projection-driven trust bootstraps. Incremented
/// once per peer whose initial AGT trust score was seeded from a
/// controller-verified TrustGraph edge (Phase F2). Never incremented
/// on subsequent updates — bootstrap is a one-time event per peer.
pub static AGT_TRUSTGRAPH_BOOTSTRAPS: LazyLock<prometheus::IntCounter> = LazyLock::new(|| {
    prometheus::register_int_counter!(opts!(
        "kars_agt_trustgraph_bootstraps_total",
        "Brand-new peers seeded with an AGT trust score derived from a TrustGraph projection edge"
    ))
    .unwrap()
});

/// Last loaded TrustGraph projection version-hash (gauge with the
/// hash as a label). One series per (sandbox, version_hash) pair.
/// Used to confirm in operator dashboards that all sandboxes are
/// observing the same controller-published projection.
pub static AGT_TRUSTGRAPH_PROJECTION_VERSION: LazyLock<prometheus::IntGaugeVec> =
    LazyLock::new(|| {
        prometheus::register_int_gauge_vec!(
            opts!(
                "kars_agt_trustgraph_projection_version",
                "Constant 1 — labeled with the loaded TrustGraph projection version-hash"
            ),
            &["version_hash"]
        )
        .unwrap()
    });

/// Total behavior anomaly alerts.
pub static AGT_BEHAVIOR_ALERTS: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(opts!(
        "kars_agt_behavior_alerts_total",
        "Cumulative behavior anomaly alerts"
    ))
    .unwrap()
});

/// Number of loaded policy rules.
pub static AGT_POLICY_RULES: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(opts!(
        "kars_agt_policy_rules",
        "Number of loaded AGT policy rules"
    ))
    .unwrap()
});

/// Total credential redactions by kind (AGT CredentialRedactor).
pub static AGT_REDACTIONS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "kars_agt_redactions_total",
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
            "kars_agt_response_threats_total",
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
            "kars_agt_tool_rate_limits_total",
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
            "kars_agt_message_signatures_total",
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
            "kars_handoff_pending_events_total",
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
            "kars_handoff_phase_transitions_total",
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
            "kars_upstream_retries_total",
            "Upstream Azure OpenAI retries on idempotent requests"
        ),
        &["sandbox", "reason"]
    )
    .unwrap()
});

// ──────────────────────────────────────────────────────────────────────
// Policy-bundle audit metrics (Slice §3 — Ready ⇔ router-echo loop)
//
// One gauge per `PolicyKind` (5 bundles total today: AgtProfile,
// InferencePolicy, Memory, EgressAllowlist, EgressApproval). Cardinality
// is bounded by the closed `PolicyKind` taxonomy in `policy_status.rs`.
// ──────────────────────────────────────────────────────────────────────

/// Unix-epoch timestamp (seconds) of the most recent **successful**
/// load for each policy bundle. Stays put across subsequent failures
/// so dashboards can show "last good state" age via
/// `time() - kars_policy_bundle_loaded_at_seconds`.
pub static POLICY_BUNDLE_LOADED_AT: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!(
        opts!(
            "kars_policy_bundle_loaded_at_seconds",
            "Unix timestamp of last successful policy-bundle load, per kind"
        ),
        &["kind"]
    )
    .unwrap()
});

/// Health gauge per policy bundle: `1` when the last load attempt
/// succeeded, `0` when the last attempt failed. Pair with
/// `kars_policy_bundle_reload_total{outcome="error"}` for
/// rate-of-failure alerts.
pub static POLICY_BUNDLE_HEALTHY: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!(
        opts!(
            "kars_policy_bundle_healthy",
            "1 if the most recent policy-bundle load succeeded, 0 otherwise"
        ),
        &["kind"]
    )
    .unwrap()
});

/// Total policy-bundle reload attempts by kind and outcome.
/// `outcome` ∈ `{"success", "error"}`. Use the `rate()` of the
/// `error` series for SLO burn alerts.
pub static POLICY_BUNDLE_RELOADS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        opts!(
            "kars_policy_bundle_reload_total",
            "Policy-bundle reload attempts by kind and outcome"
        ),
        &["kind", "outcome"]
    )
    .unwrap()
});
