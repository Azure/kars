use actix_web::{web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{info, warn, error};
use chrono::{Utc, Duration};
use uuid::Uuid;

use crate::auth;
use crate::AppState;

/// Helper to return 503 Service Unavailable during startup
fn service_unavailable() -> HttpResponse {
    HttpResponse::ServiceUnavailable()
        .content_type("application/json")
        .body(r#"{"error":"Service is starting up","status":"starting"}"#)
}

/// Reputation feedback entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReputationFeedback {
    pub id: Uuid,
    pub target_amid: String,
    pub from_amid: String,
    pub session_id: String,
    pub score: f64,
    pub tags: Vec<String>,
    pub from_tier: String,
    pub created_at: chrono::DateTime<Utc>,
}

/// Completed session record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedSession {
    pub id: Uuid,
    pub session_id: String,
    pub initiator_amid: String,
    pub receiver_amid: String,
    pub intent: String,
    pub outcome: SessionOutcome,
    pub started_at: chrono::DateTime<Utc>,
    pub completed_at: chrono::DateTime<Utc>,
}

/// Session outcome
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionOutcome {
    Success,
    Failed,
    Timeout,
    Rejected,
    Cancelled,
}

impl std::fmt::Display for SessionOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionOutcome::Success => write!(f, "success"),
            SessionOutcome::Failed => write!(f, "failed"),
            SessionOutcome::Timeout => write!(f, "timeout"),
            SessionOutcome::Rejected => write!(f, "rejected"),
            SessionOutcome::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Request to record session completion
#[derive(Debug, Deserialize)]
pub struct RecordSessionRequest {
    pub session_id: String,
    pub initiator_amid: String,
    pub receiver_amid: String,
    pub intent: String,
    pub outcome: SessionOutcome,
    pub started_at: chrono::DateTime<Utc>,
    pub reporter_amid: String,
    pub signature: String,
    pub timestamp: String,
}

/// Request to submit feedback
#[derive(Debug, Deserialize)]
pub struct SubmitFeedbackRequest {
    pub target_amid: String,
    pub session_id: String,
    pub score: f64,
    pub tags: Option<Vec<String>>,
    pub from_amid: String,
    pub signature: String,
    pub timestamp: String,
}

/// Reputation score response
#[derive(Debug, Serialize)]
pub struct ReputationScore {
    pub amid: String,
    pub score: f64,
    pub completion_rate: f64,
    pub total_sessions: i64,
    pub successful_sessions: i64,
    pub feedback_count: i64,
    pub average_feedback: f64,
    pub tier: String,
    pub age_days: i64,
    pub tags: Vec<TagAggregate>,
    pub last_updated: chrono::DateTime<Utc>,
}

/// Aggregated tag count
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagAggregate {
    pub tag: String,
    pub count: i64,
}

/// Reputation formula constants
const COMPLETION_WEIGHT: f64 = 0.3;
const FEEDBACK_WEIGHT: f64 = 0.4;
const AGE_WEIGHT: f64 = 0.1;
const TIER_WEIGHT: f64 = 0.2;

// Tier bonuses
const TIER_ANONYMOUS_BONUS: f64 = 0.0;
const TIER_VERIFIED_BONUS: f64 = 0.1;
const TIER_ORGANIZATION_BONUS: f64 = 0.2;

// Feedback weight discounts
const TIER_2_FEEDBACK_DISCOUNT: f64 = 0.5;  // 50% weight for anonymous feedback
const MUTUAL_ONLY_DISCOUNT: f64 = 0.8;  // 80% discount if only mutual ratings

// Minimum ratings for ranking
const MIN_RATINGS_FOR_RANKING: i64 = 5;

