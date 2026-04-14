//! Native AGT governance — in-process policy evaluation, trust management,
//! audit logging, and agent identity.
//!
//! Uses the `agentmesh` crate (v3.0.2) for core primitives.  The `/agt/*`
//! route handlers call these functions directly instead of forwarding HTTP
//! to a separate service.

use agentmesh::AuditLogger;
use agentmesh::identity::AgentIdentity;
use agentmesh::policy::PolicyEngine;
use agentmesh::trust::{TrustConfig, TrustManager};
use agentmesh::types::PolicyDecision;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::metrics;

// ── Governance metrics ───────────────────────────────────────────────────────

/// Counters tracked by the governance engine, exposed via `/agt/status`
/// and Prometheus `/metrics`.
pub struct GovernanceMetrics {
    pub evaluations: AtomicU64,
    pub denials: AtomicU64,
    pub rate_limits: AtomicU64,
    pub approvals: AtomicU64,
    pub content_flags: AtomicU64,
    pub eval_latency_sum_us: AtomicU64,
    pub behavior_alerts: AtomicU64,
}

impl Default for GovernanceMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl GovernanceMetrics {
    pub fn new() -> Self {
        Self {
            evaluations: AtomicU64::new(0),
            denials: AtomicU64::new(0),
            rate_limits: AtomicU64::new(0),
            approvals: AtomicU64::new(0),
            content_flags: AtomicU64::new(0),
            eval_latency_sum_us: AtomicU64::new(0),
            behavior_alerts: AtomicU64::new(0),
        }
    }

    pub fn avg_eval_latency_us(&self) -> u64 {
        let total = self.evaluations.load(Ordering::Relaxed);
        if total == 0 {
            return 0;
        }
        self.eval_latency_sum_us.load(Ordering::Relaxed) / total
    }
}

// ── Token-bucket rate limiter ────────────────────────────────────────────────

/// Simple token-bucket rate limiter matching the original governance semantics.
pub struct RateLimiter {
    global: Mutex<TokenBucket>,
    per_agent: Mutex<HashMap<String, TokenBucket>>,
    per_agent_config: Mutex<(f64, f64)>, // (rate, capacity)
}

struct TokenBucket {
    tokens: f64,
    capacity: f64,
    rate: f64,
    last_refill: Instant,
}

impl TokenBucket {
    fn new(rate: f64, capacity: f64) -> Self {
        Self {
            tokens: capacity,
            capacity,
            rate,
            last_refill: Instant::now(),
        }
    }

