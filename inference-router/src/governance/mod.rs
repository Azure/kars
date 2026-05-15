// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native AGT governance — in-process policy evaluation, trust management,
//! audit logging, and agent identity.
//!
//! Uses the `agentmesh` crate (v3.1.0) for core primitives + MCP modules.
//! The `/agt/*` route handlers call these functions directly instead of
//! forwarding HTTP to a separate service.

use agentmesh::AuditLogger;
use agentmesh::identity::AgentIdentity;
use agentmesh::mcp::rate_limit::{InMemoryRateLimitStore, McpSlidingRateLimiter};
use agentmesh::mcp::redactor::{CredentialKind, CredentialRedactor};
use agentmesh::mcp::response::{McpResponseScanner, McpResponseThreatType};
use agentmesh::mcp::{InMemoryAuditSink, McpMetricsCollector, SystemClock};
use agentmesh::policy::PolicyEngine;
use agentmesh::trust::{TrustConfig, TrustManager};
use agentmesh::types::PolicyDecision;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::behavior_monitor::BehaviorMonitor;
use crate::metrics;
use crate::policy_status::{PolicyKind, PolicyStatusRegistry};
use crate::rate_limiter::RateLimiter;

mod trust_ops;

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
    pub redactions: AtomicU64,
    pub response_threats: AtomicU64,
    pub tool_rate_limits: AtomicU64,
    pub messages_signed: AtomicU64,
    pub messages_verified: AtomicU64,
    pub signatures_rejected: AtomicU64,
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
            redactions: AtomicU64::new(0),
            response_threats: AtomicU64::new(0),
            tool_rate_limits: AtomicU64::new(0),
            messages_signed: AtomicU64::new(0),
            messages_verified: AtomicU64::new(0),
            signatures_rejected: AtomicU64::new(0),
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

// ── Main governance engine ───────────────────────────────────────────────────

/// Native governance engine — single struct that owns all AGT components. Thread-safe, shared via `Arc<Governance>`.
pub struct Governance {
    pub identity: AgentIdentity,
    pub policy: PolicyEngine,
    pub trust: TrustManager,
    pub audit: AuditLogger,
    pub(crate) audit_dedup: crate::providers::audit_impl::AuditDedup,
    /// Sandbox-wide audit sink (Slice 4 DoD #4, #5). Composite over the
    /// always-on local JSONL writer (Slice 4a) and any configured remote
    /// sinks (Slice 4c — Azure Monitor today). `None` when neither
    /// local nor remote sinks could be initialized — the in-memory
    /// `audit` chain remains authoritative.
    pub(crate) audit_sink: Option<std::sync::Arc<dyn crate::audit_sink::AuditSink>>,
    pub redactor: CredentialRedactor,
    pub response_scanner: McpResponseScanner,
    pub tool_rate_limiter: McpSlidingRateLimiter,
    pub rate_limiter: RateLimiter,
    pub behavior: BehaviorMonitor,
    pub metrics: GovernanceMetrics,
    pub sandbox_name: String,
    pub trust_threshold: u32,
    /// Per-peer last-interaction timestamps (SDK's TrustScore doesn't expose
    /// this, so we track it alongside). Used by the operator UX to render
    /// recency of mesh peers — without it, peers whose name doesn't match
    /// a known sandbox CR (e.g. cloud-offload parents identified only by AMID) are filtered out of the peer panel.
    peer_last_seen: std::sync::Mutex<std::collections::HashMap<String, std::time::SystemTime>>,
    /// Read-only TrustGraph projection consulted by the bootstrap path
    /// in `update_trust` to seed AGT scores for brand-new peers.
    /// Always present; an absent / unconfigured projection is
    /// represented by [`crate::a2a::trust_graph_projection::TrustGraphProjection::empty`].
    pub(crate) trust_graph: crate::a2a::trust_graph_projection::TrustGraphProjection,
    start_time: Instant,
    policy_rule_count: AtomicU64,
    /// Per-CRD policy load status. Slice 1 of `crd-well-oiled-machine`:
    /// the AGT engine writes a `PolicyKind::AgtProfile` row here on
    /// every successful `load_policies_from_dir`. `GET /internal/policy-status`
    /// reads it; future slices add their own kinds. Shared via
    /// `Arc<PolicyStatusRegistry>` with `AppState`.
    pub policy_status: std::sync::Arc<PolicyStatusRegistry>,
}

