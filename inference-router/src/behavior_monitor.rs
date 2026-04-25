//! Local-fallback behavior monitor — in-process anomaly detector.
//!
//! Extracted from `governance.rs` per §4.2 hotspot decomposition.
//! Authoritative behavioral anomaly detection lives in AGT
//! (`BehaviorMonitor` per `docs/agt-boundary.md` §1.1); this module
//! is the in-process fallback used when the AGT-side monitor is not
//! configured / has degraded. Behaviour change: **none**.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

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

        // Reset window every 60 seconds.
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

        // Return true if anomaly detected.
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
    pub fn alerts_detail(&self) -> Vec<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_success_no_alert() {
        let m = BehaviorMonitor::new(100, 5, 10);
        assert!(!m.record("a1", true));
        assert_eq!(m.alert_count(), 0);
        assert!(m.alerts_detail().is_empty());
    }

    #[test]
    fn consecutive_failures_trip_threshold() {
        let m = BehaviorMonitor::new(1000, 2, 1000);
        assert!(!m.record("a1", false));
        assert!(!m.record("a1", false));
        // Third failure pushes counter to 3, above threshold 2.
        assert!(m.record("a1", false));
        assert_eq!(m.alert_count(), 1);
        let detail = m.alerts_detail();
        assert_eq!(detail.len(), 1);
        assert_eq!(detail[0]["agent"], "a1");
        let reasons = detail[0]["reasons"].as_array().unwrap();
        assert!(
            reasons
                .iter()
                .any(|r| r.as_str().unwrap().contains("consecutive failures"))
        );
    }

    #[test]
    fn success_resets_consecutive_failures_but_not_capability_denials() {
        let m = BehaviorMonitor::new(1000, 2, 2);
        m.record("a1", false);
        m.record("a1", false);
        m.record("a1", true);
        // consecutive_failures reset to 0 by the success — not an alert
        // on that axis. capability_denials persist (2, not above
        // threshold 2 yet).
        assert!(!m.record("a1", true));
        // Two more failures bring capability_denials above threshold.
        m.record("a1", false);
        assert!(m.record("a1", false));
    }

    #[test]
    fn burst_threshold_trips_on_high_volume() {
        let m = BehaviorMonitor::new(2, 1000, 1000);
        m.record("a1", true);
        m.record("a1", true);
        // Third call in same 60 s window pushes recent_calls above 2.
        assert!(m.record("a1", true));
    }

    #[test]
    fn agents_isolated_in_state() {
        let m = BehaviorMonitor::new(1000, 2, 1000);
        m.record("a1", false);
        m.record("a1", false);
        m.record("a1", false);
        // Different agent untouched.
        assert!(!m.record("a2", true));
        let alerts = m.alerts_detail();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0]["agent"], "a1");
    }
}