/// Calculate reputation score for an agent
pub async fn calculate_reputation(
    state: web::Data<Arc<AppState>>,
    query: web::Query<AmidQuery>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    match calculate_reputation_score(pool, &query.amid).await {
        Ok(score) => HttpResponse::Ok().json(score),
        Err(e) => {
            error!("Reputation calculation error: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to calculate reputation"
            }))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AmidQuery {
    pub amid: String,
}

/// Submit reputation feedback
pub async fn submit_feedback(
    state: web::Data<Arc<AppState>>,
    req: web::Json<SubmitFeedbackRequest>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    // Validate score
    if req.score < 0.0 || req.score > 1.0 {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "Score must be between 0.0 and 1.0"
        }));
    }

    // Look up sender to verify and get tier
    let sender = match get_agent(pool, &req.from_amid).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            return HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "Sender not found"
            }));
        }
        Err(e) => {
            error!("Database error: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Internal error"
            }));
        }
    };

    // Verify signature
    if let Err(auth_err) = auth::verify_update_signature(
        &sender.signing_public_key,
         &req.timestamp,
        &req.signature,
    ) {
        warn!("Feedback signature failed: {:?}", auth_err);
        return HttpResponse::Unauthorized().json(serde_json::json!({
            "error": format!("Signature verification failed: {}", auth_err)
        }));
    }

    // Verify target exists
    if get_agent(pool, &req.target_amid).await.unwrap_or(None).is_none() {
        return HttpResponse::NotFound().json(serde_json::json!({
            "error": "Target agent not found"
        }));
    }

    // Check for duplicate feedback on same session
    if has_existing_feedback(pool, &req.from_amid, &req.session_id).await.unwrap_or(false) {
        return HttpResponse::Conflict().json(serde_json::json!({
            "error": "Feedback already submitted for this session"
        }));
    }

    // Detect rapid change (5+ feedbacks in 1 hour from same source)
    if let Ok(recent_count) = get_recent_feedback_count(pool, &req.from_amid, &req.target_amid).await {
        if recent_count >= 5 {
            warn!("Rapid feedback detected: {} -> {}", req.from_amid, req.target_amid);
            // Still record but flag for review
        }
    }

    // Store feedback
    let feedback_id = Uuid::new_v4();
    let tags = req.tags.clone().unwrap_or_default();

    match store_feedback(
        pool,
        feedback_id,
        &req.target_amid,
        &req.from_amid,
        &req.session_id,
        req.score,
        &tags,
        &sender.tier,
    ).await {
        Ok(_) => {
            // Update cached reputation score
            if let Err(e) = update_cached_reputation(pool, &req.target_amid).await {
                warn!("Failed to update cached reputation: {}", e);
            }

            info!("Feedback recorded: {} -> {} = {}", req.from_amid, req.target_amid, req.score);
            HttpResponse::Ok().json(serde_json::json!({
                "success": true,
                "feedback_id": feedback_id
            }))
        }
        Err(e) => {
            error!("Failed to store feedback: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to store feedback"
            }))
        }
    }
}

/// Record a completed session
pub async fn record_session(
    state: web::Data<Arc<AppState>>,
    req: web::Json<RecordSessionRequest>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    // Verify reporter is one of the participants
    if req.reporter_amid != req.initiator_amid && req.reporter_amid != req.receiver_amid {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "Only session participants can record completion"
        }));
    }

    // Look up reporter
    let reporter = match get_agent(pool, &req.reporter_amid).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            return HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "Reporter not found"
            }));
        }
        Err(e) => {
            error!("Database error: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Internal error"
            }));
        }
    };

    // Verify signature
    if let Err(auth_err) = auth::verify_update_signature(
        &reporter.signing_public_key,
         &req.timestamp,
        &req.signature,
    ) {
        warn!("Session record signature failed: {:?}", auth_err);
        return HttpResponse::Unauthorized().json(serde_json::json!({
            "error": format!("Signature verification failed: {}", auth_err)
        }));
    }

    // Store session record
    let session_record_id = Uuid::new_v4();
    match store_completed_session(
        pool,
        session_record_id,
        &req.session_id,
        &req.initiator_amid,
        &req.receiver_amid,
        &req.intent,
        &req.outcome.to_string(),
        req.started_at,
    ).await {
        Ok(_) => {
            info!("Session recorded: {} ({:?})", req.session_id, req.outcome);
            HttpResponse::Ok().json(serde_json::json!({
                "success": true,
                "session_record_id": session_record_id
            }))
        }
        Err(e) => {
            error!("Failed to store session: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to store session"
            }))
        }
    }
}

