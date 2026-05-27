// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `/internal/*` — operator-only introspection routes.
//!
//! These routes are mounted on the `protected` axum router in
//! `main.rs`, so they inherit the admin-token gate +
//! `ADMIN_ALLOW_IPS` allowlist. They are deliberately not under
//! `/agt/*` (which is also protected but reserved for governance
//! mutations) or `/admin/*` (which is reserved for operator-driven
//! mutations like `/admin/model`).
//!
//! The first endpoint, `GET /internal/policy-status`, is the bottom
//! half of the `Ready ⇔ router echoed digest` loop introduced by
//! `crd-well-oiled-machine` Slice 1. The controller polls this route
//! and only promotes a CRD from `phase=Compiled` to `phase=Ready` when
//! the digest in the response matches the digest it published.
//!
//! The shape is a stable wire contract — clients (controller poller,
//! `kars inspect`, headlamp plugin) match on field names. Don't
//! rename fields without a deprecation window.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use serde::Serialize;

use super::AppState;
use crate::deployment_health::DeploymentHealthSnapshot;
use crate::policy_status::PolicyStatusEntry;

pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route("/internal/policy-status", get(policy_status))
        .route("/internal/egress/blocked", get(egress_blocked))
        .route("/internal/egress/blocked/top", get(egress_blocked_top))
}

/// JSON envelope returned by `GET /internal/policy-status`. Each
/// `PolicyStatusEntry` carries `kind`, `digest`, `source_path`,
/// `loaded_at`, and `last_error` — see [`crate::policy_status`] for
/// the field-level contract.
#[derive(Debug, Serialize)]
struct PolicyStatusResponse {
    /// Schema version of this response envelope. Bump on breaking
    /// changes to the entry shape so clients can fail loudly.
    schema_version: u32,
    count: usize,
    entries: Vec<EntryDto>,
    /// Per-deployment health snapshot — additive in schema_version 1
    /// (always present, may be empty). Populated by the Slice 2d.2
    /// `forward_with_failover` walker as it observes upstream
    /// responses. Clients that don't care can ignore it; the
    /// controller and `kars inspect` consume it for surfacing
    /// `modelPreference.fallback[]` activity.
    #[serde(default)]
    deployment_health: Vec<DeploymentHealthSnapshot>,
}

/// Wire DTO. We deliberately don't `#[derive(Serialize)]` on
/// `PolicyStatusEntry` and expose it directly because `SystemTime`
/// serializes as a tagged-union nanosecond pair by default — useless
/// for downstream consumers. Convert to RFC 3339 here.
#[derive(Debug, Serialize)]
struct EntryDto {
    kind: &'static str,
    digest: Option<String>,
    source_path: String,
    loaded_at: String,
    last_error: Option<String>,
}

impl From<PolicyStatusEntry> for EntryDto {
    fn from(e: PolicyStatusEntry) -> Self {
        Self {
            kind: e.kind.as_str(),
            digest: e.digest,
            source_path: e.source_path,
            loaded_at: format_rfc3339(e.loaded_at),
            last_error: e.last_error,
        }
    }
}

/// RFC 3339 / ISO 8601 formatter for `SystemTime`. We avoid `chrono`
/// here because the router already pulls `time = "0.3"` transitively
/// via tower/hyper; introducing a second time crate just for one
/// formatter would be wasteful. The router's existing telemetry path
/// uses raw seconds-since-epoch in many places — this helper keeps
/// the wire surface human-readable without proliferating timestamp
/// formats elsewhere.
fn format_rfc3339(t: std::time::SystemTime) -> String {
    let dur = match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d,
        Err(_) => return "1970-01-01T00:00:00Z".to_string(),
    };
    let secs = dur.as_secs() as i64;
    let nanos = dur.subsec_nanos();

    // Civil-time conversion via days-from-epoch using the algorithm
    // from Howard Hinnant's "date" library — deterministic, no
    // external deps, valid through year 9999.
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400) as u32;
    let (year, month, day) = days_to_ymd(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day / 60) % 60;
    let second = secs_of_day % 60;
    // Truncate to milliseconds — operator UX doesn't need
    // nanosecond precision and a shorter string is easier to scan.
    let millis = nanos / 1_000_000;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, month, day, hour, minute, second, millis
    )
}

