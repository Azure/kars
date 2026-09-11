// kars Bridge — cluster-wide mission/team-run retention policy.
//
// Kars intentionally keeps mission/team-run records (KarsTask CRs) after
// delivery — they're the audit trail (deliverable, receipt, activity) a human
// reviews. Only the SANDBOX (live compute) auto-tears-down once delivery is
// terminal. Left unmanaged, the CR records accumulate forever (missions list
// grows unbounded, and on a small/kind cluster the CR count itself becomes
// noise). This mirrors Kubernetes' `Job.spec.ttlSecondsAfterFinished`: once a
// task's deliverable landed (`status.deliveredAt` stamped), the controller's
// retention reconciler deletes it once the effective TTL elapses.
//
// Effective TTL per task = that task's own `spec.retentionTtlSeconds`
// override (set at creation), else this cluster-wide default. `0`/absent
// means "never auto-delete" — the safe, backward-compatible default. The
// principal + roster members of a standing team are NEVER auto-deleted
// (the controller pins them to `0` unconditionally) — only individual
// missions and team RUN records are eligible.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::kars::cluster::Cluster;
use crate::state::AppState;

fn require_cluster(state: &AppState) -> AppResult<&Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

#[derive(Debug, Serialize)]
pub struct RetentionPolicyDto {
    /// Effective cluster-wide default, in seconds. `0` = disabled (never
    /// auto-delete unless a mission/team sets its own override).
    pub default_ttl_seconds: i64,
    /// Human-readable summary of what's currently in effect, for the console.
    pub summary: String,
}

fn summarize(ttl: i64) -> String {
    if ttl <= 0 {
        "Disabled — delivered missions and team runs are kept until a human deletes them (the default).".to_string()
    } else {
        let hours = ttl as f64 / 3600.0;
        if hours >= 1.0 && (ttl % 3600) == 0 {
            format!(
                "Delivered missions and team runs auto-delete {hours:.0}h after their deliverable lands (unless a mission overrides its own retention)."
            )
        } else {
            format!(
                "Delivered missions and team runs auto-delete {ttl}s after their deliverable lands (unless a mission overrides its own retention)."
            )
        }
    }
}

/// `GET /api/operator/retention-policy`.
pub async fn get_retention_policy(
    State(state): State<AppState>,
) -> AppResult<Json<RetentionPolicyDto>> {
    let cluster = require_cluster(&state)?;
    let ttl = cluster.read_retention_policy().await;
    Ok(Json(RetentionPolicyDto {
        default_ttl_seconds: ttl,
        summary: summarize(ttl),
    }))
}

#[derive(Debug, Deserialize)]
pub struct SetRetentionPolicyRequest {
    /// Seconds; `0` disables cluster-wide auto-delete. Must be `>= 0`.
    pub default_ttl_seconds: i64,
}

/// `PUT /api/operator/retention-policy` — admin-only (gated at the web-proxy
/// layer, same pattern as inference budgets).
pub async fn set_retention_policy(
    State(state): State<AppState>,
    Json(body): Json<SetRetentionPolicyRequest>,
) -> AppResult<Json<RetentionPolicyDto>> {
    let cluster = require_cluster(&state)?;
    if body.default_ttl_seconds < 0 {
        return Err(AppError::BadRequest(
            "default_ttl_seconds must be >= 0".into(),
        ));
    }
    cluster
        .write_retention_policy(body.default_ttl_seconds)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    Ok(Json(RetentionPolicyDto {
        default_ttl_seconds: body.default_ttl_seconds,
        summary: summarize(body.default_ttl_seconds),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_summary_is_honest() {
        assert!(summarize(0).contains("Disabled"));
    }

    #[test]
    fn hour_aligned_ttl_reads_in_hours() {
        assert!(summarize(86400).contains("24h"));
    }

    #[test]
    fn non_aligned_ttl_reads_in_seconds() {
        assert!(summarize(90).contains("90s"));
    }
}
