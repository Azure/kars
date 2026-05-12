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
//! `azureclaw inspect`, headlamp plugin) match on field names. Don't
//! rename fields without a deprecation window.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use serde::Serialize;

use super::AppState;
use crate::policy_status::PolicyStatusEntry;

pub fn internal_routes() -> Router<AppState> {
    Router::new().route("/internal/policy-status", get(policy_status))
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
    Json(PolicyStatusResponse {
        schema_version: 1,
        count: entries.len(),
        entries,
    })
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
        reg.record_success(PolicyKind::AgtProfile, "/etc/azureclaw/policies", b"hello");
        let entry = reg.get(PolicyKind::AgtProfile).unwrap();
        let dto = EntryDto::from(entry);
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["kind"].as_str(), Some("AgtProfile"));
        assert_eq!(
            json["digest"].as_str(),
            Some("sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"),
        );
        assert_eq!(
            json["source_path"].as_str(),
            Some("/etc/azureclaw/policies")
        );
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
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["schema_version"].as_u64(), Some(1));
        assert_eq!(json["count"].as_u64(), Some(1));
        assert_eq!(json["entries"].as_array().unwrap().len(), 1);
        assert_eq!(json["entries"][0]["kind"].as_str(), Some("AgtProfile"));
    }

    #[test]
    fn empty_registry_yields_empty_entries_array() {
        let reg = PolicyStatusRegistry::new();
        let entries: Vec<EntryDto> = reg.snapshot().into_iter().map(EntryDto::from).collect();
        let resp = PolicyStatusResponse {
            schema_version: 1,
            count: entries.len(),
            entries,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["count"].as_u64(), Some(0));
        assert_eq!(json["entries"].as_array().unwrap().len(), 0);
    }
}