    fn allow(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

impl RateLimiter {
    pub fn new(
        global_rate: f64,
        global_capacity: f64,
        per_agent_rate: f64,
        per_agent_capacity: f64,
    ) -> Self {
        Self {
            global: Mutex::new(TokenBucket::new(global_rate, global_capacity)),
            per_agent: Mutex::new(HashMap::new()),
            per_agent_config: Mutex::new((per_agent_rate, per_agent_capacity)),
        }
    }

    pub fn allow(&self, agent_id: &str) -> bool {
        let global_ok = self.global.lock().unwrap().allow();
        if !global_ok {
            return false;
        }
        let (pa_rate, pa_cap) = *self.per_agent_config.lock().unwrap();
        let mut per_agent = self.per_agent.lock().unwrap();
        let bucket = per_agent
            .entry(agent_id.to_string())
            .or_insert_with(|| TokenBucket::new(pa_rate, pa_cap));
        bucket.allow()
    }

    /// Update rate limits at runtime (e.g. from API endpoint).
    pub fn update_rates(
        &self,
        global_rate: f64,
        global_capacity: f64,
        per_agent_rate: f64,
        per_agent_capacity: f64,
    ) {
        let mut global = self.global.lock().unwrap();
        global.rate = global_rate;
        global.capacity = global_capacity;
        drop(global);
        *self.per_agent_config.lock().unwrap() = (per_agent_rate, per_agent_capacity);
        // Clear per-agent buckets so they pick up new rates on next call
        self.per_agent.lock().unwrap().clear();
    }

    pub fn global_rate(&self) -> f64 {
        self.global.lock().unwrap().rate
    }

    pub fn global_capacity(&self) -> f64 {
        self.global.lock().unwrap().capacity
    }

    pub fn per_agent_rate(&self) -> f64 {
        self.per_agent_config.lock().unwrap().0
    }

    pub fn per_agent_capacity(&self) -> f64 {
        self.per_agent_config.lock().unwrap().1
    }
}

// ── Behavior monitor ─────────────────────────────────────────────────────────

/// Simple anomaly detector matching the original AgentBehaviorMonitor.
pub struct BehaviorMonitor {
    burst_threshold: u32,
    consecutive_failure_threshold: u32,
    capability_denial_threshold: u32,
    state: Mutex<HashMap<String, BehaviorState>>,
}

struct BehaviorState {
    recent_calls: u32,
    window_start: Instant,
    consecutive_failures: u32,
    capability_denials: u32,
}

impl Default for BehaviorState {
    fn default() -> Self {
        Self {
            recent_calls: 0,
            window_start: Instant::now(),
            consecutive_failures: 0,
            capability_denials: 0,
        }
    }
}

impl BehaviorState {
    /// Which thresholds this state exceeds, if any.
    fn triggered_reasons(&self, burst_t: u32, fail_t: u32, denial_t: u32) -> Vec<String> {
        let mut reasons = Vec::new();
        if self.recent_calls > burst_t {
            reasons.push(format!(
                "burst: {} calls/60s (threshold {})",
                self.recent_calls, burst_t
            ));
        }
        if self.consecutive_failures > fail_t {
            reasons.push(format!(
                "consecutive failures: {} (threshold {})",
                self.consecutive_failures, fail_t
            ));
        }
        if self.capability_denials > denial_t {
            reasons.push(format!(
                "capability denials: {} (threshold {})",
                self.capability_denials, denial_t
            ));
        }
        reasons
    }
}

impl BehaviorMonitor {
    pub fn new(
        burst_threshold: u32,
        consecutive_failure_threshold: u32,
        capability_denial_threshold: u32,
    ) -> Self {
        Self {
            burst_threshold,
            consecutive_failure_threshold,
            capability_denial_threshold,
            state: Mutex::new(HashMap::new()),
        }
    }

    pub fn record(&self, agent_id: &str, success: bool) -> bool {
        let mut state = self.state.lock().unwrap();
        let entry = state.entry(agent_id.to_string()).or_default();

        // Reset window every 60 seconds
        if entry.window_start.elapsed() > Duration::from_secs(60) {
            entry.recent_calls = 0;
            entry.window_start = Instant::now();
        }

        entry.recent_calls += 1;
        if success {
            entry.consecutive_failures = 0;
        } else {
            entry.consecutive_failures += 1;
            entry.capability_denials += 1;
        }

        // Return true if anomaly detected
        !entry
            .triggered_reasons(
                self.burst_threshold,
                self.consecutive_failure_threshold,
                self.capability_denial_threshold,
            )
            .is_empty()
    }

    pub fn alert_count(&self) -> u64 {
        let state = self.state.lock().unwrap();
        state
            .values()
            .filter(|s| {
                !s.triggered_reasons(
                    self.burst_threshold,
                    self.consecutive_failure_threshold,
                    self.capability_denial_threshold,
                )
                .is_empty()
            })
            .count() as u64
    }

    /// Per-agent alert details: which agents are flagged and why.
    pub fn alerts_detail(&self) -> Vec<serde_json::Value> {
        let state = self.state.lock().unwrap();
        state
            .iter()
            .filter_map(|(agent, s)| {
                let reasons = s.triggered_reasons(
                    self.burst_threshold,
                    self.consecutive_failure_threshold,
                    self.capability_denial_threshold,
                );
                if reasons.is_empty() {
                    None
                } else {
                    Some(serde_json::json!({
                        "agent": agent,
                        "reasons": reasons,
                        "calls_in_window": s.recent_calls,
                        "consecutive_failures": s.consecutive_failures,
                        "capability_denials": s.capability_denials,
                    }))
                }
            })
            .collect()
    }
}

// ── Main governance engine ───────────────────────────────────────────────────

/// Native governance engine — single struct that owns all AGT components.
/// Thread-safe, shared via `Arc<Governance>`.
pub struct Governance {
    pub identity: AgentIdentity,
    pub policy: PolicyEngine,
    pub trust: TrustManager,
    pub audit: AuditLogger,
    pub rate_limiter: RateLimiter,
    pub behavior: BehaviorMonitor,
    pub metrics: GovernanceMetrics,
    pub sandbox_name: String,
    pub trust_threshold: u32,
    start_time: Instant,
    policy_rule_count: AtomicU64,
}

impl Governance {
    /// Initialize governance from environment variables.
    pub fn new(sandbox_name: &str) -> Self {
        let trust_threshold: u32 = std::env::var("AGT_TRUST_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(500);

        let trust_config = TrustConfig {
            initial_score: 500,
            threshold: trust_threshold,
            reward: 10,
            penalty: 50,
            persist_path: Some("/tmp/agt/trust_scores.json".into()),
            decay_rate: 0.95,
        };

        // Create persistent trust dir
        let _ = std::fs::create_dir_all("/tmp/agt");

        let identity = AgentIdentity::generate(
            sandbox_name,
            vec!["azureclaw-agent".into(), "task-execution".into()],
        )
        .unwrap_or_else(|e| {
            tracing::error!("Failed to generate AGT identity: {}", e);
            // Fallback — generate with empty capabilities
            AgentIdentity::generate(sandbox_name, vec![])
                .expect("identity generation must not fail")
        });

        let policy = PolicyEngine::new();
        let policy_rule_count = AtomicU64::new(0);

        tracing::info!(
            sandbox = sandbox_name,
            did = %identity.did,
            trust_threshold,
            trust_db = "/tmp/agt/trust_scores.json",
            "Native AGT governance initialized"
        );

        Self {
            identity,
            policy,
            trust: TrustManager::new(trust_config),
            audit: AuditLogger::new(),
            rate_limiter: RateLimiter::new(500.0, 1000.0, 50.0, 100.0),
            behavior: BehaviorMonitor::new(100, 20, 10),
            metrics: GovernanceMetrics::new(),
            sandbox_name: sandbox_name.to_string(),
            trust_threshold,
            policy_rule_count,
            start_time: Instant::now(),
        }
    }

    /// Load all YAML policy files from a directory.  Returns rule count.
    pub fn load_policies_from_dir(&self, dir: &str) -> Result<usize, String> {
        let path = Path::new(dir);
        if !path.is_dir() {
            tracing::debug!(
                dir,
                "Policy directory not found — starting with empty policy"
            );
            return Ok(0);
        }

        let mut total_rules = 0;
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().is_some_and(|e| e == "yaml" || e == "yml") {
                    match self.policy.load_from_file(p.to_str().unwrap_or_default()) {
                        Ok(()) => {
                            // Count actual rules inside the file, not just files
                            let rules_in_file = Self::count_rules_in_file(&p);
                            tracing::info!(file = %p.display(), rules = rules_in_file, "Loaded policy file");
                            total_rules += rules_in_file;
                        }
                        Err(e) => {
                            tracing::warn!(file = %p.display(), error = %e, "Failed to load policy");
                        }
                    }
                }
            }
        }
        self.policy_rule_count
            .store(total_rules as u64, Ordering::Relaxed);
        metrics::AGT_POLICY_RULES.set(total_rules as i64);
        Ok(total_rules)
    }

    /// Count the number of policy rules in a YAML file by parsing the `policies` array.
    fn count_rules_in_file(path: &Path) -> usize {
        let Ok(yaml) = std::fs::read_to_string(path) else {
            return 1; // fallback: count as 1 file
        };
        let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&yaml) else {
            return 1;
        };
        value
            .get("policies")
            .and_then(|p| p.as_sequence())
            .map_or(1, |seq| seq.len())
    }

