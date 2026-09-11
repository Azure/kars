// kars Bridge BFF — team digests (design note §20). The standing-operation
// report stream that surfaces in the steering inbox: each team publishes a
// periodic digest (runs/delivered/tokens/knowledge/health), and the inbox shows
// them alongside the decision queue so the operator gets the autonomous-
// monitoring report in one place.

use axum::{Json, extract::State};
use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct DigestDto {
    pub team: String,
    pub at: String,
    pub reporting_to: Option<String>,
    pub health: String,
    pub summary: String,
    pub runs_generated: i64,
    pub runs_delivered: i64,
    pub tokens_spent: i64,
    pub knowledge_entries: i64,
    /// Verified reporting channel (team→recipient edge) this report flows on.
    pub channel: Option<String>,
    pub gated: bool,
}

/// `GET /api/digests` — the cross-team digest stream, newest first.
pub async fn list_digests(State(state): State<AppState>) -> AppResult<Json<Vec<DigestDto>>> {
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;
    let raw = cluster.list_team_digests().await;
    let digests = raw
        .into_iter()
        .filter_map(|v| {
            Some(DigestDto {
                team: v.get("team")?.as_str()?.to_string(),
                at: v.get("at")?.as_str()?.to_string(),
                reporting_to: v
                    .get("reporting_to")
                    .and_then(|x| x.as_str())
                    .map(str::to_string),
                health: v
                    .get("health")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                summary: v
                    .get("summary")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                runs_generated: v
                    .get("runs_generated")
                    .and_then(|x| x.as_i64())
                    .unwrap_or(0),
                runs_delivered: v
                    .get("runs_delivered")
                    .and_then(|x| x.as_i64())
                    .unwrap_or(0),
                tokens_spent: v.get("tokens_spent").and_then(|x| x.as_i64()).unwrap_or(0),
                knowledge_entries: v
                    .get("knowledge_entries")
                    .and_then(|x| x.as_i64())
                    .unwrap_or(0),
                channel: v
                    .get("channel")
                    .and_then(|x| x.as_str())
                    .map(str::to_string),
                gated: v.get("gated").and_then(|x| x.as_bool()).unwrap_or(false),
            })
        })
        .collect();
    Ok(Json(digests))
}