/// Convert days since 1970-01-01 to civil (year, month, day).
/// Howard Hinnant's algorithm — public domain.
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    (y as i32, m as u32, d as u32)
}

/// `GET /internal/policy-status` — snapshot of every `PolicyKind`
/// the router has loaded into memory.
///
/// Returns `200 OK` with an empty `entries` array when no consumer
/// has run yet (e.g., AGT engine never reached
/// `load_policies_from_dir`). The controller treats that the same
/// way as "consumer not registered" — keeps the CRD in
/// `phase=Compiled`, never promotes to `Ready`.
async fn policy_status(State(state): State<AppState>) -> impl IntoResponse {
    let entries: Vec<EntryDto> = state
        .policy_status
        .snapshot()
        .into_iter()
        .map(EntryDto::from)
        .collect();
    let deployment_health = state.deployment_health.snapshot();
    Json(PolicyStatusResponse {
        schema_version: 1,
        count: entries.len(),
        entries,
        deployment_health,
    })
}

// ---------------------------------------------------------------------------
// Slice 5a — surfaced egress blocked buffer
//
// Operator-facing companion to the existing `/egress/learned/blocked`
// endpoint (which keeps its old shape for the
// `kars egress … --pending`/`--approve` workflow). The `/internal`
// variants are the canonical surface the `kars egress blocked` CLI
// and the headlamp plugin consume:
//
//   GET /internal/egress/blocked?since=<rfc3339>
//   GET /internal/egress/blocked/top?window=<go-duration>&n=<int>
//
// Both routes are mounted on the `protected` axum router (admin-token +
// `ADMIN_ALLOW_IPS` gated), and they read from the same in-process
// `BlockedBuffer` the forward-proxy enforcement path already populates.
// ---------------------------------------------------------------------------

/// Wire DTO for a single blocked-attempt entry. Mirrors
/// [`crate::egress_blocked::BlockedEntry`] but renames timestamp fields
/// to the suffix-stripped form preferred by the JSON wire surface and
/// adds RFC 3339 strings alongside the raw Unix seconds for human eyes
/// (controller / CLI parses both).
#[derive(Debug, Serialize)]
struct BlockedEntryDto {
    host: String,
    port: u16,
    source_sandbox: String,
    count: u32,
    first_seen_unix: u64,
    last_seen_unix: u64,
    first_seen: String,
    last_seen: String,
}

impl From<crate::egress_blocked::BlockedEntry> for BlockedEntryDto {
    fn from(e: crate::egress_blocked::BlockedEntry) -> Self {
        let first_seen = format_rfc3339_unix(e.first_seen_unix);
        let last_seen = format_rfc3339_unix(e.last_seen_unix);
        Self {
            host: e.host,
            port: e.port,
            source_sandbox: e.source_sandbox,
            count: e.count,
            first_seen_unix: e.first_seen_unix,
            last_seen_unix: e.last_seen_unix,
            first_seen,
            last_seen,
        }
    }
}

#[derive(Debug, Serialize)]
struct BlockedResponse {
    schema_version: u32,
    total: usize,
    count: usize,
    /// Echo of the resolved `since` filter as Unix seconds. `0` when the
    /// caller passed no filter or the input parsed as the epoch.
    since_unix: u64,
    entries: Vec<BlockedEntryDto>,
}

#[derive(Debug, Serialize)]
struct TopHost {
    host: String,
    count: u32,
}

#[derive(Debug, Serialize)]
struct BlockedTopResponse {
    schema_version: u32,
    /// Echo of the resolved window start as Unix seconds.
    since_unix: u64,
    /// Original window string from the request, normalized lowercase.
    window: String,
    n: usize,
    top: Vec<TopHost>,
}