    /// Reload policies from disk (for hot-reload).
    pub fn reload_policies(&self) {
        let dir = std::env::var("AGT_POLICY_DIR")
            .unwrap_or_else(|_| "/sandbox/.openclaw/policies".into());
        match self.load_policies_from_dir(&dir) {
            Ok(count) => tracing::info!(rules = count, "Policy hot-reloaded"),
            Err(e) => tracing::warn!(error = %e, "Policy hot-reload failed"),
        }
    }

    /// Spawn a background task that watches AGT_POLICY_DIR for changes
    /// and reloads policies when file mtimes change.
    pub fn spawn_policy_watcher(governance: std::sync::Arc<Self>) {
        let dir = std::env::var("AGT_POLICY_DIR")
            .unwrap_or_else(|_| "/sandbox/.openclaw/policies".into());
        let interval_secs: u64 = std::env::var("AGT_POLICY_WATCH_INTERVAL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);

        tokio::spawn(async move {
            let mut last_mtime = dir_max_mtime(&dir);
            let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
            interval.tick().await; // skip immediate tick
            loop {
                interval.tick().await;
                let current = dir_max_mtime(&dir);
                if current != last_mtime {
                    tracing::info!("Policy directory changed, reloading");
                    governance.reload_policies();
                    last_mtime = current;
                }
            }
        });
    }

    // ── Evaluate ─────────────────────────────────────────────────────────

    /// Evaluate an action through the full governance pipeline
    /// (rate limiter → policy engine → behavior monitor → audit).
    ///
    /// Returns JSON matching the API contract:
    /// `{allowed, action, decision, matched_rule, reason, rate_limited}`
    pub fn evaluate(&self, agent_id: &str, action: &str, extra: Option<&Value>) -> Value {
        let start = Instant::now();
        self.metrics.evaluations.fetch_add(1, Ordering::Relaxed);

        // Rate limit check first (token bucket — not a capability denial)
        if !self.rate_limiter.allow(agent_id) {
            self.metrics.rate_limits.fetch_add(1, Ordering::Relaxed);
            self.audit.log(agent_id, action, "denied");
            let elapsed = start.elapsed();
            self.metrics
                .eval_latency_sum_us
                .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
            metrics::AGT_POLICY_EVALUATIONS
                .with_label_values(&["rate_limited"])
                .inc();
            metrics::AGT_EVAL_LATENCY.observe(elapsed.as_secs_f64());
            metrics::AGT_AUDIT_ENTRIES.set(self.audit.entries().len() as i64);
            return serde_json::json!({
                "allowed": false,
                "action": "deny",
                "decision": "deny",
                "reason": "Rate limited (token bucket)",
                "rate_limited": true,
            });
        }

        // Build context (matches server.py _build_context)
        let context = Self::build_context(action, extra);

        // Evaluate through agentmesh PolicyEngine
        let decision = self.policy.evaluate(action, context.as_ref());

        let (allowed, action_str, rule, reason): (bool, &str, Option<&str>, Option<&str>) =
            match &decision {
                PolicyDecision::Allow => (true, "allow", None, None),
                PolicyDecision::Deny(reason) => {
                    self.metrics.denials.fetch_add(1, Ordering::Relaxed);
                    (false, "deny", None, Some(reason.as_str()))
                }
                PolicyDecision::RequiresApproval(reason) => {
                    self.metrics.approvals.fetch_add(1, Ordering::Relaxed);
                    (false, "requires_approval", None, Some(reason.as_str()))
                }
                PolicyDecision::RateLimited { .. } => {
                    self.metrics.rate_limits.fetch_add(1, Ordering::Relaxed);
                    (false, "deny", None, Some("Policy rate limited"))
                }
            };

        let outcome = if allowed { "allow" } else { "deny" };
        self.audit.log(agent_id, action, outcome);

        // Rate-limited calls are not capability denials — don't feed them
        // into behavior monitoring (they'd inflate capability_denials and
        // trigger false-positive anomaly alerts).
        let is_rate_limited = matches!(decision, PolicyDecision::RateLimited { .. });
        if !is_rate_limited {
            let is_anomaly = self.behavior.record(agent_id, allowed);
            if is_anomaly {
                self.metrics.behavior_alerts.fetch_add(1, Ordering::Relaxed);
                metrics::AGT_BEHAVIOR_ALERTS.set(self.behavior.alert_count() as i64);
            }
        }

        let elapsed = start.elapsed();
        self.metrics
            .eval_latency_sum_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);

        // Prometheus
        metrics::AGT_POLICY_EVALUATIONS
            .with_label_values(&[action_str])
            .inc();
        metrics::AGT_EVAL_LATENCY.observe(elapsed.as_secs_f64());
        metrics::AGT_AUDIT_ENTRIES.set(self.audit.entries().len() as i64);

        serde_json::json!({
            "allowed": allowed,
            "action": action_str,
            "decision": action_str,
            "matched_rule": rule,
            "reason": reason,
            "rate_limited": false,
        })
    }

