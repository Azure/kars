//! Per-subject token-bucket rate limiter.
//!
//! In-memory only. Replicas of the gateway do not synchronise their
//! buckets in v1; sharing state across replicas is a v2 concern
//! gated by `rate_limit.shared_redis_url` (the impl is `unimplemented!()`
//! below — see [`SharedRedisLimiter`]).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// Token-bucket parameters. `capacity` is the burst allowance,
/// `refill_per_sec` is the steady-state rate.
#[derive(Debug, Clone, Copy)]
pub struct BucketSpec {
    pub capacity: u32,
    pub refill_per_sec: f64,
}

impl Default for BucketSpec {
    fn default() -> Self {
        // 60 burst, 5 rps steady state — chosen so a normally-paced
        // peer can complete an A2A handshake (card fetch + 2..3
        // tasks/send) within the burst, but a runaway peer is capped
        // at 5 rps before the first 60-second window closes.
        Self {
            capacity: 60,
            refill_per_sec: 5.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

/// In-memory limiter, keyed by verified subject claim from the JWS.
pub struct SubjectLimiter {
    spec: BucketSpec,
    state: Mutex<HashMap<String, Bucket>>,
    max_subjects: usize,
}

impl SubjectLimiter {
    pub fn new(spec: BucketSpec, max_subjects: usize) -> Self {
        Self {
            spec,
            state: Mutex::new(HashMap::new()),
            max_subjects,
        }
    }

    /// Returns `true` if the request is permitted; `false` if the
    /// subject is currently over-budget.
    pub fn allow(&self, subject: &str) -> bool {
        let mut g = self.state.lock().expect("limiter poisoned");
        let now = Instant::now();
        let cap = self.spec.capacity as f64;
        let rate = self.spec.refill_per_sec;

        let bucket = g.entry(subject.to_string()).or_insert(Bucket {
            tokens: cap,
            last_refill: now,
        });

        let dt = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + dt * rate).min(cap);
        bucket.last_refill = now;

        let permitted = if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        };

        if g.len() > self.max_subjects {
            // Evict the most-stale subject. With the configured cap
            // (default 50k) this only kicks in under a sustained
            // distinct-subject spray — i.e., a credential-stuffing
            // pattern that should already have been bounced upstream
            // by the JWS issuer allow-list.
            let stale = g
                .iter()
                .min_by_key(|(_, b)| b.last_refill)
                .map(|(k, _)| k.clone());
            if let Some(k) = stale
                && k != subject
            {
                g.remove(&k);
            }
        }
        permitted
    }

    pub fn subject_count(&self) -> usize {
        self.state.lock().map(|g| g.len()).unwrap_or(0)
    }
}

/// Cross-replica synchronisation entry-point. Feature-flagged off by
/// default — see Helm value `a2aGateway.rateLimits.sharedRedisUrl`.
///
/// **Not implemented in S3.5.** v1 ships with in-memory limiters per
/// replica; the threat model treats burst-skew across replicas as
/// acceptable because the inference-router *also* enforces a
/// per-subject limit downstream.
pub struct SharedRedisLimiter;

impl SharedRedisLimiter {
    pub fn connect(_url: &str) -> Self {
        // TODO(s3.6+): wire to redis crate. Deliberately unimplemented
        // in S3.5 so the surface is locked in but no half-baked impl
        // ships.
        unimplemented!("shared redis rate limiter is a post-S3.5 feature; see ADR-0001 #4");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn first_request_permitted() {
        let l = SubjectLimiter::new(
            BucketSpec {
                capacity: 5,
                refill_per_sec: 1.0,
            },
            1024,
        );
        assert!(l.allow("alice"));
    }

    #[test]
    fn burst_capacity_respected() {
        let l = SubjectLimiter::new(
            BucketSpec {
                capacity: 3,
                refill_per_sec: 0.0,
            },
            1024,
        );
        assert!(l.allow("alice"));
        assert!(l.allow("alice"));
        assert!(l.allow("alice"));
        assert!(!l.allow("alice"));
    }

    #[test]
    fn distinct_subjects_isolated() {
        let l = SubjectLimiter::new(
            BucketSpec {
                capacity: 1,
                refill_per_sec: 0.0,
            },
            1024,
        );
        assert!(l.allow("alice"));
        assert!(!l.allow("alice"));
        assert!(l.allow("bob"));
    }

    #[test]
    fn refill_replenishes_tokens() {
        let l = SubjectLimiter::new(
            BucketSpec {
                capacity: 2,
                refill_per_sec: 100.0,
            },
            1024,
        );
        assert!(l.allow("c"));
        assert!(l.allow("c"));
        assert!(!l.allow("c"));
        std::thread::sleep(Duration::from_millis(40));
        assert!(l.allow("c"), "tokens should refill within 40ms at 100/s");
    }

    #[test]
    fn subject_count_tracked() {
        let l = SubjectLimiter::new(BucketSpec::default(), 1024);
        l.allow("a");
        l.allow("b");
        l.allow("c");
        assert_eq!(l.subject_count(), 3);
    }

    #[test]
    fn eviction_bounds_subject_map() {
        let l = SubjectLimiter::new(BucketSpec::default(), 4);
        for i in 0..40 {
            l.allow(&format!("s-{i}"));
        }
        // Cap +1 in worst case (we evict only after exceeding).
        assert!(l.subject_count() <= 5);
    }

    #[test]
    #[should_panic(expected = "post-S3.5")]
    fn shared_redis_is_unimplemented_in_v1() {
        let _ = SharedRedisLimiter::connect("redis://example:6379");
    }
}