impl Governance {
    /// Initialize governance from environment variables.
    pub fn new(sandbox_name: &str) -> Self {
        Self::new_with_status(
            sandbox_name,
            std::sync::Arc::new(PolicyStatusRegistry::new()),
        )
    }

    /// Initialize governance with a shared [`PolicyStatusRegistry`].
    /// Production callers use the default `new()` helper; tests and
    /// `AppState::new` use this variant to share one registry instance
    /// with the HTTP layer.
    pub fn new_with_status(
        sandbox_name: &str,
        policy_status: std::sync::Arc<PolicyStatusRegistry>,
    ) -> Self {
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

        // AGT MCP modules (from agentmesh 3.1.0)
        let redactor = CredentialRedactor::new().unwrap_or_else(|e| {
            tracing::warn!("CredentialRedactor init failed ({e}), using fallback");
            CredentialRedactor::new().expect("redactor must initialize")
        });
        let mcp_clock: std::sync::Arc<dyn agentmesh::mcp::Clock> = std::sync::Arc::new(SystemClock);
        let mcp_audit: std::sync::Arc<dyn agentmesh::mcp::McpAuditSink> = std::sync::Arc::new(
            InMemoryAuditSink::new(CredentialRedactor::new().expect("redactor for audit sink")),
        );
        let mcp_metrics = McpMetricsCollector::default();
        let response_scanner = McpResponseScanner::new(
            CredentialRedactor::new().expect("redactor for scanner"),
            mcp_audit,
            mcp_metrics,
            mcp_clock.clone(),
        )
        .expect("McpResponseScanner must initialize");

        // Per-tool sliding window: configurable via env vars
        let tool_max = std::env::var("TOOL_RATE_LIMIT_MAX")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100usize);
        let tool_window_secs = std::env::var("TOOL_RATE_LIMIT_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60u64);
        let tool_rate_limiter = McpSlidingRateLimiter::new(
            tool_max,
            Duration::from_secs(tool_window_secs),
            mcp_clock,
            std::sync::Arc::new(InMemoryRateLimitStore::default()),
        )
        .expect("McpSlidingRateLimiter must initialize");

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
            audit_dedup: crate::providers::audit_impl::AuditDedup::new(),
            audit_sink: crate::audit_sink::build_sink_from_env(
                sandbox_name,
                std::sync::Arc::new(crate::auth::WorkloadIdentityAuth::new()),
            ),
            redactor,
            response_scanner,
            tool_rate_limiter,
            rate_limiter: RateLimiter::new(500.0, 1000.0, 50.0, 100.0),
            behavior: BehaviorMonitor::new(
                // Behavior-monitor thresholds. Defaults tuned for interactive
                // AzureClaw sandboxes; offload workers (research loops with
                // many tool/inference calls in short bursts) get higher
                // limits injected via env vars by the controller reconciler.
                std::env::var("AGT_BEHAVIOR_BURST_THRESHOLD")
                    .ok()
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(100),
                std::env::var("AGT_BEHAVIOR_CONSECUTIVE_FAILURE_THRESHOLD")
                    .ok()
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(20),
                std::env::var("AGT_BEHAVIOR_COOLDOWN_SECS")
                    .ok()
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(10),
            ),
            metrics: GovernanceMetrics::new(),
            sandbox_name: sandbox_name.to_string(),
            trust_threshold,
            policy_rule_count,
            peer_last_seen: std::sync::Mutex::new(std::collections::HashMap::new()),
            trust_graph: {
                let p = crate::a2a::trust_graph_loader::load_or_empty();
                if !p.version_hash().is_empty() {
                    metrics::AGT_TRUSTGRAPH_PROJECTION_VERSION
                        .with_label_values(&[p.version_hash()])
                        .set(1);
                }
                p
            },
            start_time: Instant::now(),
            policy_status,
        }
    }

    /// Test-only: replace the trust-graph projection. Lives behind
    /// `#[cfg(test)]` so the field stays effectively private in
    /// production — bootstrap data flows through the file-mount
    /// loader only.
    #[cfg(test)]
    pub(crate) fn set_trust_graph_for_test(
        &mut self,
        projection: crate::a2a::trust_graph_projection::TrustGraphProjection,
    ) {
        self.trust_graph = projection;
    }

    /// Append one entry to the in-memory hash chain **and** mirror it to
    /// the sandbox-local JSONL writer (Slice 4 DoD #4). Every production
    /// callsite goes through this helper so audit rows persist across
    /// router restart. The chain entry is returned for callers that need
    /// the receipt (e.g. [`crate::providers::audit_impl`]).
    ///
    /// JSONL failures are logged at `warn` and discarded — the in-memory
    /// chain is authoritative for the lifetime of the process, and the
    /// audited request must not be denied because the disk filled up.
    pub fn audit_log(&self, agent_id: &str, action: &str, decision: &str) -> agentmesh::AuditEntry {
        let entry = self.audit.log(agent_id, action, decision);
        if let Some(sink) = &self.audit_sink {
            sink.write(&entry);
        }
        entry
    }

    /// Load all YAML policy files from a directory.  Returns rule count.
    pub fn load_policies_from_dir(&self, dir: &str) -> Result<usize, String> {
        let path = Path::new(dir);
        if !path.is_dir() {
            tracing::debug!(
                dir,
                "Policy directory not found — starting with empty policy"
            );
            // No directory → no profile. Record `None` so the controller
            // can distinguish "no profile mounted yet" from "profile
            // loaded with digest X".
            self.policy_status.record_error(
                PolicyKind::AgtProfile,
                dir,
                "policy directory not found",
            );
            return Ok(0);
        }

        let mut total_rules = 0;
        // Collect files in deterministic order so the aggregate digest
        // is independent of filesystem listing order. Without sorting,
        // two routers seeing the same files could echo different
        // digests, defeating the controller's match check.
        let mut yaml_files: Vec<std::path::PathBuf> = Vec::new();
        let mut read_err: Option<String> = None;
        match std::fs::read_dir(path) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().is_some_and(|e| e == "yaml" || e == "yml") {
                        yaml_files.push(p);
                    }
                }
                yaml_files.sort();
            }
            Err(e) => {
                // Surface silent permission/IO failures — without this, a
                // hot-reload that hits EACCES looks indistinguishable from a
                // legitimately-empty policy directory and silently disables
                // governance enforcement.
                tracing::warn!(dir, error = %e, "Policy reload: read_dir failed (perms?)");
                read_err = Some(format!("read_dir failed: {e}"));
            }
        }

        // Aggregate canonical bytes: per-file, `len(name)\0name\0len(bytes)\0bytes\0`.
        // Length-prefixing prevents collisions like `("ab", "c")` vs
        // `("a", "bc")` producing the same hash if we just concatenated.
        let mut canonical: Vec<u8> = Vec::new();
        for p in &yaml_files {
            match std::fs::read(p) {
                Ok(bytes) => {
                    let name = p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default()
                        .as_bytes();
                    canonical.extend_from_slice(&(name.len() as u64).to_be_bytes());
                    canonical.extend_from_slice(name);
                    canonical.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
                    canonical.extend_from_slice(&bytes);
                    match self.policy.load_from_file(p.to_str().unwrap_or_default()) {
                        Ok(()) => {
                            let rules_in_file = Self::count_rules_in_file(p);
                            tracing::info!(file = %p.display(), rules = rules_in_file, "Loaded policy file");
                            total_rules += rules_in_file;
                        }
                        Err(e) => {
                            tracing::warn!(file = %p.display(), error = %e, "Failed to load policy");
                            // Stash the first per-file load failure as
                            // the registry error — operator UX cue.
                            if read_err.is_none() {
                                read_err = Some(format!("{}: {}", p.display(), e));
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(file = %p.display(), error = %e, "Failed to read policy file");
                    if read_err.is_none() {
                        read_err = Some(format!("{}: {}", p.display(), e));
                    }
                }
            }
        }

        self.policy_rule_count
            .store(total_rules as u64, Ordering::Relaxed);
        metrics::AGT_POLICY_RULES.set(total_rules as i64);

        // Update the status registry. Empty directory + no read error
        // ⇒ record success with the digest of an empty buffer — that's
        // a real "I loaded nothing, on purpose" state, distinct from
        // "I couldn't read the directory".
        match read_err {
            Some(err) => {
                self.policy_status
                    .record_error(PolicyKind::AgtProfile, dir, &err);
            }
            None => {
                self.policy_status
                    .record_success(PolicyKind::AgtProfile, dir, &canonical);
            }
        }
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
        let dir =
            std::env::var("AGT_POLICY_DIR").unwrap_or_else(|_| "/etc/azureclaw/policies".into());
        match self.load_policies_from_dir(&dir) {
            Ok(count) => tracing::info!(rules = count, "Policy hot-reloaded"),
            Err(e) => tracing::warn!(error = %e, "Policy hot-reload failed"),
        }
    }

    /// Spawn a background task that watches AGT_POLICY_DIR for changes
    /// and reloads policies when file mtimes change.
    pub fn spawn_policy_watcher(governance: std::sync::Arc<Self>) {
        let dir =
            std::env::var("AGT_POLICY_DIR").unwrap_or_else(|_| "/etc/azureclaw/policies".into());
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
            self.audit_log(agent_id, action, "denied");
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
        self.audit_log(agent_id, action, outcome);

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

    // ── Credential Redaction (AGT CredentialRedactor) ────────────────────

    /// Redact credentials from text. Tracks metrics and audit.
    pub fn redact_text(&self, text: &str) -> String {
        let result = self.redactor.redact(text);
        if !result.detected.is_empty() {
            self.metrics.redactions.fetch_add(1, Ordering::Relaxed);
            let kinds: Vec<&str> = result
                .detected
                .iter()
                .map(|k: &CredentialKind| k.as_str())
                .collect();
            for kind in &kinds {
                metrics::AGT_REDACTIONS.with_label_values(&[kind]).inc();
            }
            tracing::warn!(
                sandbox = %self.sandbox_name,
                kinds = ?kinds,
                "Credential redacted from output"
            );
            self.audit_log(
                &self.sandbox_name,
                &format!("credential_redacted:{}", kinds.join(",")),
                "sanitized",
            );
        }
        result.sanitized
    }

    // ── Response Scanning (AGT McpResponseScanner) ──────────────────────

    /// Scan model response text for threats (prompt injection, exfil URLs, etc.).
    /// Returns the sanitized text and whether any threats were found.
    ///
    /// When `AZURECLAW_SUPPRESS_EXFIL_URL=1` is set, the upstream AGT
    /// `ExfiltrationUrl` finding is downgraded: the threat is still recorded
    /// (metrics + warn log + audit), but URLs are NOT redacted from the model
    /// response. Rationale: AGT 3.1.0 / 3.3.0 use a naive substring guard
    /// (`text.contains("post"|"send"|"upload"|...)`) which over-fires on
    /// long-form content (e.g. "security posture", "post-mortem") and strips
    /// legitimate citations. Other threat types (prompt-injection tags,
    /// imperative phrasing, credential leakage) remain fully enforced.
    pub fn scan_response(&self, text: &str) -> (String, bool) {
        match self.response_scanner.scan_text(text) {
            Ok(result) => {
                if !result.findings.is_empty() {
                    self.metrics
                        .response_threats
                        .fetch_add(result.findings.len() as u64, Ordering::Relaxed);
                    for finding in &result.findings {
                        let label = match finding.threat_type {
                            McpResponseThreatType::PromptInjectionTag => "prompt_injection",
                            McpResponseThreatType::ImperativePhrasing => "imperative_phrasing",
                            McpResponseThreatType::CredentialLeakage => "credential_leakage",
                            McpResponseThreatType::ExfiltrationUrl => "exfiltration_url",
                        };
                        metrics::AGT_RESPONSE_THREATS
                            .with_label_values(&[label])
                            .inc();
                    }
                    let types: Vec<&str> = result
                        .findings
                        .iter()
                        .map(|f| match f.threat_type {
                            McpResponseThreatType::PromptInjectionTag => "prompt_injection",
                            McpResponseThreatType::ImperativePhrasing => "imperative_phrasing",
                            McpResponseThreatType::CredentialLeakage => "credential_leakage",
                            McpResponseThreatType::ExfiltrationUrl => "exfiltration_url",
                        })
                        .collect();
                    tracing::warn!(
                        sandbox = %self.sandbox_name,
                        threats = ?types,
                        "Response threats detected"
                    );
                    self.audit_log(
                        &self.sandbox_name,
                        &format!("response_threat:{}", types.join(",")),
                        "flagged",
                    );

                    let suppress_exfil = std::env::var("AZURECLAW_SUPPRESS_EXFIL_URL")
                        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                        .unwrap_or(false);
                    if suppress_exfil {
                        let only_exfil = result.findings.iter().all(|f| {
                            matches!(f.threat_type, McpResponseThreatType::ExfiltrationUrl)
                        });
                        if only_exfil {
                            tracing::warn!(
                                sandbox = %self.sandbox_name,
                                "exfiltration_url redaction suppressed (env override); citations retained",
                            );
                            return (text.to_string(), false);
                        }
                    }
                }
                (result.sanitized, !result.findings.is_empty())
            }
            Err(e) => {
                tracing::warn!(sandbox = %self.sandbox_name, "Response scan error: {e}");
                (text.to_string(), false)
            }
        }
    }

    // ── Per-Tool Rate Limiting (AGT McpSlidingRateLimiter) ──────────────

    /// Check per-tool rate limit. Returns (allowed, retry_after_secs).
    pub fn check_tool_rate(&self, tool: &str) -> (bool, u64) {
        match self.tool_rate_limiter.check(tool) {
            Ok(decision) => {
                if !decision.allowed {
                    self.metrics
                        .tool_rate_limits
                        .fetch_add(1, Ordering::Relaxed);
                    metrics::AGT_TOOL_RATE_LIMITS
                        .with_label_values(&[tool])
                        .inc();
                    tracing::warn!(
                        sandbox = %self.sandbox_name,
                        tool,
                        retry_after = decision.retry_after_secs,
                        "Per-tool rate limit exceeded"
                    );
                    self.audit_log(
                        &self.sandbox_name,
                        &format!("tool_rate_limited:{tool}"),
                        "denied",
                    );
                }
                (decision.allowed, decision.retry_after_secs)
            }
            Err(e) => {
                tracing::warn!(sandbox = %self.sandbox_name, "Tool rate check error: {e}");
                (true, 0) // fail-open
            }
        }
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
            "redactions_total": self.metrics.redactions.load(Ordering::Relaxed),
            "response_threats_total": self.metrics.response_threats.load(Ordering::Relaxed),
            "tool_rate_limits_total": self.metrics.tool_rate_limits.load(Ordering::Relaxed),
            "messages_signed": self.metrics.messages_signed.load(Ordering::Relaxed),
            "messages_verified": self.metrics.messages_verified.load(Ordering::Relaxed),
            "signatures_rejected": self.metrics.signatures_rejected.load(Ordering::Relaxed),
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

// Audit-sink construction lives in `crate::audit_sink::build_sink_from_env`.
// Slice 4c moved the helper out of this module so the local JSONL writer
// and the Azure Monitor remote sink can share configuration logic.

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

    #[test]
    fn trustgraph_bootstrap_overrides_requested_for_new_peer() {
        const PK_A: &str = "0EqyMnQrtKs6E2i9RhXk5tAiSrcaAWuvhSCjMsl3hzc";
        const PK_B: &str = "oJql9HpnWYAv-VX43C0qFKXJnSO-l_hkEn_5ODRVpPA";
        const SIG_AB: &str = "5-Esp-uk11u_cGsCRdUXwxbxtCzSQsNaEzIBeFUBPZFWz9cUtr9PBw6eWFIBAAprz9kGwH2wTk3xaSnbJVQzAw";

        let alpha = format!("alpha-tg-bs-{}", std::process::id());
        let beta = format!("beta-tg-bs-{}", std::process::id());
        let gamma = format!("gamma-tg-bs-{}", std::process::id());

        let raw = format!(
            r#"{{
              "vertices": [
                {{"id": "{alpha}", "alg": "EdDSA", "publicKeyB64u": "{PK_A}"}},
                {{"id": "{beta}",  "alg": "EdDSA", "publicKeyB64u": "{PK_B}"}}
              ],
              "edges": [
                {{"from": "{alpha}", "to": "{beta}", "score": 750,
                  "issuedAt": 1700000000, "signature": "{SIG_AB}"}}
              ],
              "versionHash": "abc123def4567890",
              "inputEdgeCount": 1
            }}"#
        );
        let projection =
            crate::a2a::trust_graph_projection::TrustGraphProjection::from_json(&raw).unwrap();

        let mut gov = Governance::new(&alpha);
        gov.set_trust_graph_for_test(projection);

        let res = gov.update_trust(&beta, 0, 0);
        assert!(res.is_ok());
        let score = gov.trust.get_trust_score(&beta).score;
        assert!(
            score >= 500,
            "expected bootstrap-from-edge >= 500, got {score}"
        );

        let _ = gov.update_trust(&gamma, 0, 0);
        let no_edge_score = gov.trust.get_trust_score(&gamma).score;
        assert!(
            no_edge_score < score,
            "no-edge peer ({no_edge_score}) must not exceed edge-bootstrapped peer ({score})"
        );
    }

    #[test]
    fn trustgraph_does_not_override_existing_peer_score() {
        const PK_A: &str = "0EqyMnQrtKs6E2i9RhXk5tAiSrcaAWuvhSCjMsl3hzc";
        const PK_B: &str = "oJql9HpnWYAv-VX43C0qFKXJnSO-l_hkEn_5ODRVpPA";
        const SIG_AB: &str = "5-Esp-uk11u_cGsCRdUXwxbxtCzSQsNaEzIBeFUBPZFWz9cUtr9PBw6eWFIBAAprz9kGwH2wTk3xaSnbJVQzAw";

        let alpha = format!("alpha-tg-stable-{}", std::process::id());
        let beta = format!("beta-tg-stable-{}", std::process::id());

        let raw = format!(
            r#"{{
              "vertices": [
                {{"id": "{alpha}", "alg": "EdDSA", "publicKeyB64u": "{PK_A}"}},
                {{"id": "{beta}",  "alg": "EdDSA", "publicKeyB64u": "{PK_B}"}}
              ],
              "edges": [
                {{"from": "{alpha}", "to": "{beta}", "score": 750,
                  "issuedAt": 1700000000, "signature": "{SIG_AB}"}}
              ],
              "versionHash": "abc123def4567890",
              "inputEdgeCount": 1
            }}"#
        );
        let projection =
            crate::a2a::trust_graph_projection::TrustGraphProjection::from_json(&raw).unwrap();

        let mut gov = Governance::new(&alpha);
        gov.set_trust_graph_for_test(projection);

        let _ = gov.update_trust(&beta, 0, 0);
        let after_bootstrap = gov.trust.get_trust_score(&beta).score;

        let _ = gov.update_trust(&beta, 0, 0);
        let after_negative = gov.trust.get_trust_score(&beta).score;
        assert!(
            after_negative <= after_bootstrap,
            "negative interaction must not be re-bootstrapped (was {after_bootstrap}, now {after_negative})"
        );
    }

    #[test]
    fn trustgraph_no_projection_is_no_op() {
        let sandbox = format!("standalone-tg-{}", std::process::id());
        let peer = format!("peer-tg-noop-{}", std::process::id());
        let gov = Governance::new(&sandbox);
        assert!(gov.trust_graph.is_empty());

        let _ = gov.update_trust(&peer, 200, 0);
        let s = gov.trust.get_trust_score(&peer).score;
        assert!(
            (200..=220).contains(&s),
            "expected bootstrap near 200 with no projection, got {s}"
        );
    }

    #[test]
    fn trustgraph_self_edge_does_not_bootstrap() {
        const PK_A: &str = "0EqyMnQrtKs6E2i9RhXk5tAiSrcaAWuvhSCjMsl3hzc";
        const SIG_AB: &str = "5-Esp-uk11u_cGsCRdUXwxbxtCzSQsNaEzIBeFUBPZFWz9cUtr9PBw6eWFIBAAprz9kGwH2wTk3xaSnbJVQzAw";

        let alpha = format!("alpha-tg-self-{}", std::process::id());
        let raw = format!(
            r#"{{
              "vertices": [{{"id": "{alpha}", "alg": "EdDSA", "publicKeyB64u": "{PK_A}"}}],
              "edges": [{{"from": "{alpha}", "to": "{alpha}", "score": 1000,
                          "issuedAt": 1700000000, "signature": "{SIG_AB}"}}],
              "versionHash": "0000000000000000",
              "inputEdgeCount": 1
            }}"#
        );
        let projection =
            crate::a2a::trust_graph_projection::TrustGraphProjection::from_json(&raw).unwrap();
        let mut gov = Governance::new(&alpha);
        gov.set_trust_graph_for_test(projection);

        let res = gov.update_trust(&alpha, 0, 0);
        assert!(res.is_err());
    }
}
