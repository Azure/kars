use sqlx::{PgPool, Row};
use anyhow::Result;
use chrono::Utc;

use crate::models::*;

/// Create a new agent in the database
pub async fn create_agent(pool: &PgPool, agent: &Agent) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO agents (
            id, amid, signing_public_key, exchange_public_key, tier,
            display_name, organization_id, capabilities, relay_endpoint,
            direct_endpoint, status, reputation_score, last_seen,
            created_at, updated_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15
        )
        "#
    )
    .bind(&agent.id)
    .bind(&agent.amid)
    .bind(&agent.signing_public_key)
    .bind(&agent.exchange_public_key)
    .bind(&agent.tier)
    .bind(&agent.display_name)
    .bind(&agent.organization_id)
    .bind(&agent.capabilities)
    .bind(&agent.relay_endpoint)
    .bind(&agent.direct_endpoint)
    .bind(&agent.status)
    .bind(agent.reputation_score)
    .bind(&agent.last_seen)
    .bind(&agent.created_at)
    .bind(&agent.updated_at)
    .execute(pool)
    .await?;

    Ok(())
}

/// Get an agent by AMID
pub async fn get_agent_by_amid(pool: &PgPool, amid: &str) -> Result<Option<Agent>> {
    let agent = sqlx::query_as::<_, Agent>(
        r#"
        SELECT id, amid, signing_public_key, exchange_public_key, tier,
               display_name, organization_id, capabilities, relay_endpoint,
               direct_endpoint, status, reputation_score, last_seen,
               created_at, updated_at
        FROM agents
        WHERE amid = $1
        "#
    )
    .bind(amid)
    .fetch_optional(pool)
    .await?;

    Ok(agent)
}

/// Search agents by capability
pub async fn search_by_capability(
    pool: &PgPool,
    req: &CapabilitySearchRequest,
) -> Result<(Vec<Agent>, u64)> {
    // Count total
    let total: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*) FROM agents
        WHERE $1 = ANY(capabilities)
        AND ($2 IS NULL OR
             CASE tier
                WHEN 'organization' THEN 1
                WHEN 'verified' THEN 1
                ELSE 2
             END <= $2)
        AND ($3 IS NULL OR reputation_score >= $3)
        AND ($4 IS NULL OR status = $4)
        "#
    )
    .bind(&req.capability)
    .bind(req.tier_min.map(|t| t as i32))
    .bind(req.reputation_min)
    .bind(&req.status)
    .fetch_one(pool)
    .await?;

    // Fetch page
    let agents = sqlx::query_as::<_, Agent>(
        r#"
        SELECT id, amid, signing_public_key, exchange_public_key, tier,
               display_name, organization_id, capabilities, relay_endpoint,
               direct_endpoint, status, reputation_score, last_seen,
               created_at, updated_at
        FROM agents
        WHERE $1 = ANY(capabilities)
        AND ($2 IS NULL OR
             CASE tier
                WHEN 'organization' THEN 1
                WHEN 'verified' THEN 1
                ELSE 2
             END <= $2)
        AND ($3 IS NULL OR reputation_score >= $3)
        AND ($4 IS NULL OR status = $4)
        ORDER BY reputation_score DESC, last_seen DESC
        LIMIT $5 OFFSET $6
        "#
    )
    .bind(&req.capability)
    .bind(req.tier_min.map(|t| t as i32))
    .bind(req.reputation_min)
    .bind(&req.status)
    .bind(req.limit as i64)
    .bind(req.offset as i64)
    .fetch_all(pool)
    .await?;

    Ok((agents, total as u64))
}

/// Update agent status
pub async fn update_agent_status(
    pool: &PgPool,
    amid: &str,
    status: PresenceStatus,
) -> Result<bool> {
    let result = sqlx::query(
        r#"
        UPDATE agents
        SET status = $2, last_seen = $3, updated_at = $3
        WHERE amid = $1
        "#
    )
    .bind(amid)
    .bind(status)
    .bind(Utc::now())
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Update agent capabilities
pub async fn update_agent_capabilities(
    pool: &PgPool,
    amid: &str,
    capabilities: &[String],
) -> Result<bool> {
    let result = sqlx::query(
        r#"
        UPDATE agents
        SET capabilities = $2, updated_at = $3
        WHERE amid = $1
        "#
    )
    .bind(amid)
    .bind(capabilities)
    .bind(Utc::now())
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Get basic stats (total agents, online agents)
pub async fn get_stats(pool: &PgPool) -> Result<(u64, u64)> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agents")
        .fetch_one(pool)
        .await?;

    let online: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agents WHERE status = 'online'"
    )
    .fetch_one(pool)
    .await?;

    Ok((total as u64, online as u64))
}

