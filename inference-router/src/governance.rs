//! AGT Governance Module — Inter-Agent Mesh, Trust, Policy, and Audit.
//!
//! This module implements the AGT (Agent Governance Toolkit) control plane
//! within the per-sandbox inference router. Since agents are network-isolated
//! (iptables blocks cross-pod traffic), the inference router is the ONLY
//! path for inter-agent communication.
//!
//! ## Architecture
//!
//! ```text
//! Agent A (sandbox-a)          Agent B (sandbox-b)
//!    ↓                              ↓
//! localhost:8443                localhost:8443
//!    ↓                              ↓
//! Router A ──── K8s Service ──── Router B
//!    ↓              ↑               ↓
//!    └────── mesh-relay-svc ────────┘
//! ```
//!
//! Components:
//! - **Policy enforcement**: Validates tool calls against policy before execution
//! - **Trust scoring**: Per-agent trust scores (0-1000) with decay
//! - **Inter-agent mesh**: Signed message relay between sandboxes
//! - **Audit log**: Hash-chain append-only log for compliance

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;

// ── Policy Engine ───────────────────────────────────────────────────────────

/// Policy action result.
#[derive(Debug, Clone, PartialEq)]
pub enum PolicyDecision {
    Allow,
    Deny(String),
    RequiresApproval(String),
    RateLimited { retry_after_secs: u64 },
}

/// A loaded policy profile.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct PolicyProfile {
    pub version: String,
    pub agent: String,
    pub policies: Vec<Policy>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Policy {
    pub name: String,
    #[serde(rename = "type")]
    pub policy_type: String,
    #[serde(default)]
    pub allowed_actions: Vec<String>,
    #[serde(default)]
    pub denied_actions: Vec<String>,
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default)]
    pub min_approvals: u32,
    #[serde(default)]
    pub max_calls: u32,
    #[serde(default)]
    pub window: String,
}

/// Policy enforcement engine.
pub struct PolicyEngine {
    profile: Option<PolicyProfile>,
    rate_counters: RwLock<HashMap<String, (u64, std::time::Instant)>>,
}

impl PolicyEngine {
    pub fn new() -> Self {
        Self {
            profile: None,
            rate_counters: RwLock::new(HashMap::new()),
        }
    }

    /// Check if a policy profile is loaded.
    pub fn is_loaded(&self) -> bool {
        self.profile.is_some()
    }

    /// Load policies from YAML string.
    pub fn load_from_yaml(&mut self, yaml: &str) -> anyhow::Result<()> {
        let profile: PolicyProfile = serde_yaml::from_str(yaml)?;
        tracing::info!(
            policies = profile.policies.len(),
            agent = %profile.agent,
            "AGT policy profile loaded"
        );
        self.profile = Some(profile);
        Ok(())
    }

