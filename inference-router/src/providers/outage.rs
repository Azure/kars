// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Outage semantics for AGT / mesh provider calls.
//!
//! Reference: internal Phase 1 plan §1.3.
//!
//! When a provider (AGT or vendored mesh) is unreachable / slow / erroring,
//! the per-tenant outage policy decides what the router does with the
//! in-flight request. Three modes:
//!
//! * [`OutageMode::Strict`]      — fail-closed. Prod default.
//! * [`OutageMode::CachedRead`]  — allow if a cached decision is under TTL,
//!   otherwise fail-closed.
//! * [`OutageMode::DegradedDev`] — fail-open with a warning label on the
//!   response. Rejected in prod by admission unless the `ClawSandbox`
//!   carries `azureclaw.azure.com/dev-only: "true"`.
//!
//! This module is **pure data + pure logic**. It does not make any provider
//! call — it is the deterministic decision function the call-site reaches
//! for once a provider call has returned an error.
//!
//! Wire format (serde): `camelCase` — `"strict" | "cachedRead" | "degradedDev"`.
//! Must match `ClawSandbox.spec.agt.outageMode` enum (CRD CEL validation
//! lands with the CRD itself in Phase 1 minimal-CRDs scope).

use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// Per-tenant policy selecting what the router does when an AGT/mesh
/// provider call fails or times out.
///
/// Default is [`Strict`][Self::Strict] — the only prod-safe choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutageMode {
    /// Fail-closed. Reject the request. Prod default.
    Strict,
    /// Allow when a cached provider decision exists and is within TTL;
    /// otherwise fail-closed.
    CachedRead,
    /// Fail-open with a warning label. Admission rejects this in non-dev
    /// tenants. See `ci/no-null-provider-prod.sh` and the
    /// `null-provider-admission` VAP for the static + runtime mirror.
    DegradedDev,
}

impl Default for OutageMode {
    /// [`Strict`][Self::Strict] — fail-closed. Matches plan §0.2 #8
    /// (fail-closed defaults).
    fn default() -> Self {
        Self::Strict
    }
}

impl fmt::Display for OutageMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Strict => f.write_str("strict"),
            Self::CachedRead => f.write_str("cachedRead"),
            Self::DegradedDev => f.write_str("degradedDev"),
        }
    }
}

impl FromStr for OutageMode {
    type Err = OutageParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Accept both camelCase (wire) and lowercase shorthand. Nothing else.
        match s {
            "strict" | "Strict" => Ok(Self::Strict),
            "cachedRead" | "cached-read" | "cached_read" => Ok(Self::CachedRead),
            "degradedDev" | "degraded-dev" | "degraded_dev" => Ok(Self::DegradedDev),
            other => Err(OutageParseError(other.to_string())),
        }
    }
}

impl OutageMode {
    /// `true` when this mode is only legal in a dev tenant. Admission +
    /// `OutageConfig::validate_for_env` both enforce this.
    pub fn is_dev_only(self) -> bool {
        matches!(self, Self::DegradedDev)
    }

    /// `true` when this mode can serve a stale cached decision.
    pub fn permits_cached_fallback(self) -> bool {
        matches!(self, Self::CachedRead)
    }
}

/// Error produced by [`OutageMode::from_str`] when the wire value is not
/// one of the three recognised modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutageParseError(pub String);

impl fmt::Display for OutageParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unrecognised outageMode {:?} (expected strict, cachedRead, or degradedDev)",
            self.0
        )
    }
}

impl std::error::Error for OutageParseError {}

/// TTL used by [`OutageMode::CachedRead`] when no per-sandbox override is
/// set. Matches plan §1.3 cached-decision TTL default.
pub const DEFAULT_CACHED_TTL: Duration = Duration::from_secs(60);

/// Maximum per-sandbox cached TTL override. Caps how stale a policy
/// decision the router will accept in a CachedRead outage. Deliberately
/// short: if a cache can outlive an incident by hours, it's replacing
/// AGT rather than buffering a blip.
pub const MAX_CACHED_TTL: Duration = Duration::from_secs(15 * 60);