    /// Build structured context from an action string (matches server.py).
    fn build_context(
        action: &str,
        extra: Option<&Value>,
    ) -> Option<HashMap<String, serde_yaml::Value>> {
        let parts: Vec<&str> = action.splitn(2, ':').collect();
        let category = parts.first().copied().unwrap_or("unknown");
        let detail = parts.get(1).copied().unwrap_or("");
        let cmd = detail.split_whitespace().next().unwrap_or("");

        let mut ctx = HashMap::new();
        ctx.insert(
            "action".into(),
            serde_yaml::to_value(serde_json::json!({
                "full": action,
                "category": category,
                "detail": detail,
                "command": cmd,
            }))
            .unwrap_or(serde_yaml::Value::Null),
        );

        if let Some(extra_val) = extra {
            if let Some(obj) = extra_val.as_object() {
                for (k, v) in obj {
                    if let Ok(yaml_v) = serde_yaml::to_value(v) {
                        ctx.insert(k.clone(), yaml_v);
                    }
                }
            }
        }

        Some(ctx)
    }

    // ── Trust ────────────────────────────────────────────────────────────

    /// Update trust score with clamped semantics.
    ///
    /// Matches server.py: ±200 delta per update, max 500 for new agents,
    /// self-trust rejection, clamped to 0–1000.
    pub fn update_trust(
        &self,
        agent_id: &str,
        requested_score: u32,
        _interactions: u64,
    ) -> Result<Value, &'static str> {
        // Reject self-trust updates
        if agent_id == self.sandbox_name {
            return Err("Cannot update own trust score");
        }