    /// Load from environment (AGT_POLICY_DIR + AGT_POLICY_PROFILE).
    pub fn load_from_env(&mut self) {
        let dir = std::env::var("AGT_POLICY_DIR").unwrap_or_default();
        let profile = std::env::var("AGT_POLICY_PROFILE").unwrap_or_else(|_| "default".into());

        if dir.is_empty() {
            return;
        }

        let path = format!("{}/azureclaw-{}.yaml", dir, profile);
        match std::fs::read_to_string(&path) {
            Ok(yaml) => {
                if let Err(e) = self.load_from_yaml(&yaml) {
                    tracing::warn!(path = %path, error = %e, "Failed to parse AGT policy");
                }
            }
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "AGT policy file not found");
            }
        }
    }

    /// Evaluate an action against the loaded policy.
    pub async fn evaluate(&self, action: &str) -> PolicyDecision {
        let profile = match &self.profile {
            Some(p) => p,
            None => return PolicyDecision::Allow, // no policy = allow all
        };

        for policy in &profile.policies {
            match policy.policy_type.as_str() {
                "capability" => {
                    // Check deny list first
                    for denied in &policy.denied_actions {
                        if action_matches(action, denied) {
                            return PolicyDecision::Deny(format!(
                                "Blocked by policy '{}': action '{}' is denied",
                                policy.name, action
                            ));
                        }
                    }
                    // Check allow list (if non-empty, action must match)
                    if !policy.allowed_actions.is_empty() {
                        let allowed = policy.allowed_actions.iter().any(|a| action_matches(action, a));
                        if !allowed && action.starts_with("shell:") {
                            return PolicyDecision::Deny(format!(
                                "Blocked by policy '{}': action '{}' not in allowlist",
                                policy.name, action
                            ));
                        }
                    }
                }
                "approval" => {
                    for pattern in &policy.actions {
                        if action_matches(action, pattern) {
                            return PolicyDecision::RequiresApproval(format!(
                                "Policy '{}' requires {} approval(s) for '{}'",
                                policy.name, policy.min_approvals, action
                            ));
                        }
                    }
                }
                "rate_limit" => {
                    if policy.max_calls > 0 {
                        for pattern in &policy.actions {
                            if action_matches(action, pattern) {
                                return self.check_rate_limit(
                                    &policy.name,
                                    policy.max_calls,
                                    &policy.window,
                                ).await;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        PolicyDecision::Allow
    }

    async fn check_rate_limit(&self, name: &str, max_calls: u32, window: &str) -> PolicyDecision {
        let window_secs = parse_duration(window);
        let mut counters = self.rate_counters.write().await;
        let entry = counters.entry(name.to_string()).or_insert((0, std::time::Instant::now()));

        if entry.1.elapsed().as_secs() > window_secs {
            // Window expired, reset
            *entry = (1, std::time::Instant::now());
            PolicyDecision::Allow
        } else if entry.0 >= max_calls as u64 {
            let retry_after = window_secs - entry.1.elapsed().as_secs();
            PolicyDecision::RateLimited { retry_after_secs: retry_after }
        } else {
            entry.0 += 1;
            PolicyDecision::Allow
        }
    }
}

/// Pattern matching: `shell:*` matches `shell:ls`, `delete_*` matches `delete_file`.
fn action_matches(action: &str, pattern: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        action.starts_with(prefix)
    } else {
        action == pattern
    }
}

fn parse_duration(s: &str) -> u64 {
    if let Some(val) = s.strip_suffix('m') {
        val.parse::<u64>().unwrap_or(60) * 60
    } else if let Some(val) = s.strip_suffix('s') {
        val.parse::<u64>().unwrap_or(60)
    } else {
        60
    }
}

// ── Trust Scoring ───────────────────────────────────────────────────────────

/// Trust tier based on score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TrustTier {
    VerifiedPartner,  // 900-1000
    Trusted,          // 700-899
    Standard,         // 500-699
    Probationary,     // 300-499
    Untrusted,        // 0-299
}

impl TrustTier {
    pub fn from_score(score: u32) -> Self {
        match score {
            900..=1000 => TrustTier::VerifiedPartner,
            700..=899 => TrustTier::Trusted,
            500..=699 => TrustTier::Standard,
            300..=499 => TrustTier::Probationary,
            _ => TrustTier::Untrusted,
        }
    }
}

/// Per-agent trust state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustState {
    pub agent_id: String,
    pub score: u32,
    pub tier: TrustTier,
    pub interactions: u64,
    pub last_interaction: Option<String>,
}

/// Trust store — tracks trust scores for known agents.
/// Persists to /tmp/agt-trust-store.json so scores survive pod restarts.
pub struct TrustStore {
    agents: RwLock<HashMap<String, TrustState>>,
    pub default_score: u32,
    threshold: u32,
    persist_path: Option<String>,
}

impl TrustStore {
    pub fn new(default_score: u32, threshold: u32) -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
            default_score,
            threshold,
            persist_path: None,
        }
    }

    pub fn from_env() -> Self {
        let threshold = std::env::var("AGT_TRUST_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(500);
        let persist_path = Some("/tmp/agt-trust-store.json".to_string());
        let mut store = Self {
            agents: RwLock::new(HashMap::new()),
            default_score: 500,
            threshold,
            persist_path,
        };
        store.load_from_disk();
        store
    }

    /// Load persisted trust state from disk (best-effort).
    fn load_from_disk(&mut self) {
        if let Some(path) = &self.persist_path {
            if let Ok(data) = std::fs::read_to_string(path) {
                if let Ok(states) = serde_json::from_str::<Vec<TrustState>>(&data) {
                    let mut agents = HashMap::new();
                    for state in states {
                        agents.insert(state.agent_id.clone(), state);
                    }
                    self.agents = RwLock::new(agents);
                }
            }
        }
    }

    /// Persist trust state to disk (best-effort, async-safe).
    async fn save_to_disk(&self) {
        if let Some(path) = &self.persist_path {
            let agents = self.agents.read().await;
            let states: Vec<&TrustState> = agents.values().collect();
            if let Ok(json) = serde_json::to_string(&states) {
                let _ = tokio::fs::write(path, json).await;
            }
        }
    }

    /// Get trust state for an agent (creates with default if unknown).
    pub async fn get_trust(&self, agent_id: &str) -> TrustState {
        let agents = self.agents.read().await;
        if let Some(state) = agents.get(agent_id) {
            return state.clone();
        }
        drop(agents);

        let state = TrustState {
            agent_id: agent_id.to_string(),
            score: self.default_score,
            tier: TrustTier::from_score(self.default_score),
            interactions: 0,
            last_interaction: None,
        };

        let mut agents = self.agents.write().await;
        agents.insert(agent_id.to_string(), state.clone());
        state
    }

    /// Check if an agent is trusted enough for communication.
    pub async fn is_trusted(&self, agent_id: &str) -> bool {
        let state = self.get_trust(agent_id).await;
        state.score >= self.threshold
    }

    /// Record a successful interaction (increases trust).
    pub async fn record_success(&self, agent_id: &str) {
        let mut agents = self.agents.write().await;
        if let Some(state) = agents.get_mut(agent_id) {
            state.score = (state.score + 10).min(1000);
            state.tier = TrustTier::from_score(state.score);
            state.interactions += 1;
            state.last_interaction = Some(chrono_now());
        }
        drop(agents);
        self.save_to_disk().await;
    }

    /// Record a failed/suspicious interaction (decreases trust).
    pub async fn record_failure(&self, agent_id: &str) {
        let mut agents = self.agents.write().await;
        if let Some(state) = agents.get_mut(agent_id) {
            state.score = state.score.saturating_sub(50);
            state.tier = TrustTier::from_score(state.score);
            state.interactions += 1;
            state.last_interaction = Some(chrono_now());
        }
        drop(agents);
        self.save_to_disk().await;
    }

    /// Get all trust states.
    pub async fn all_agents(&self) -> Vec<TrustState> {
        self.agents.read().await.values().cloned().collect()
    }

    /// Synchronously seed an agent with the default score (used at startup).
    pub fn seed_agent(&mut self, agent_id: &str) {
        let state = TrustState {
            agent_id: agent_id.to_string(),
            score: self.default_score,
            tier: TrustTier::from_score(self.default_score),
            interactions: 0,
            last_interaction: None,
        };
        self.agents.get_mut().insert(agent_id.to_string(), state);
    }
}

// ── Audit Log ───────────────────────────────────────────────────────────────

/// A single audit entry in the hash-chain log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub seq: u64,
    pub timestamp: String,
    pub sandbox: String,
    pub action: String,
    pub decision: String,
    pub details: String,
    /// SHA-256 hash of previous entry (hash-chain integrity)
    pub prev_hash: String,
    /// SHA-256 hash of this entry
    pub hash: String,
}

