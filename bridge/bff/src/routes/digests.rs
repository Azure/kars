// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
    pub tokens_spent: Option<i64>,
    pub tokens_spent_scope: Option<String>,
    pub knowledge_entries: i64,
    /// Verified reporting channel (team→recipient edge) this report flows on.
    pub channel: Option<String>,
    pub gated: bool,
}

fn token_usage(value: &serde_json::Value) -> (Option<i64>, Option<String>) {
    // Historical digests predate lifetime observations and contain root synthesis only.
    let scope = value
        .get("tokens_spent_scope")
        .and_then(|scope| scope.as_str())
        .unwrap_or("RetainedRootSynthesis");
    if !matches!(scope, "GovernedInferenceLifetime" | "RetainedRootSynthesis") {
        return (None, None);
    }
    (
        value
            .get("tokens_spent")
            .and_then(|tokens| tokens.as_i64())
            .filter(|tokens| *tokens >= 0),
        Some(scope.into()),
    )
}

/// `GET /api/digests` — the cross-team digest stream, newest first.
pub async fn list_digests(State(state): State<AppState>) -> AppResult<Json<Vec<DigestDto>>> {
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;
    let raw = cluster.list_team_digests().await;
    let digests = raw
        .into_iter()
        .filter_map(|v| {
            let (tokens_spent, tokens_spent_scope) = token_usage(&v);
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
                tokens_spent,
                tokens_spent_scope,
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

#[cfg(test)]
mod tests {
    use super::token_usage;
    use serde_json::json;

    #[test]
    fn token_usage_preserves_unavailable_and_scopes_historical_synthesis() {
        assert_eq!(
            token_usage(&json!({"tokens_spent": 37488})),
            (Some(37488), Some("RetainedRootSynthesis".into()))
        );
        for tokens in [json!(86793), json!(0), json!(null), json!(-1)] {
            let value =
                json!({"tokens_spent": tokens, "tokens_spent_scope":"GovernedInferenceLifetime"});
            assert_eq!(
                token_usage(&value),
                (
                    tokens.as_i64().filter(|tokens| *tokens >= 0),
                    Some("GovernedInferenceLifetime".into())
                )
            );
        }
        assert_eq!(token_usage(&json!({})).0, None);
        assert_eq!(
            token_usage(&json!({"tokens_spent": 37488, "tokens_spent_scope":"unknown"})),
            (None, None)
        );
    }
}