/// Get detailed registry statistics
pub async fn get_detailed_stats(pool: &PgPool) -> Result<serde_json::Value> {
    let (total, online) = get_stats(pool).await?;

    let by_tier: Vec<(String, i64)> = sqlx::query_as(
        "SELECT tier::text, COUNT(*) FROM agents GROUP BY tier"
    )
    .fetch_all(pool)
    .await?;

    let by_status: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status::text, COUNT(*) FROM agents GROUP BY status"
    )
    .fetch_all(pool)
    .await?;

    let avg_reputation: f64 = sqlx::query_scalar(
        "SELECT COALESCE(AVG(reputation_score), 0.5) FROM agents"
    )
    .fetch_one(pool)
    .await?;

    Ok(serde_json::json!({
        "total_agents": total,
        "online_agents": online,
        "by_tier": by_tier.into_iter().collect::<std::collections::HashMap<_, _>>(),
        "by_status": by_status.into_iter().collect::<std::collections::HashMap<_, _>>(),
        "average_reputation": avg_reputation,
    }))
}

// ============== Prekey Functions ==============

/// Store or update signed prekey
pub async fn upsert_signed_prekey(
    pool: &PgPool,
    amid: &str,
    prekey_id: i32,
    public_key: &str,
    signature: &str,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO signed_prekeys (amid, prekey_id, public_key, signature)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (amid, prekey_id) DO UPDATE
        SET public_key = EXCLUDED.public_key,
            signature = EXCLUDED.signature,
            created_at = NOW()
        "#
    )
    .bind(amid)
    .bind(prekey_id)
    .bind(public_key)
    .bind(signature)
    .execute(pool)
    .await?;

    Ok(())
}

/// Store one-time prekeys
pub async fn store_one_time_prekeys(
    pool: &PgPool,
    amid: &str,
    prekeys: &[(i32, String)],  // (id, public_key)
) -> Result<()> {
    for (prekey_id, public_key) in prekeys {
        sqlx::query(
            r#"
            INSERT INTO one_time_prekeys (amid, prekey_id, public_key)
            VALUES ($1, $2, $3)
            ON CONFLICT (amid, prekey_id) DO NOTHING
            "#
        )
        .bind(amid)
        .bind(prekey_id)
        .bind(public_key)
        .execute(pool)
        .await?;
    }

    Ok(())
}

/// Get signed prekey for an agent
pub async fn get_signed_prekey(
    pool: &PgPool,
    amid: &str,
) -> Result<Option<(i32, String, String)>> {  // (id, public_key, signature)
    let row: Option<(i32, String, String)> = sqlx::query_as(
        r#"
        SELECT prekey_id, public_key, signature
        FROM signed_prekeys
        WHERE amid = $1
        ORDER BY prekey_id DESC
        LIMIT 1
        "#
    )
    .bind(amid)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

/// Get and consume one-time prekey atomically
pub async fn consume_one_time_prekey(
    pool: &PgPool,
    amid: &str,
) -> Result<Option<(i32, String)>> {  // (id, public_key)
    let row: Option<(i32, String)> = sqlx::query_as(
        "SELECT * FROM consume_one_time_prekey($1)"
    )
    .bind(amid)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

/// Get count of remaining one-time prekeys
pub async fn get_prekey_count(pool: &PgPool, amid: &str) -> Result<i64> {
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*) FROM one_time_prekeys
        WHERE amid = $1 AND NOT consumed
        "#
    )
    .bind(amid)
    .fetch_one(pool)
    .await?;

    Ok(count)
}

// ============== Certificate Functions ==============

/// Store agent certificate
pub async fn store_agent_certificate(
    pool: &PgPool,
    amid: &str,
    certificate: &str,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO agent_certificates (amid, certificate, created_at)
        VALUES ($1, $2, NOW())
        ON CONFLICT (amid) DO UPDATE
        SET certificate = EXCLUDED.certificate,
            created_at = NOW()
        "#
    )
    .bind(amid)
    .bind(certificate)
    .execute(pool)
    .await?;

    Ok(())
}

