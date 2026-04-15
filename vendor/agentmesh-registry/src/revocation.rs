use actix_web::{web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{info, warn, error};
use chrono::Utc;
use uuid::Uuid;

use crate::auth;
use crate::AppState;

/// Helper to return 503 Service Unavailable during startup
fn service_unavailable() -> HttpResponse {
    HttpResponse::ServiceUnavailable()
        .content_type("application/json")
        .body(r#"{"error":"Service is starting up","status":"starting"}"#)
}

/// Revocation reasons
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RevocationReason {
    KeyCompromise,
    PolicyViolation,
    Superseded,
    CessationOfOperation,
    PrivilegeWithdrawn,
    AdminRequest,
    Other,
}

impl std::fmt::Display for RevocationReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RevocationReason::KeyCompromise => write!(f, "key_compromise"),
            RevocationReason::PolicyViolation => write!(f, "policy_violation"),
            RevocationReason::Superseded => write!(f, "superseded"),
            RevocationReason::CessationOfOperation => write!(f, "cessation_of_operation"),
            RevocationReason::PrivilegeWithdrawn => write!(f, "privilege_withdrawn"),
            RevocationReason::AdminRequest => write!(f, "admin_request"),
            RevocationReason::Other => write!(f, "other"),
        }
    }
}

/// Revocation entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevocationEntry {
    pub id: Uuid,
    pub amid: String,
    pub reason: String,
    pub revoked_by: String,
    pub revoked_at: chrono::DateTime<Utc>,
    pub notes: Option<String>,
}

/// Request to revoke an agent
#[derive(Debug, Deserialize)]
pub struct RevokeRequest {
    pub amid: String,
    pub reason: RevocationReason,
    pub notes: Option<String>,
    pub revoker_amid: String,
    pub signature: String,
    pub timestamp: String,
}

/// Response for revocation
#[derive(Debug, Serialize)]
pub struct RevokeResponse {
    pub success: bool,
    pub revocation_id: Option<Uuid>,
    pub error: Option<String>,
}

/// Revocation status response
#[derive(Debug, Serialize)]
pub struct RevocationStatus {
    pub amid: String,
    pub revoked: bool,
    pub revocation: Option<RevocationEntry>,
}

/// Bulk revocation check request
#[derive(Debug, Deserialize)]
pub struct BulkCheckRequest {
    pub amids: Vec<String>,
}

/// Bulk revocation check response
#[derive(Debug, Serialize)]
pub struct BulkCheckResponse {
    pub revocations: Vec<RevocationStatus>,
    pub total_checked: usize,
    pub total_revoked: usize,
}

/// Revocation list response (for caching)
#[derive(Debug, Serialize)]
pub struct RevocationListResponse {
    pub revocations: Vec<RevocationEntry>,
    pub total: i64,
    pub last_updated: chrono::DateTime<Utc>,
    pub next_update: chrono::DateTime<Utc>,
}