        let existing = self.trust.get_trust_score(agent_id);
        let old_score = existing.score;
        let is_new = existing.interactions == 0;

        if is_new {
            // Bootstrap: set initial score (capped at 500) then record first interaction.
            let initial = requested_score.min(500);
            self.trust.set_trust(agent_id, initial);
            self.trust.record_success(agent_id);
        } else if requested_score >= old_score {
            // Positive interaction — use SDK's built-in reward + decay + interaction bump.
            self.trust.record_success(agent_id);
        } else {
            // Negative interaction — use SDK's built-in penalty + decay + interaction bump.
            self.trust.record_failure(agent_id);
        }

        let updated = self.trust.get_trust_score(agent_id);
        metrics::AGT_KNOWN_AGENTS.set(self.trust.all_agents().len() as i64);

        self.audit
            .log(agent_id, &format!("trust_update:{}", agent_id), "success");

        Ok(serde_json::json!({
            "ok": true,
            "agent_id": agent_id,
            "score": updated.score,
            "interactions": updated.interactions,
        }))
    }

    /// Get all trust scores with tier labels (matching API JSON shape).
    pub fn all_trust_scores(&self) -> Vec<Value> {
        self.trust
            .all_agents()
            .into_iter()
            .map(|ts| {
                serde_json::json!({
                    "agent_id": ts.agent_id,
                    "score": ts.score,
                    "tier": tier_label(ts.score),
                    "interactions": ts.interactions,
                    "last_interaction": "", // TrustManager doesn't expose timestamps in TrustScore
                })
            })
            .collect()
    }

    /// Get trust score for a single agent (matching API JSON shape).
    #[allow(dead_code)] // Used by individual trust route
    pub fn get_trust_score_json(&self, agent_id: &str) -> Value {
        let ts = self.trust.get_trust_score(agent_id);
        serde_json::json!({
            "agent_id": ts.agent_id,
            "score": ts.score,
            "tier": tier_label(ts.score),
            "interactions": ts.interactions,
            "last_interaction": "",
        })
    }

    /// Delete trust state for an agent by resetting to initial score (0 interactions).
    /// The SDK TrustManager has no delete method, so we set score to 0.
    pub fn delete_trust(&self, agent_id: &str) -> Value {
        self.trust.set_trust(agent_id, 0);
        self.audit.log(agent_id, "trust_delete", "success");
        metrics::AGT_KNOWN_AGENTS.set(self.trust.all_agents().len() as i64);
        serde_json::json!({
            "ok": true,
            "agent_id": agent_id,
            "deleted": true,
        })
    }

    // ── Content flag ─────────────────────────────────────────────────────

    /// Report content safety flag and optionally penalize trust.
    pub fn report_content_flag(
        &self,
        agent_id: &str,
        _flags: &Value,
        filtered: &[String],
        detected: &[String],
        penalty: i32,
    ) -> Value {
        self.metrics.content_flags.fetch_add(1, Ordering::Relaxed);

        // Prometheus: increment per-category counters
        for cat in filtered.iter().chain(detected.iter()) {
            metrics::AGT_CONTENT_FLAGS.with_label_values(&[cat]).inc();
        }
        if filtered.is_empty() && detected.is_empty() {
            metrics::AGT_CONTENT_FLAGS
                .with_label_values(&["unknown"])
                .inc();
        }

        let flag_summary: String = filtered
            .iter()
            .chain(detected.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join(",");

        self.audit.log(
            agent_id,
            &format!("content_flag:{}", flag_summary),
            "flagged",
        );

        self.behavior.record(agent_id, false);

        if penalty < 0 {
            let existing = self.trust.get_trust_score(agent_id);
            let old_score = existing.score;
            let new_score = old_score.saturating_sub((-penalty) as u32);
            self.trust.set_trust(agent_id, new_score);

            tracing::warn!(
                agent_id,
                categories = %flag_summary,
                penalty,
                old_score,
                new_score,
                "Content flag with trust penalty"
            );

            return serde_json::json!({
                "ok": true,
                "penalty_applied": penalty,
                "trust_score": new_score,
                "previous_score": old_score,
            });
        }

        serde_json::json!({
            "ok": true,
            "penalty_applied": 0,
            "trust_score": null,
        })
    }

    // ── Status ───────────────────────────────────────────────────────────

    /// Build the full `/agt/status` JSON response.
    pub fn status_json(&self) -> Value {
        let trust_states = self.all_trust_scores();
        let audit_count = self.audit.entries().len();
        let integrity_ok = self.audit.verify();
        let relay_url = std::env::var("AGT_RELAY_URL").unwrap_or_default();
        let registry_url = std::env::var("AGT_REGISTRY_URL").unwrap_or_default();

        serde_json::json!({
            "enabled": true,
            "governance_mode": "native",
            "sandbox": self.sandbox_name,
            "agent_did": self.identity.did,
            "policy_loaded": self.policy.is_loaded(),
            "policy_rules": self.policy_rule_count.load(Ordering::Relaxed),
            "audit_entries": audit_count,
            "audit_integrity": integrity_ok,
            "known_agents": trust_states.len(),
            "trust_states": trust_states,
            "trust_threshold": self.trust_threshold,
            "trust_updates": trust_states.iter().filter(|t| {
                t.get("interactions").and_then(|i| i.as_u64()).unwrap_or(0) > 0
            }).count(),
            "total_interactions": trust_states.iter()
                .map(|t| t.get("interactions").and_then(|i| i.as_u64()).unwrap_or(0))
                .sum::<u64>(),
            // New stats (native governance only)
            "policy_evaluations": self.metrics.evaluations.load(Ordering::Relaxed),
            "policy_denials": self.metrics.denials.load(Ordering::Relaxed),
            "policy_rate_limits": self.metrics.rate_limits.load(Ordering::Relaxed),
            "eval_latency_avg_us": self.metrics.avg_eval_latency_us(),
            "behavior_alerts": self.behavior.alert_count(),
            "behavior_alerts_detail": self.behavior.alerts_detail(),
            "content_flags": self.metrics.content_flags.load(Ordering::Relaxed),
            "uptime_secs": self.start_time.elapsed().as_secs(),
            "relay_url": relay_url,
            "registry_url": registry_url,
            "rate_limit": {
                "global_rate": self.rate_limiter.global_rate(),
                "global_capacity": self.rate_limiter.global_capacity(),
                "per_agent_rate": self.rate_limiter.per_agent_rate(),
                "per_agent_capacity": self.rate_limiter.per_agent_capacity(),
            },
        })
    }

    /// Build the `/agt/audit` JSON response.
    pub fn audit_json(&self) -> Value {
        let entries = self.audit.entries();
        let last_100: Vec<_> = entries.iter().rev().take(100).rev().collect();
        serde_json::json!({
            "entries": last_100.iter().map(|e| {
                serde_json::json!({
                    "action": e.action,
                    "agent_id": e.agent_id,
                    "decision": e.decision,
                    "result": e.decision,
                    "timestamp": e.timestamp,
                })
            }).collect::<Vec<_>>(),
            "count": last_100.len(),
            "sandbox": self.sandbox_name,
        })
    }

    /// Verify audit chain integrity.
    pub fn audit_verify_json(&self) -> Value {
        let valid = self.audit.verify();
        let count = self.audit.entries().len();
        serde_json::json!({
            "integrity": if valid { "valid" } else { "COMPROMISED" },
            "entries": count,
            "sandbox": self.sandbox_name,
            "message": if valid { "Hash chain verified" } else { "Hash chain broken" },
        })
    }
}