/// Append-only hash-chain audit log.
pub struct AuditLog {
    entries: RwLock<Vec<AuditEntry>>,
    sandbox_name: String,
}

impl AuditLog {
    pub fn new(sandbox_name: &str) -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
            sandbox_name: sandbox_name.to_string(),
        }
    }

    /// Append an audit entry with hash-chain integrity.
    pub async fn append(&self, action: &str, decision: &str, details: &str) -> AuditEntry {
        let mut entries = self.entries.write().await;
        let seq = entries.len() as u64;
        let prev_hash = entries.last()
            .map(|e| e.hash.clone())
            .unwrap_or_else(|| "genesis".to_string());

        let entry = AuditEntry {
            seq,
            timestamp: chrono_now(),
            sandbox: self.sandbox_name.clone(),
            action: action.to_string(),
            decision: decision.to_string(),
            details: details.to_string(),
            prev_hash: prev_hash.clone(),
            hash: String::new(), // computed below
        };

        // Compute hash: SHA-256(seq | timestamp | action | decision | prev_hash)
        let hash_input = format!("{}|{}|{}|{}|{}",
            entry.seq, entry.timestamp, entry.action, entry.decision, prev_hash);
        let hash = sha256_hex(&hash_input);

        let mut entry = entry;
        entry.hash = hash;
        entries.push(entry.clone());

        tracing::debug!(
            seq = entry.seq,
            action = %entry.action,
            decision = %entry.decision,
            "AGT audit entry"
        );

        entry
    }

    /// Get all entries (for API response).
    pub async fn entries(&self) -> Vec<AuditEntry> {
        self.entries.read().await.clone()
    }

    /// Get entries since a sequence number.
    #[allow(dead_code)]
    pub async fn entries_since(&self, since_seq: u64) -> Vec<AuditEntry> {
        self.entries.read().await
            .iter()
            .filter(|e| e.seq >= since_seq)
            .cloned()
            .collect()
    }

    /// Verify hash-chain integrity.
    pub async fn verify_integrity(&self) -> bool {
        let entries = self.entries.read().await;
        for (i, entry) in entries.iter().enumerate() {
            let expected_prev = if i == 0 {
                "genesis".to_string()
            } else {
                entries[i - 1].hash.clone()
            };
            if entry.prev_hash != expected_prev {
                tracing::error!(seq = entry.seq, "Audit log integrity violation: prev_hash mismatch");
                return false;
            }
            let hash_input = format!("{}|{}|{}|{}|{}",
                entry.seq, entry.timestamp, entry.action, entry.decision, entry.prev_hash);
            if entry.hash != sha256_hex(&hash_input) {
                tracing::error!(seq = entry.seq, "Audit log integrity violation: hash mismatch");
                return false;
            }
        }
        true
    }
}