/// `GET /internal/egress/blocked?since=<rfc3339>` — list of every blocked
/// host the router has observed whose `last_seen_unix >= since`. Newest
/// first. The buffer is bounded (default 1024 distinct keys) so this is
/// safe to call frequently from the CLI's `--watch` loop.
///
/// Missing or unparseable `since` collapses to `0` (return everything).
async fn egress_blocked(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let since_unix = params
        .get("since")
        .map(|s| s.as_str())
        .map(parse_since_or_zero)
        .unwrap_or(0);
    let entries: Vec<BlockedEntryDto> = state
        .blocked_egress
        .snapshot_since(since_unix)
        .into_iter()
        .map(BlockedEntryDto::from)
        .collect();
    Json(BlockedResponse {
        schema_version: 1,
        total: state.blocked_egress.len(),
        count: entries.len(),
        since_unix,
        entries,
    })
}

/// `GET /internal/egress/blocked/top?window=5m&n=10` — top-N
/// most-attempted blocked hosts in the rolling window. Aggregates across
/// source sandboxes + ports by hostname. Used by the plugin sidebar and
/// the rate-limited `EgressBlockedSeen` k8s event (planned for Slice 5b).
///
/// Accepted `window` formats: bare seconds (`300`), or Go-style duration
/// (`5m`, `1h`, `30s`). Default 5m. `n` defaults to 10, capped at 100.
async fn egress_blocked_top(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let window_raw = params
        .get("window")
        .cloned()
        .unwrap_or_else(|| "5m".to_string());
    let window_secs = parse_duration_secs(&window_raw).unwrap_or(300);
    let n: usize = params
        .get("n")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(10)
        .min(100);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let since_unix = now.saturating_sub(window_secs);
    let top: Vec<TopHost> = state
        .blocked_egress
        .top_hosts(since_unix, n)
        .into_iter()
        .map(|(host, count)| TopHost { host, count })
        .collect();
    Json(BlockedTopResponse {
        schema_version: 1,
        since_unix,
        window: window_raw.to_lowercase(),
        n,
        top,
    })
}

/// Parse a Go-style duration string into seconds. Accepts integer
/// seconds (`"300"`), or `Nm` / `Nh` / `Ns` (single-unit suffix). Returns
/// `None` on parse failure so the caller can supply a default.
///
/// Intentionally narrow — operators paste these strings into URLs; we
/// don't want surprises from compound forms like `1h30m`.
fn parse_duration_secs(raw: &str) -> Option<u64> {
    let s = raw.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    let (num, mul) = if let Some(rest) = s.strip_suffix("ms") {
        // milliseconds — round to a 1-second floor so we never accept
        // a window that would inflate to "everything" via underflow.
        (rest, 0u64)
    } else if let Some(rest) = s.strip_suffix('s') {
        (rest, 1)
    } else if let Some(rest) = s.strip_suffix('m') {
        (rest, 60)
    } else if let Some(rest) = s.strip_suffix('h') {
        (rest, 3_600)
    } else if let Some(rest) = s.strip_suffix('d') {
        (rest, 86_400)
    } else {
        return None;
    };
    let n: u64 = num.trim().parse().ok()?;
    Some(n.saturating_mul(mul).max(if mul == 0 { 0 } else { 1 }))
}

/// Parse the `since` query parameter. Accepts:
/// - integer Unix seconds (`"1705314645"`)
/// - RFC 3339 / ISO 8601 (`"2024-01-15T10:30:45Z"`)
/// - Go-style relative duration prefixed with `-` (`"-10m"`)
///
/// On failure → `0` (return everything). Loud failure isn't useful here
/// because the CLI does its own parsing; the router is the second line.
fn parse_since_or_zero(raw: &str) -> u64 {
    let s = raw.trim();
    if s.is_empty() {
        return 0;
    }
    if let Ok(n) = s.parse::<u64>() {
        return n;
    }
    // Relative `-Nm` form → "now minus duration".
    if let Some(stripped) = s.strip_prefix('-') {
        if let Some(secs) = parse_duration_secs(stripped) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            return now.saturating_sub(secs);
        }
    }
    parse_rfc3339_to_unix(s).unwrap_or(0)
}

