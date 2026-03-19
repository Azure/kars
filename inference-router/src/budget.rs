//! Token budget enforcement — per-sandbox daily and per-request limits.
//!
//! Tracks cumulative token usage in memory (resets daily) and rejects
//! requests that would exceed the configured budget.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Per-sandbox token usage tracker with daily reset.
#[derive(Clone)]
pub struct TokenBudgetTracker {
    /// Daily token limit (0 = unlimited)
    daily_limit: u64,
    /// Per-request token limit (0 = unlimited)
    per_request_limit: u64,
    /// Per-sandbox cumulative usage since last reset
    usage: Arc<RwLock<HashMap<String, SandboxUsage>>>,
}

struct SandboxUsage {
    total_tokens: u64,
    reset_at: Instant,
}

impl TokenBudgetTracker {
    pub fn new(daily_limit: u64, per_request_limit: u64) -> Self {
        Self {
            daily_limit,
            per_request_limit,
            usage: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Check if a request is allowed (pre-request check).
    /// Returns Ok(()) if allowed, Err(message) if budget exceeded.
    pub async fn check_budget(&self, sandbox: &str) -> Result<(), String> {
        if self.daily_limit == 0 {
            return Ok(());
        }

        let usage = self.usage.read().await;
        if let Some(entry) = usage.get(sandbox) {
            // Reset if a day has passed
            if entry.reset_at.elapsed() < Duration::from_secs(86400)
                && entry.total_tokens >= self.daily_limit
            {
                return Err(format!(
                    "Daily token budget exceeded ({}/{} tokens). Resets in {}h.",
                    entry.total_tokens,
                    self.daily_limit,
                    (86400 - entry.reset_at.elapsed().as_secs()) / 3600
                ));
            }
        }
        Ok(())
    }

    /// Check if a response's token usage exceeds per-request limit.
    pub fn check_per_request(&self, total_tokens: u64) -> Result<(), String> {
        if self.per_request_limit > 0 && total_tokens > self.per_request_limit {
            return Err(format!(
                "Per-request token limit exceeded ({}/{} tokens)",
                total_tokens, self.per_request_limit
            ));
        }
        Ok(())
    }

    /// Record token usage from a completed request.
    pub async fn record_usage(&self, sandbox: &str, tokens: u64) {
        let mut usage = self.usage.write().await;
        let entry = usage.entry(sandbox.to_string()).or_insert(SandboxUsage {
            total_tokens: 0,
            reset_at: Instant::now(),
        });

        // Reset if a day has passed
        if entry.reset_at.elapsed() >= Duration::from_secs(86400) {
            entry.total_tokens = 0;
            entry.reset_at = Instant::now();
        }

        entry.total_tokens += tokens;

        if self.daily_limit > 0 {
            let pct = (entry.total_tokens as f64 / self.daily_limit as f64 * 100.0) as u64;
            if pct >= 90 {
                tracing::warn!(
                    sandbox = %sandbox,
                    used = entry.total_tokens,
                    limit = self.daily_limit,
                    "Token budget at {pct}%"
                );
            }
        }
    }

    /// Get current usage for a sandbox.
    pub async fn get_usage(&self, sandbox: &str) -> (u64, u64) {
        let usage = self.usage.read().await;
        match usage.get(sandbox) {
            Some(entry) => (entry.total_tokens, self.daily_limit),
            None => (0, self.daily_limit),
        }
    }
}