/// Revoke an agent's certificate
pub async fn revoke_agent(
    state: web::Data<Arc<AppState>>,
    req: web::Json<RevokeRequest>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    info!("Revocation request for {} by {}", req.amid, req.revoker_amid);

    // Look up the revoker to verify they have permission
    let revoker = match get_agent_by_amid(pool, &req.revoker_amid).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            return HttpResponse::Unauthorized().json(RevokeResponse {
                success: false,
                revocation_id: None,
                error: Some("Revoker not found".to_string()),
            });
        }
        Err(e) => {
            error!("Database error: {}", e);
            return HttpResponse::InternalServerError().json(RevokeResponse {
                success: false,
                revocation_id: None,
                error: Some("Internal error".to_string()),
            });
        }
    };

    // Verify signature
    if let Err(auth_err) = auth::verify_update_signature(
        &revoker.signing_public_key,
         &req.timestamp,
        &req.signature,
    ) {
        warn!("Revocation signature failed: {:?}", auth_err);
        return HttpResponse::Unauthorized().json(RevokeResponse {
            success: false,
            revocation_id: None,
            error: Some(format!("Signature verification failed: {}", auth_err)),
        });
    }

    // Check if already revoked
    if let Ok(Some(_)) = get_revocation(pool, &req.amid).await {
        return HttpResponse::Conflict().json(RevokeResponse {
            success: false,
            revocation_id: None,
            error: Some("Agent already revoked".to_string()),
        });
    }

    // Permission check: agent can revoke themselves, or org admin can revoke org agents
    let target = match get_agent_by_amid(pool, &req.amid).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            return HttpResponse::NotFound().json(RevokeResponse {
                success: false,
                revocation_id: None,
                error: Some("Target agent not found".to_string()),
            });
        }
        Err(e) => {
            error!("Database error: {}", e);
            return HttpResponse::InternalServerError().json(RevokeResponse {
                success: false,
                revocation_id: None,
                error: Some("Internal error".to_string()),
            });
        }
    };

    let can_revoke = if req.revoker_amid == req.amid {
        // Self-revocation always allowed
        true
    } else if let (Some(revoker_org), Some(target_org)) = (revoker.organization_id, target.organization_id) {
        // Check if revoker is org admin for target's org
        revoker_org == target_org && is_org_admin(pool, revoker_org, &req.revoker_amid).await.unwrap_or(false)
    } else {
        false
    };

    if !can_revoke {
        return HttpResponse::Forbidden().json(RevokeResponse {
            success: false,
            revocation_id: None,
            error: Some("Not authorized to revoke this agent".to_string()),
        });
    }

    // Create revocation entry
    let revocation_id = Uuid::new_v4();
    match create_revocation(
        pool,
        revocation_id,
        &req.amid,
        &req.reason.to_string(),
        &req.revoker_amid,
        req.notes.as_deref(),
    ).await {
        Ok(_) => {
            info!("Agent {} revoked by {} (reason: {})", req.amid, req.revoker_amid, req.reason);
            HttpResponse::Ok().json(RevokeResponse {
                success: true,
                revocation_id: Some(revocation_id),
                error: None,
            })
        }
        Err(e) => {
            error!("Failed to create revocation: {}", e);
            HttpResponse::InternalServerError().json(RevokeResponse {
                success: false,
                revocation_id: None,
                error: Some("Failed to create revocation".to_string()),
            })
        }
    }
}

/// Check revocation status of a single agent
pub async fn check_revocation(
    state: web::Data<Arc<AppState>>,
    query: web::Query<AmidQuery>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    match get_revocation(pool, &query.amid).await {
        Ok(Some(rev)) => {
            HttpResponse::Ok().json(RevocationStatus {
                amid: query.amid.clone(),
                revoked: true,
                revocation: Some(rev),
            })
        }
        Ok(None) => {
            HttpResponse::Ok().json(RevocationStatus {
                amid: query.amid.clone(),
                revoked: false,
                revocation: None,
            })
        }
        Err(e) => {
            error!("Database error: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Internal error"
            }))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AmidQuery {
    pub amid: String,
}

/// Bulk check revocation status
pub async fn bulk_check_revocation(
    state: web::Data<Arc<AppState>>,
    req: web::Json<BulkCheckRequest>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    if req.amids.len() > 100 {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "Maximum 100 AMIDs per request"
        }));
    }

    let mut results = Vec::new();
    let mut total_revoked = 0;

    for amid in &req.amids {
        match get_revocation(pool, amid).await {
            Ok(Some(rev)) => {
                total_revoked += 1;
                results.push(RevocationStatus {
                    amid: amid.clone(),
                    revoked: true,
                    revocation: Some(rev),
                });
            }
            Ok(None) => {
                results.push(RevocationStatus {
                    amid: amid.clone(),
                    revoked: false,
                    revocation: None,
                });
            }
            Err(e) => {
                error!("Database error for {}: {}", amid, e);
                results.push(RevocationStatus {
                    amid: amid.clone(),
                    revoked: false,
                    revocation: None,
                });
            }
        }
    }

    HttpResponse::Ok().json(BulkCheckResponse {
        revocations: results,
        total_checked: req.amids.len(),
        total_revoked,
    })
}