/// RFC 3339 → Unix seconds. Hand-rolled to avoid pulling in `chrono`
/// just for the inverse of [`format_rfc3339`]. Accepts the
/// `YYYY-MM-DDTHH:MM:SS(.fff)?(Z|±HH:MM)` shape produced by the rest of
/// the router. Returns `None` on malformed input — callers fall back to
/// `0` (return-everything).
fn parse_rfc3339_to_unix(s: &str) -> Option<u64> {
    // Strip trailing `Z` or `+00:00`/`-00:00` offsets. We don't support
    // non-zero offsets — every callsite in the router emits UTC.
    let body = s
        .strip_suffix('Z')
        .or_else(|| s.strip_suffix("+00:00"))
        .or_else(|| s.strip_suffix("-00:00"))
        .unwrap_or(s);
    // Drop any subsecond suffix.
    let body = body.split('.').next()?;
    // Expect `YYYY-MM-DDTHH:MM:SS`.
    let (date, time) = body.split_once('T')?;
    let mut date_parts = date.split('-');
    let y: i32 = date_parts.next()?.parse().ok()?;
    let mo: u32 = date_parts.next()?.parse().ok()?;
    let d: u32 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let h: u32 = time_parts.next()?.parse().ok()?;
    let mi: u32 = time_parts.next()?.parse().ok()?;
    let se: u32 = time_parts.next()?.parse().ok()?;
    // Civil → days-from-epoch using the inverse of `days_to_ymd`.
    let yp = if mo <= 2 { y - 1 } else { y };
    let era = if yp >= 0 { yp } else { yp - 399 } / 400;
    let yoe = (yp - era * 400) as u64;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp as u64 + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era as i64 * 146_097 + doe as i64 - 719_468;
    let secs = days
        .checked_mul(86_400)?
        .checked_add(h as i64 * 3_600)?
        .checked_add(mi as i64 * 60)?
        .checked_add(se as i64)?;
    if secs < 0 {
        return None;
    }
    Some(secs as u64)
}