// ── Inter-Agent Mesh Message ────────────────────────────────────────────────

/// A message in the inter-agent communication mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshMessage {
    pub id: String,
    pub from_agent: String,
    pub to_agent: String,
    pub content: String,
    pub message_type: String,
    pub timestamp: String,
    /// HMAC-SHA256 signature using sender's sandbox secret
    pub signature: String,
}

/// Inbox for received mesh messages.
pub struct MeshInbox {
    messages: RwLock<Vec<MeshMessage>>,
}

impl MeshInbox {
    pub fn new() -> Self {
        Self {
            messages: RwLock::new(Vec::new()),
        }
    }

    pub async fn receive(&self, msg: MeshMessage) {
        self.messages.write().await.push(msg);
    }

    #[allow(dead_code)]
    pub async fn drain(&self) -> Vec<MeshMessage> {
        let mut msgs = self.messages.write().await;
        std::mem::take(&mut *msgs)
    }

    pub async fn peek(&self) -> Vec<MeshMessage> {
        self.messages.read().await.clone()
    }
}

// ── Governance State ────────────────────────────────────────────────────────

/// Combined AGT governance state for a sandbox.
pub struct GovernanceState {
    pub enabled: bool,
    pub policy: PolicyEngine,
    pub trust: TrustStore,
    pub audit: AuditLog,
    pub inbox: MeshInbox,
    pub sandbox_name: String,
}

impl GovernanceState {
    pub fn new(sandbox_name: &str) -> Self {
        let enabled = std::env::var("AGT_GOVERNANCE_ENABLED")
            .map(|v| v == "true")
            .unwrap_or(false);

        let mut policy = PolicyEngine::new();
        let trust = TrustStore::from_env();

        if enabled {
            policy.load_from_env();
            tracing::info!(sandbox = %sandbox_name, "AGT governance ENABLED");
        }

        let trust = {
            let mut t = trust;
            // Seed self agent so /agt/status always shows at least one trust entry
            if enabled {
                t.seed_agent(sandbox_name);
            }
            t
        };

        Self {
            enabled,
            policy,
            trust,
            audit: AuditLog::new(sandbox_name),
            inbox: MeshInbox::new(),
            sandbox_name: sandbox_name.to_string(),
        }
    }

    /// Evaluate a tool action and log it.
    pub async fn evaluate_action(&self, action: &str) -> PolicyDecision {
        if !self.enabled {
            return PolicyDecision::Allow;
        }

        let decision = self.policy.evaluate(action).await;
        let decision_str = match &decision {
            PolicyDecision::Allow => "allow",
            PolicyDecision::Deny(_) => "deny",
            PolicyDecision::RequiresApproval(_) => "requires_approval",
            PolicyDecision::RateLimited { .. } => "rate_limited",
        };

        self.audit.append(action, decision_str, &format!("{:?}", decision)).await;
        decision
    }
}

// ── Utility ─────────────────────────────────────────────────────────────────

fn chrono_now() -> String {
    // Simple ISO 8601 timestamp without chrono dependency
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    // Approximate ISO format
    format!("{}Z", secs)
}

fn sha256_hex(input: &str) -> String {
    // Minimal SHA-256 using ring-free approach:
    // We use a simple hash for the prototype. In production, use ring::digest.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    let h1 = hasher.finish();
    input.len().hash(&mut hasher);
    let h2 = hasher.finish();
    format!("{:016x}{:016x}", h1, h2)
}