/// Error returned by [`OutageConfig::validate_for_env`] when the config
/// is not legal for the runtime environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutageConfigError {
    /// `DegradedDev` selected on a non-dev tenant.
    DegradedDevInProd,
    /// `cached_ttl` exceeds [`MAX_CACHED_TTL`].
    CachedTtlTooLarge { requested: Duration, max: Duration },
    /// `cached_ttl` is zero for a `CachedRead` config.
    CachedTtlZero,
}

impl fmt::Display for OutageConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DegradedDevInProd => f.write_str(
                "outageMode=degradedDev is only valid on dev-only sandboxes (label azureclaw.azure.com/dev-only=true)",
            ),
            Self::CachedTtlTooLarge { requested, max } => write!(
                f,
                "cachedTtl {requested:?} exceeds maximum {max:?}"
            ),
            Self::CachedTtlZero => {
                f.write_str("cachedTtl must be greater than zero for outageMode=cachedRead")
            }
        }
    }
}

impl std::error::Error for OutageConfigError {}

/// Per-tenant outage configuration, typically read from
/// `ClawSandbox.spec.agt.{outageMode,cachedTtlSeconds}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutageConfig {
    /// Which outage behaviour applies.
    pub mode: OutageMode,
    /// TTL used by [`OutageMode::CachedRead`]. Ignored by other modes but
    /// kept round-trippable through serde.
    #[serde(default = "default_cached_ttl")]
    pub cached_ttl: Duration,
}

fn default_cached_ttl() -> Duration {
    DEFAULT_CACHED_TTL
}

impl Default for OutageConfig {
    fn default() -> Self {
        Self {
            mode: OutageMode::default(),
            cached_ttl: DEFAULT_CACHED_TTL,
        }
    }
}

impl OutageConfig {
    /// Validates this configuration against the runtime environment.
    ///
    /// * `is_dev_env` must be `true` only for sandboxes that admission has
    ///   already marked dev-only (mirrors the static `ci/no-null-provider-prod.sh`
    ///   guard and the null-provider VAP).
    pub fn validate_for_env(&self, is_dev_env: bool) -> Result<(), OutageConfigError> {
        if self.mode == OutageMode::DegradedDev && !is_dev_env {
            return Err(OutageConfigError::DegradedDevInProd);
        }
        if self.mode == OutageMode::CachedRead {
            if self.cached_ttl.is_zero() {
                return Err(OutageConfigError::CachedTtlZero);
            }
            if self.cached_ttl > MAX_CACHED_TTL {
                return Err(OutageConfigError::CachedTtlTooLarge {
                    requested: self.cached_ttl,
                    max: MAX_CACHED_TTL,
                });
            }
        }
        Ok(())
    }
}

/// A cached provider decision — what the router had from the last successful
/// provider call for the same request shape.
///
/// `T` is the provider's verdict type (policy verdicts, signing receipts,
/// etc.) — the outage decision logic is verdict-agnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedDecision<T> {
    pub verdict: T,
    pub created_at: SystemTime,
}

impl<T> CachedDecision<T> {
    pub fn new(verdict: T, created_at: SystemTime) -> Self {
        Self {
            verdict,
            created_at,
        }
    }

    /// `true` if this cache entry is older than `ttl` relative to `now`.
    ///
    /// Clock going backwards (`now < created_at`) is treated as expired to
    /// fail-closed on any observable clock skew — consistent with §0.2 #8.
    pub fn is_expired(&self, ttl: Duration, now: SystemTime) -> bool {
        match now.duration_since(self.created_at) {
            Ok(age) => age > ttl,
            Err(_) => true,
        }
    }
}

/// The result of applying [`decide_outage`] to a provider failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutageAction<T> {
    /// Reject the in-flight request. Carries the mode that produced the
    /// decision for caller-side logging / audit.
    Deny { mode: OutageMode },
    /// Serve the cached verdict. Only produced by [`OutageMode::CachedRead`]
    /// with a non-expired cache.
    ServeCached { verdict: T },
    /// Fail-open with a warning label. Only produced by
    /// [`OutageMode::DegradedDev`]. Caller MUST stamp the warning label on
    /// the response.
    AllowWithWarning,
}