/// Get top agents by reputation
pub async fn leaderboard(
    state: web::Data<Arc<AppState>>,
    query: web::Query<LeaderboardQuery>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    let limit = query.limit.unwrap_or(20).min(100);
    let intent = query.intent.as_deref();

    match get_reputation_leaderboard(pool, limit, intent).await {
        Ok(entries) => HttpResponse::Ok().json(serde_json::json!({
            "leaderboard": entries,
            "limit": limit,
            "intent_filter": intent
        })),
        Err(e) => {
            error!("Leaderboard error: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to get leaderboard"
            }))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LeaderboardQuery {
    pub limit: Option<i64>,
    pub intent: Option<String>,
}

// Core reputation calculation

async fn calculate_reputation_score(
    pool: &PgPool,
    amid: &str,
) -> Result<ReputationScore, String> {
    // Get agent info
    let agent = get_agent(pool, amid).await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Agent not found".to_string())?;

    // Get session statistics
    let (total_sessions, successful_sessions) = get_session_stats(pool, amid).await
        .map_err(|e| e.to_string())?;

    // Get feedback statistics
    let (feedback_count, average_feedback, weighted_feedback) =
        get_feedback_stats(pool, amid).await.map_err(|e| e.to_string())?;

    // Get tag aggregates
    let tags = get_tag_aggregates(pool, amid).await.map_err(|e| e.to_string())?;

    // Calculate completion rate
    let completion_rate = if total_sessions > 0 {
        successful_sessions as f64 / total_sessions as f64
    } else {
        0.5 // Default for new agents
    };

    // Calculate age factor (days/365, capped at 1.0)
    let age_days = Utc::now().signed_duration_since(agent.created_at).num_days();
    let age_factor = (age_days as f64 / 365.0).min(1.0);

    // Calculate tier bonus
    let tier_bonus = match agent.tier.as_str() {
        "anonymous" => TIER_ANONYMOUS_BONUS,
        "verified" => TIER_VERIFIED_BONUS,
        "organization" => TIER_ORGANIZATION_BONUS,
        _ => TIER_ANONYMOUS_BONUS,
    };

    // Apply reputation formula
    let score = if feedback_count < MIN_RATINGS_FOR_RANKING {
        // Not enough ratings, use baseline
        0.5 + tier_bonus
    } else {
        // Full formula
        (COMPLETION_WEIGHT * completion_rate) +
        (FEEDBACK_WEIGHT * weighted_feedback) +
        (AGE_WEIGHT * age_factor) +
        (TIER_WEIGHT * (0.5 + tier_bonus))
    };

    // Clamp to 0.0 - 1.0
    let final_score = score.max(0.0).min(1.0);

    Ok(ReputationScore {
        amid: amid.to_string(),
        score: final_score,
        completion_rate,
        total_sessions,
        successful_sessions,
        feedback_count,
        average_feedback,
        tier: agent.tier,
        age_days,
        tags,
        last_updated: Utc::now(),
    })
}

// Database functions

#[derive(Debug, Clone)]
struct AgentRecord {
    signing_public_key: String,
    tier: String,
    created_at: chrono::DateTime<Utc>,
}

async fn get_agent(pool: &PgPool, amid: &str) -> Result<Option<AgentRecord>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT signing_public_key, tier::text as "tier!", created_at FROM agents WHERE amid = $1"#,
        amid
    )
    .fetch_optional(pool)
    .await?;

    Ok(match row {
        Some(r) => Some(AgentRecord {
            signing_public_key: r.signing_public_key,
            tier: r.tier,
            created_at: r.created_at,
        }),
        None => None,
    })
}

async fn get_session_stats(pool: &PgPool, amid: &str) -> Result<(i64, i64), sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT
            COUNT(*) as total,
            COUNT(*) FILTER (WHERE outcome = 'success') as successful
        FROM completed_sessions
        WHERE initiator_amid = $1 OR receiver_amid = $1
        "#,
        amid
    )
    .fetch_one(pool)
    .await?;

    Ok((row.total.unwrap_or(0), row.successful.unwrap_or(0)))
}

async fn get_feedback_stats(pool: &PgPool, amid: &str) -> Result<(i64, f64, f64), sqlx::Error> {
    // Get all feedback with tier information for weighting
    let query_rows = sqlx::query!(
        r#"
        SELECT score, from_tier
        FROM reputation_feedbacks
        WHERE target_amid = $1
        "#,
        amid
    )
    .fetch_all(pool)
    .await?;

    let mut feedback_data: Vec<(f64, String)> = Vec::new();
    for r in query_rows {
        feedback_data.push((r.score, r.from_tier.clone()));
    }

    if feedback_data.is_empty() {
        return Ok((0, 0.5, 0.5));
    }

    let count = feedback_data.len() as i64;
    let mut total_score = 0.0;
    let mut weighted_score = 0.0;
    let mut total_weight = 0.0;

    for (score, from_tier) in &feedback_data {
        total_score += score;

        // Apply tier-based weight discount
        let weight = match from_tier.as_str() {
            "anonymous" => TIER_2_FEEDBACK_DISCOUNT,
            _ => 1.0,
        };

        weighted_score += score * weight;
        total_weight += weight;
    }

    let average = total_score / count as f64;
    let weighted_average = if total_weight > 0.0 {
        weighted_score / total_weight
    } else {
        average
    };

    Ok((count, average, weighted_average))
}