/// Render Unix seconds in the same `format_rfc3339` shape used by
/// `policy-status`. The blocked-buffer entries are stored as `u64`
/// epoch seconds (no nanosecond precision) so we always emit `.000`.
fn format_rfc3339_unix(unix_secs: u64) -> String {
    format_rfc3339(std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix_secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy_status::{PolicyKind, PolicyStatusRegistry};
    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn format_rfc3339_epoch_is_unix_zero() {
        assert_eq!(format_rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn format_rfc3339_known_timestamp() {
        // 2024-01-15T10:30:45.500Z = 1705314645 seconds + 500ms.
        let t = UNIX_EPOCH + Duration::from_millis(1_705_314_645_500);
        assert_eq!(format_rfc3339(t), "2024-01-15T10:30:45.500Z");
    }

    #[test]
    fn format_rfc3339_leap_year_feb_29() {
        // 2024-02-29T00:00:00Z.
        let t = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert_eq!(format_rfc3339(t), "2024-02-29T00:00:00.000Z");
    }

    #[test]
    fn entry_dto_serializes_with_camel_pascal_kind() {
        let reg = PolicyStatusRegistry::new();
        reg.record_success(PolicyKind::AgtProfile, "/etc/kars/policies", b"hello");
        let entry = reg.get(PolicyKind::AgtProfile).unwrap();
        let dto = EntryDto::from(entry);
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["kind"].as_str(), Some("AgtProfile"));
        assert_eq!(
            json["digest"].as_str(),
            Some("sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"),
        );
        assert_eq!(json["source_path"].as_str(), Some("/etc/kars/policies"));
        assert!(json["loaded_at"].as_str().unwrap().ends_with("Z"));
        assert!(json["last_error"].is_null());
    }

    #[test]
    fn policy_status_response_envelope_shape() {
        let reg = Arc::new(PolicyStatusRegistry::new());
        reg.record_success(PolicyKind::AgtProfile, "/x", b"y");
        let entries: Vec<EntryDto> = reg.snapshot().into_iter().map(EntryDto::from).collect();
        let resp = PolicyStatusResponse {
            schema_version: 1,
            count: entries.len(),
            entries,
            deployment_health: Vec::new(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["schema_version"].as_u64(), Some(1));
        assert_eq!(json["count"].as_u64(), Some(1));
        assert_eq!(json["entries"].as_array().unwrap().len(), 1);
        assert_eq!(json["entries"][0]["kind"].as_str(), Some("AgtProfile"));
        assert_eq!(json["deployment_health"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn empty_registry_yields_empty_entries_array() {
        let reg = PolicyStatusRegistry::new();
        let entries: Vec<EntryDto> = reg.snapshot().into_iter().map(EntryDto::from).collect();
        let resp = PolicyStatusResponse {
            schema_version: 1,
            count: entries.len(),
            entries,
            deployment_health: Vec::new(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["count"].as_u64(), Some(0));
        assert_eq!(json["entries"].as_array().unwrap().len(), 0);
        assert_eq!(json["deployment_health"].as_array().unwrap().len(), 0);
    }

    // ---- Slice 5a helpers ------------------------------------------------

    #[test]
    fn parse_duration_secs_bare_seconds() {
        assert_eq!(parse_duration_secs("300"), Some(300));
        assert_eq!(parse_duration_secs("0"), Some(0));
    }

    #[test]
    fn parse_duration_secs_suffixes() {
        assert_eq!(parse_duration_secs("5s"), Some(5));
        assert_eq!(parse_duration_secs("5m"), Some(300));
        assert_eq!(parse_duration_secs("1h"), Some(3_600));
        assert_eq!(parse_duration_secs("2d"), Some(172_800));
        // Case-insensitive.
        assert_eq!(parse_duration_secs("5M"), Some(300));
    }

    #[test]
    fn parse_duration_secs_invalid() {
        assert_eq!(parse_duration_secs(""), None);
        assert_eq!(parse_duration_secs("abc"), None);
        assert_eq!(parse_duration_secs("5y"), None);
        // Compound forms intentionally not supported.
        assert_eq!(parse_duration_secs("1h30m"), None);
    }

    #[test]
    fn parse_since_or_zero_unix_integer() {
        assert_eq!(parse_since_or_zero("1705314645"), 1_705_314_645);
        assert_eq!(parse_since_or_zero(""), 0);
        assert_eq!(parse_since_or_zero("garbage"), 0);
    }

    #[test]
    fn parse_since_or_zero_rfc3339_roundtrips_with_formatter() {
        // 2024-01-15T10:30:45Z = 1705314645.
        let n = parse_since_or_zero("2024-01-15T10:30:45Z");
        assert_eq!(n, 1_705_314_645);
        // Subseconds dropped (we round down to nearest second).
        let n2 = parse_since_or_zero("2024-01-15T10:30:45.500Z");
        assert_eq!(n2, 1_705_314_645);
    }

    #[test]
    fn parse_since_relative_minus_form() {
        // -1h should be roughly `now - 3600`. We can't assert exact value
        // without freezing the clock; just check it's monotonically less
        // than the current epoch and within 5s of the expected delta.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = parse_since_or_zero("-1h");
        let expected = now.saturating_sub(3600);
        assert!(result <= now);
        assert!(result.abs_diff(expected) <= 5);
    }

    #[test]
    fn format_rfc3339_unix_zero_is_epoch() {
        assert_eq!(format_rfc3339_unix(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn format_rfc3339_unix_known_timestamp() {
        // 2024-01-15T10:30:45Z
        assert_eq!(
            format_rfc3339_unix(1_705_314_645),
            "2024-01-15T10:30:45.000Z"
        );
    }
}