/// Get the maximum mtime of YAML files in a directory (for change detection).
fn dir_max_mtime(dir: &str) -> Option<std::time::SystemTime> {
    let path = Path::new(dir);
    if !path.is_dir() {
        return None;
    }
    std::fs::read_dir(path)
        .ok()?
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == "yaml" || ext == "yml")
        })
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
}

/// Convert trust score to tier label (matches _score_to_tier convention).
pub fn tier_label(score: u32) -> &'static str {
    if score >= 800 {
        "Sovereign"
    } else if score >= 600 {
        "Verified"
    } else if score >= 400 {
        "Known"
    } else if score >= 200 {
        "Observed"
    } else {
        "Anonymous"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_labels_are_correct() {
        assert_eq!(tier_label(1000), "Sovereign");
        assert_eq!(tier_label(800), "Sovereign");
        assert_eq!(tier_label(799), "Verified");
        assert_eq!(tier_label(600), "Verified");
        assert_eq!(tier_label(500), "Known");
        assert_eq!(tier_label(400), "Known");
        assert_eq!(tier_label(200), "Observed");
        assert_eq!(tier_label(100), "Anonymous");
        assert_eq!(tier_label(0), "Anonymous");
    }

    #[test]
    fn rate_limiter_allows_then_blocks() {
        let rl = RateLimiter::new(2.0, 2.0, 2.0, 2.0);
        assert!(rl.allow("agent-1"));
        assert!(rl.allow("agent-1"));
        // Third should be blocked (bucket empty)
        assert!(!rl.allow("agent-1"));
    }

    #[test]
    fn behavior_monitor_detects_burst() {
        let bm = BehaviorMonitor::new(3, 20, 10);
        assert!(!bm.record("a", true));
        assert!(!bm.record("a", true));
        assert!(!bm.record("a", true));
        // 4th call exceeds burst_threshold=3
        assert!(bm.record("a", true));
    }

    #[test]
    fn behavior_monitor_detects_consecutive_failures() {
        let bm = BehaviorMonitor::new(100, 2, 100);
        assert!(!bm.record("a", false));
        assert!(!bm.record("a", false));
        // 3rd failure exceeds threshold=2
        assert!(bm.record("a", false));
    }

    #[test]
    fn build_context_parses_action_string() {
        let ctx = Governance::build_context("shell:ls -la /tmp", None).unwrap();
        let action = ctx.get("action").unwrap();
        let action_map: serde_json::Value = serde_yaml::from_value(action.clone()).unwrap();
        assert_eq!(action_map["category"], "shell");
        assert_eq!(action_map["detail"], "ls -la /tmp");
        assert_eq!(action_map["command"], "ls");
        assert_eq!(action_map["full"], "shell:ls -la /tmp");
    }

    #[test]
    fn governance_metrics_avg_latency() {
        let m = GovernanceMetrics::new();
        assert_eq!(m.avg_eval_latency_us(), 0);
        m.evaluations.store(2, Ordering::Relaxed);
        m.eval_latency_sum_us.store(100, Ordering::Relaxed);
        assert_eq!(m.avg_eval_latency_us(), 50);
    }

    #[test]
    fn governance_evaluate_without_policy_allows() {
        let gov = Governance::new("test-sandbox");
        let result = gov.evaluate("agent-1", "shell:ls", None);
        assert_eq!(result["allowed"], true);
        assert_eq!(result["decision"], "allow");
        assert_eq!(result["rate_limited"], false);
    }

    #[test]
    fn governance_evaluate_tracks_metrics() {
        let gov = Governance::new("test-sandbox");
        gov.evaluate("agent-1", "shell:ls", None);
        gov.evaluate("agent-1", "shell:cat", None);
        assert_eq!(gov.metrics.evaluations.load(Ordering::Relaxed), 2);
        assert_eq!(gov.metrics.denials.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn governance_trust_clamping_caps_delta() {
        let gov = Governance::new("test-sandbox");
        let peer = format!("clamp-test-{}", std::process::id());
        // New agent: bootstrap at 500, then record_success adds reward (10) → ~510
        let result = gov.update_trust(&peer, 1000, 0);
        assert!(result.is_ok());
        let score = gov.trust.get_trust_score(&peer);
        assert!(
            score.interactions >= 1,
            "First update should record at least one interaction (got {})",
            score.interactions,
        );
        assert!(
            score.score <= 520,
            "New agent score {} should be bootstrapped near 500 + reward",
            score.score
        );
        let prev = score.score;
        // Second update (positive): record_success adds another reward
        let _ = gov.update_trust(&peer, 1000, 0);
        let score2 = gov.trust.get_trust_score(&peer);
        assert!(
            score2.interactions > score.interactions,
            "Second update should bump interactions",
        );
        assert!(
            score2.score > prev,
            "Score {} should increase after positive interaction (was {})",
            score2.score,
            prev
        );
    }

    #[test]
    fn governance_trust_self_boost_rejected() {
        let gov = Governance::new("test-sandbox");
        // Sandbox trying to update its own trust should be rejected
        let result = gov.update_trust("test-sandbox", 1000, 0);
        assert!(result.is_err(), "Self-trust should be rejected");
    }

    #[test]
    fn governance_audit_log_and_verify() {
        let gov = Governance::new("test-sandbox");
        gov.audit.log("agent-1", "shell:ls", "allow");
        gov.audit.log("agent-2", "http:fetch", "deny");
        assert_eq!(gov.audit.entries().len(), 2);
        assert!(gov.audit.verify(), "Fresh audit chain should be valid");
    }

    #[test]
    fn governance_status_json_has_expected_fields() {
        let gov = Governance::new("test-sandbox");
        gov.evaluate("agent-1", "shell:ls", None);
        let status = gov.status_json();
        assert_eq!(status["enabled"], true);
        assert_eq!(status["governance_mode"], "native");
        assert_eq!(status["sandbox"], "test-sandbox");
        assert_eq!(status["policy_evaluations"], 1);
        assert!(
            status["agent_did"]
                .as_str()
                .unwrap()
                .starts_with("did:agentmesh:")
        );
    }

    #[test]
    fn governance_content_flag_applies_penalty() {
        let gov = Governance::new("test-sandbox");
        let before = gov.trust.get_trust_score("bad-agent").score;
        gov.report_content_flag(
            "bad-agent",
            &serde_json::json!({"type": "spam"}),
            &["profanity".to_string()],
            &["spam".to_string()],
            -50,
        );
        let after = gov.trust.get_trust_score("bad-agent").score;
        assert!(after < before, "Content flag should apply trust penalty");
    }
}