async fn get_tag_aggregates(pool: &PgPool, amid: &str) -> Result<Vec<TagAggregate>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT tag, COUNT(*) as count
        FROM reputation_feedbacks, UNNEST(tags) as tag
        WHERE target_amid = $1
        GROUP BY tag
        ORDER BY count DESC
        LIMIT 10
        "#,
        amid
    )
    .fetch_all(pool)
    .await?;

    let mut result = Vec::new();
    for r in rows {
        result.push(TagAggregate {
            tag: r.tag.unwrap_or_default(),
            count: r.count.unwrap_or(0),
        });
    }
    Ok(result)
}

async fn has_existing_feedback(pool: &PgPool, from_amid: &str, session_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"SELECT 1 as exists FROM reputation_feedbacks WHERE from_amid = $1 AND session_id = $2"#,
        from_amid,
        session_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(matches!(result, Some(_)))
}

async fn get_recent_feedback_count(pool: &PgPool, from_amid: &str, target_amid: &str) -> Result<i64, sqlx::Error> {
    let one_hour_ago = Utc::now() - Duration::hours(1);
    let row = sqlx::query!(
        r#"
        SELECT COUNT(*) as count
        FROM reputation_feedbacks
        WHERE from_amid = $1 AND target_amid = $2 AND created_at > $3
        "#,
        from_amid,
        target_amid,
        one_hour_ago
    )
    .fetch_one(pool)
    .await?;

    Ok(row.count.unwrap_or(0))
}

async fn store_feedback(
    pool: &PgPool,
    id: Uuid,
    target_amid: &str,
    from_amid: &str,
    session_id: &str,
    score: f64,
    tags: &[String],
    from_tier: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO reputation_feedbacks (id, target_amid, from_amid, session_id, score, tags, from_tier, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())
        "#,
        id,
        target_amid,
        from_amid,
        session_id,
        score,
        tags,
        from_tier
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn store_completed_session(
    pool: &PgPool,
    id: Uuid,
    session_id: &str,
    initiator_amid: &str,
    receiver_amid: &str,
    intent: &str,
    outcome: &str,
    started_at: chrono::DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO completed_sessions (id, session_id, initiator_amid, receiver_amid, intent, outcome, started_at, completed_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())
        ON CONFLICT (session_id) DO NOTHING
        "#,
        id,
        session_id,
        initiator_amid,
        receiver_amid,
        intent,
        outcome,
        started_at
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn update_cached_reputation(pool: &PgPool, amid: &str) -> Result<(), sqlx::Error> {
    // Calculate new score
    if let Ok(score) = calculate_reputation_score(pool, amid).await {
        // Update the agents table
        sqlx::query!(
            r#"UPDATE agents SET reputation_score = $1, updated_at = NOW() WHERE amid = $2"#,
            score.score as f32,
            amid
        )
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn get_reputation_leaderboard(
    pool: &PgPool,
    limit: i64,
    intent: Option<&str>,
) -> Result<Vec<ReputationScore>, sqlx::Error> {
    // Get top agents by reputation score that have minimum ratings
    let rows = sqlx::query!(
        r#"
        SELECT a.amid, a.tier::text as "tier!", a.reputation_score, a.created_at,
               (SELECT COUNT(*) FROM reputation_feedbacks rf WHERE rf.target_amid = a.amid) as feedback_count
        FROM agents a
        WHERE a.reputation_score > 0
          AND (SELECT COUNT(*) FROM reputation_feedbacks rf WHERE rf.target_amid = a.amid) >= $2
        ORDER BY a.reputation_score DESC
        LIMIT $1
        "#,
        limit,
        MIN_RATINGS_FOR_RANKING
    )
    .fetch_all(pool)
    .await?;

    let mut results = Vec::new();
    for row in rows {
        let (total, successful) = get_session_stats(pool, &row.amid).await.unwrap_or((0, 0));
        let tags = get_tag_aggregates(pool, &row.amid).await.unwrap_or_default();
        let age_days = Utc::now().signed_duration_since(row.created_at).num_days();

        results.push(ReputationScore {
            amid: row.amid,
            score: row.reputation_score as f64,
            completion_rate: if total > 0 { successful as f64 / total as f64 } else { 0.5 },
            total_sessions: total,
            successful_sessions: successful,
            feedback_count: row.feedback_count.unwrap_or(0),
            average_feedback: row.reputation_score as f64, // Approximation
            tier: row.tier,
            age_days,
            tags,
            last_updated: Utc::now(),
        });
    }

    Ok(results)
}
