// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Token-bucket rate limiter — local fallback used by `Governance`.
//!
//! Extracted from `governance.rs` per §4.2 hotspot decomposition. The
//! authoritative rate-limit enforcement still lives in AGT
//! (`AgtPolicyProvider` / `AgtRateLimiter`); this module is the
//! in-process fallback used when the AGT provider is not configured
//! or has degraded. Behaviour change: **none** — same global +
//! per-agent bucket structure, same allow semantics, same
//! runtime-update behaviour.
//!
//! Plan: this fallback path is targeted for removal once
//! `governance.rs` becomes pure provider dispatch (plan §4.2 note —
//! "Becomes pure provider dispatch after full AGT provider
//! landings"). Until then we keep it as a documented in-process
//! safety net.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

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
        // Clear per-agent buckets so they pick up new rates on next call.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_request_allowed() {
        let rl = RateLimiter::new(10.0, 10.0, 5.0, 5.0);
        assert!(rl.allow("a1"));
    }

    #[test]
    fn global_capacity_caps_burst() {
        // Global capacity 2, per-agent capacity 100 — global is the
        // tighter constraint.
        let rl = RateLimiter::new(0.0001, 2.0, 100.0, 100.0);
        assert!(rl.allow("a1"));
        assert!(rl.allow("a2"));
        // Third request must fail because global bucket is empty
        // and refill rate is essentially zero.
        assert!(!rl.allow("a3"));
    }

    #[test]
    fn per_agent_capacity_isolates_agents() {
        // Global capacity huge so it isn't the limit; per-agent
        // capacity 1 — second request from same agent fails, but
        // first request from a *different* agent still succeeds.
        let rl = RateLimiter::new(1000.0, 1000.0, 0.0001, 1.0);
        assert!(rl.allow("a1"));
        assert!(!rl.allow("a1"));
        assert!(rl.allow("a2"));
    }

    #[test]
    fn update_rates_clears_per_agent_buckets() {
        let rl = RateLimiter::new(1000.0, 1000.0, 0.0001, 1.0);
        assert!(rl.allow("a1"));
        assert!(!rl.allow("a1"));
        // Bump per-agent capacity; previously-throttled agent regains
        // a fresh bucket on next call.
        rl.update_rates(1000.0, 1000.0, 0.0001, 1.0);
        assert!(rl.allow("a1"));
    }

    #[test]
    fn rate_introspection_reflects_constructor_args() {
        let rl = RateLimiter::new(7.5, 10.0, 1.25, 2.5);
        assert_eq!(rl.global_rate(), 7.5);
        assert_eq!(rl.global_capacity(), 10.0);
        assert_eq!(rl.per_agent_rate(), 1.25);
        assert_eq!(rl.per_agent_capacity(), 2.5);
    }
}