/// Get agent certificate
pub async fn get_agent_certificate(pool: &PgPool, amid: &str) -> Result<Option<String>> {
    let cert: Option<String> = sqlx::query_scalar(
        r#"
        SELECT certificate FROM agent_certificates
        WHERE amid = $1
        "#
    )
    .bind(amid)
    .fetch_optional(pool)
    .await?;

    Ok(cert)
}

/// Get organization name by ID
pub async fn get_organization_name(pool: &PgPool, org_id: uuid::Uuid) -> Result<Option<String>> {
    let name: Option<String> = sqlx::query_scalar(
        r#"
        SELECT name FROM organizations
        WHERE id = $1
        "#
    )
    .bind(org_id)
    .fetch_optional(pool)
    .await?;

    Ok(name)
}

/// Get agent reputation details (ratings count and flags)
pub async fn get_agent_reputation_details(
    pool: &PgPool,
    amid: &str,
) -> Result<(u32, Vec<String>)> {
    // Get ratings count
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*) FROM reputation_feedback
        WHERE target_amid = $1
        "#
    )
    .bind(amid)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    // Get flags
    let flags: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT flag FROM agent_flags
        WHERE amid = $1 AND active = true
        "#
    )
    .bind(amid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    Ok((count as u32, flags))
}

// ============== Reputation Anti-Gaming Functions ==============

/// Submit a reputation rating with anti-gaming measures
pub async fn submit_reputation_rating(
    pool: &PgPool,
    target_amid: &str,
    rater_amid: &str,
    rater_tier: TrustTier,
    session_id: uuid::Uuid,
    score: f32,
    tags: Option<Vec<String>>,
    rater_ip_hash: Option<&str>,
) -> Result<()> {
    let tier_str = match rater_tier {
        TrustTier::Organization => "organization",
        TrustTier::Verified => "verified",
        TrustTier::Anonymous => "anonymous",
    };

    sqlx::query(
        r#"
        SELECT submit_reputation_rating($1, $2, $3::trust_tier, $4, $5, $6, $7)
        "#
    )
    .bind(target_amid)
    .bind(rater_amid)
    .bind(tier_str)
    .bind(session_id)
    .bind(score)
    .bind(tags.unwrap_or_default())
    .bind(rater_ip_hash)
    .execute(pool)
    .await?;

    Ok(())
}

/// Get reputation status (rated/unrated) for an agent
pub async fn get_reputation_status(
    pool: &PgPool,
    amid: &str,
) -> Result<String> {
    let status: String = sqlx::query_scalar(
        r#"SELECT get_reputation_status($1)"#
    )
    .bind(amid)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|_| "unrated".to_string());

    Ok(status)
}

/// Check if agents have mutual ratings within 24 hours
pub async fn has_mutual_rating(
    pool: &PgPool,
    amid1: &str,
    amid2: &str,
) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM reputation_feedback rf1
        JOIN reputation_feedback rf2 ON rf1.rater_amid = rf2.target_amid
            AND rf1.target_amid = rf2.rater_amid
        WHERE rf1.rater_amid = $1 AND rf1.target_amid = $2
        AND rf2.created_at > rf1.created_at - INTERVAL '24 hours'
        AND rf2.created_at < rf1.created_at + INTERVAL '24 hours'
        "#
    )
    .bind(amid1)
    .bind(amid2)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    Ok(count > 0)
}

/// Get agents with rapid reputation changes (for monitoring)
pub async fn get_rapid_reputation_changes(
    pool: &PgPool,
) -> Result<Vec<(String, String)>> {
    let changes: Vec<(String, String)> = sqlx::query_as(
        r#"
        SELECT amid, flag
        FROM agent_flags
        WHERE flag IN ('rapid_reputation_increase', 'rapid_reputation_decrease')
        AND active = true
        AND expires_at > NOW()
        ORDER BY created_at DESC
        LIMIT 100
        "#
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    Ok(changes)
}

/// Standard rating tags
pub const RATING_TAGS: &[&str] = &[
    "fast_response",
    "accurate",
    "professional",
    "reliable",
    "helpful",
    "knowledgeable",
    "slow_response",
    "inaccurate",
    "unhelpful",
    "unreliable",
];