/// Get full revocation list (for CRL caching)
pub async fn get_revocation_list(
    state: web::Data<Arc<AppState>>,
    query: web::Query<ListQuery>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    let limit = query.limit.unwrap_or(1000).min(10000);
    let offset = query.offset.unwrap_or(0);

    match get_all_revocations(pool, limit, offset).await {
        Ok((revocations, total)) => {
            let now = Utc::now();
            HttpResponse::Ok().json(RevocationListResponse {
                revocations,
                total,
                last_updated: now,
                next_update: now + chrono::Duration::hours(1),
            })
        }
        Err(e) => {
            error!("Database error: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Internal error"
            }))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

// Database functions

#[derive(Debug, Clone)]
struct AgentRecord {
    signing_public_key: String,
    organization_id: Option<Uuid>,
}

async fn get_agent_by_amid(pool: &PgPool, amid: &str) -> Result<Option<AgentRecord>, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT signing_public_key, organization_id FROM agents WHERE amid = $1"#,
        amid
    )
    .fetch_optional(pool)
    .await?;

    Ok(match row {
        Some(r) => Some(AgentRecord {
            signing_public_key: r.signing_public_key,
            organization_id: r.organization_id,
        }),
        None => None,
    })
}

async fn is_org_admin(pool: &PgPool, org_id: Uuid, amid: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"SELECT 1 as exists FROM organizations WHERE id = $1 AND admin_amid = $2"#,
        org_id,
        amid
    )
    .fetch_optional(pool)
    .await?;

    Ok(matches!(result, Some(_)))
}

async fn get_revocation(pool: &PgPool, amid: &str) -> Result<Option<RevocationEntry>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT id, amid, reason, revoked_by, revoked_at, notes
        FROM revocations
        WHERE amid = $1
        "#,
        amid
    )
    .fetch_optional(pool)
    .await?;

    Ok(match row {
        Some(r) => Some(RevocationEntry {
            id: r.id,
            amid: r.amid,
            reason: r.reason,
            revoked_by: r.revoked_by,
            revoked_at: r.revoked_at,
            notes: r.notes,
        }),
        None => None,
    })
}

async fn create_revocation(
    pool: &PgPool,
    id: Uuid,
    amid: &str,
    reason: &str,
    revoked_by: &str,
    notes: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO revocations (id, amid, reason, revoked_by, revoked_at, notes)
        VALUES ($1, $2, $3, $4, NOW(), $5)
        "#,
        id,
        amid,
        reason,
        revoked_by,
        notes
    )
    .execute(pool)
    .await?;

    // Downgrade revoked agent to anonymous tier — peers with
    // require_verified_tier policy will stop accepting their messages.
    let _ = crate::db::update_agent_tier(pool, amid, crate::models::TrustTier::Anonymous).await;

    Ok(())
}

async fn get_all_revocations(
    pool: &PgPool,
    limit: i64,
    offset: i64,
) -> Result<(Vec<RevocationEntry>, i64), sqlx::Error> {
    let count_result: Option<i64> = sqlx::query_scalar!("SELECT COUNT(*) FROM revocations")
        .fetch_one(pool)
        .await?;
    let count = count_result.unwrap_or(0);

    let rows = sqlx::query!(
        r#"
        SELECT id, amid, reason, revoked_by, revoked_at, notes
        FROM revocations
        ORDER BY revoked_at DESC
        LIMIT $1 OFFSET $2
        "#,
        limit,
        offset
    )
    .fetch_all(pool)
    .await?;

    let mut entries = Vec::new();
    for r in rows {
        entries.push(RevocationEntry {
            id: r.id,
            amid: r.amid,
            reason: r.reason,
            revoked_by: r.revoked_by,
            revoked_at: r.revoked_at,
            notes: r.notes,
        });
    }

    Ok((entries, count))
}