/// Given a validated [`OutageConfig`] and an optional cached verdict, decide
/// what to do when a provider call has failed.
///
/// **Pure.** This function does not perform I/O and does not mutate the
/// cache — the caller owns both. Clock is injected as `now` so tests run
/// without sleeping.
pub fn decide_outage<T: Copy>(
    config: &OutageConfig,
    cached: Option<CachedDecision<T>>,
    now: SystemTime,
) -> OutageAction<T> {
    match config.mode {
        OutageMode::Strict => OutageAction::Deny {
            mode: OutageMode::Strict,
        },
        OutageMode::CachedRead => match cached {
            Some(c) if !c.is_expired(config.cached_ttl, now) => {
                OutageAction::ServeCached { verdict: c.verdict }
            }
            _ => OutageAction::Deny {
                mode: OutageMode::CachedRead,
            },
        },
        OutageMode::DegradedDev => OutageAction::AllowWithWarning,
    }
}

// ---------------------------------------------------------------------------
// tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_is_strict() {
        assert_eq!(OutageMode::default(), OutageMode::Strict);
        assert_eq!(OutageConfig::default().mode, OutageMode::Strict);
    }

    #[test]
    fn display_matches_wire_format() {
        assert_eq!(OutageMode::Strict.to_string(), "strict");
        assert_eq!(OutageMode::CachedRead.to_string(), "cachedRead");
        assert_eq!(OutageMode::DegradedDev.to_string(), "degradedDev");
    }

    #[test]
    fn from_str_accepts_camel_kebab_and_snake() {
        assert_eq!("strict".parse::<OutageMode>().unwrap(), OutageMode::Strict);
        assert_eq!(
            "cachedRead".parse::<OutageMode>().unwrap(),
            OutageMode::CachedRead
        );
        assert_eq!(
            "cached-read".parse::<OutageMode>().unwrap(),
            OutageMode::CachedRead
        );
        assert_eq!(
            "cached_read".parse::<OutageMode>().unwrap(),
            OutageMode::CachedRead
        );
        assert_eq!(
            "degradedDev".parse::<OutageMode>().unwrap(),
            OutageMode::DegradedDev
        );
    }

    #[test]
    fn from_str_rejects_garbage() {
        assert!("".parse::<OutageMode>().is_err());
        assert!("fail-open".parse::<OutageMode>().is_err());
        assert!("STRICT".parse::<OutageMode>().is_err()); // case-sensitive beyond the two exact variants
        assert!("cached".parse::<OutageMode>().is_err());
        let err = "nope".parse::<OutageMode>().unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn dev_only_flag_marks_degraded_dev() {
        assert!(!OutageMode::Strict.is_dev_only());
        assert!(!OutageMode::CachedRead.is_dev_only());
        assert!(OutageMode::DegradedDev.is_dev_only());
    }

    #[test]
    fn permits_cached_fallback_only_for_cached_read() {
        assert!(!OutageMode::Strict.permits_cached_fallback());
        assert!(OutageMode::CachedRead.permits_cached_fallback());
        assert!(!OutageMode::DegradedDev.permits_cached_fallback());
    }

    #[test]
    fn serde_roundtrips_via_camel_case() {
        let cfg = OutageConfig {
            mode: OutageMode::CachedRead,
            cached_ttl: Duration::from_secs(30),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains(r#""mode":"cachedRead""#), "got {json}");
        let rt: OutageConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(rt, cfg);
    }

    #[test]
    fn serde_defaults_ttl_when_omitted() {
        let json = r#"{"mode":"strict"}"#;
        let cfg: OutageConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.mode, OutageMode::Strict);
        assert_eq!(cfg.cached_ttl, DEFAULT_CACHED_TTL);
    }

    #[test]
    fn serde_rejects_unknown_mode() {
        let json = r#"{"mode":"laissezFaire","cachedTtl":{"secs":1,"nanos":0}}"#;
        assert!(serde_json::from_str::<OutageConfig>(json).is_err());
    }

    #[test]
    fn validate_rejects_degraded_dev_in_prod() {
        let cfg = OutageConfig {
            mode: OutageMode::DegradedDev,
            ..OutageConfig::default()
        };
        assert_eq!(
            cfg.validate_for_env(false),
            Err(OutageConfigError::DegradedDevInProd)
        );
        assert!(cfg.validate_for_env(true).is_ok());
    }

    #[test]
    fn validate_rejects_zero_ttl_on_cached_read() {
        let cfg = OutageConfig {
            mode: OutageMode::CachedRead,
            cached_ttl: Duration::from_secs(0),
        };
        assert_eq!(
            cfg.validate_for_env(false),
            Err(OutageConfigError::CachedTtlZero)
        );
    }

    #[test]
    fn validate_rejects_excessive_ttl_on_cached_read() {
        let cfg = OutageConfig {
            mode: OutageMode::CachedRead,
            cached_ttl: MAX_CACHED_TTL + Duration::from_secs(1),
        };
        match cfg.validate_for_env(false) {
            Err(OutageConfigError::CachedTtlTooLarge { requested, max }) => {
                assert_eq!(requested, MAX_CACHED_TTL + Duration::from_secs(1));
                assert_eq!(max, MAX_CACHED_TTL);
            }
            other => panic!("expected CachedTtlTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn validate_permits_strict_regardless_of_ttl() {
        // Non-CachedRead modes ignore ttl — otherwise you couldn't change
        // mode from CachedRead→Strict without also editing cachedTtl.
        let cfg = OutageConfig {
            mode: OutageMode::Strict,
            cached_ttl: Duration::from_secs(0),
        };
        assert!(cfg.validate_for_env(false).is_ok());
    }

    #[test]
    fn strict_always_denies() {
        let cfg = OutageConfig::default();
        let cached = Some(CachedDecision::new(true, SystemTime::now()));
        match decide_outage(&cfg, cached, SystemTime::now()) {
            OutageAction::Deny {
                mode: OutageMode::Strict,
            } => {}
            other => panic!("expected Deny(Strict), got {other:?}"),
        }
    }

    #[test]
    fn cached_read_serves_fresh_cache() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let cfg = OutageConfig {
            mode: OutageMode::CachedRead,
            cached_ttl: Duration::from_secs(60),
        };
        let cached = Some(CachedDecision::new("allow", now - Duration::from_secs(10)));
        match decide_outage(&cfg, cached, now) {
            OutageAction::ServeCached { verdict } => assert_eq!(verdict, "allow"),
            other => panic!("expected ServeCached, got {other:?}"),
        }
    }

    #[test]
    fn cached_read_denies_when_cache_expired() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let cfg = OutageConfig {
            mode: OutageMode::CachedRead,
            cached_ttl: Duration::from_secs(60),
        };
        let cached = Some(CachedDecision::new("allow", now - Duration::from_secs(120)));
        match decide_outage(&cfg, cached, now) {
            OutageAction::Deny {
                mode: OutageMode::CachedRead,
            } => {}
            other => panic!("expected Deny(CachedRead), got {other:?}"),
        }
    }

    #[test]
    fn cached_read_denies_when_no_cache() {
        let cfg = OutageConfig {
            mode: OutageMode::CachedRead,
            cached_ttl: Duration::from_secs(60),
        };
        let decision: OutageAction<&'static str> = decide_outage(&cfg, None, SystemTime::now());
        match decision {
            OutageAction::Deny {
                mode: OutageMode::CachedRead,
            } => {}
            other => panic!("expected Deny(CachedRead), got {other:?}"),
        }
    }

    #[test]
    fn cached_read_treats_backwards_clock_as_expired() {
        // now < created_at — bias toward fail-closed.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let future = now + Duration::from_secs(10);
        let cfg = OutageConfig {
            mode: OutageMode::CachedRead,
            cached_ttl: Duration::from_secs(60),
        };
        let cached = Some(CachedDecision::new(0u8, future));
        match decide_outage(&cfg, cached, now) {
            OutageAction::Deny {
                mode: OutageMode::CachedRead,
            } => {}
            other => panic!("expected Deny(CachedRead), got {other:?}"),
        }
    }

    #[test]
    fn degraded_dev_allows_with_warning() {
        let cfg = OutageConfig {
            mode: OutageMode::DegradedDev,
            ..OutageConfig::default()
        };
        let decision: OutageAction<&'static str> = decide_outage(&cfg, None, SystemTime::now());
        assert_eq!(decision, OutageAction::AllowWithWarning);
    }
}
