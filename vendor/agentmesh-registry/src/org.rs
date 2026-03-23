use actix_web::{web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{info, warn, error};
use chrono::{Utc, Duration};
use uuid::Uuid;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ring::rand::{SystemRandom, SecureRandom};
use sha2::{Sha256, Digest};

use crate::auth;
use crate::AppState;

/// Helper to return 503 Service Unavailable during startup
fn service_unavailable() -> HttpResponse {
    HttpResponse::ServiceUnavailable()
        .content_type("application/json")
        .body(r#"{"error":"Service is starting up","status":"starting"}"#)
}

/// Organization model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub domain: String,
    pub root_certificate: Option<String>,
    pub dns_challenge: Option<String>,
    pub dns_verified: bool,
    pub admin_amid: String,
    pub created_at: chrono::DateTime<Utc>,
    pub verified_at: Option<chrono::DateTime<Utc>>,
}

/// Request to register an organization
#[derive(Debug, Deserialize)]
pub struct RegisterOrgRequest {
    pub name: String,
    pub domain: String,
    pub admin_amid: String,
    pub admin_signing_public_key: String,
    pub signature: String,
    pub timestamp: String,
}

/// Response for organization registration
#[derive(Debug, Serialize)]
pub struct RegisterOrgResponse {
    pub success: bool,
    pub organization_id: Option<Uuid>,
    pub dns_challenge: Option<DnsChallenge>,
    pub error: Option<String>,
}

/// DNS challenge for domain verification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsChallenge {
    pub record_type: String,
    pub record_name: String,
    pub record_value: String,
    pub expires_at: chrono::DateTime<Utc>,
}

/// Request to verify DNS challenge
#[derive(Debug, Deserialize)]
pub struct VerifyDnsRequest {
    pub organization_id: Uuid,
    pub admin_amid: String,
    pub signature: String,
    pub timestamp: chrono::DateTime<Utc>,
}

/// Response for DNS verification
#[derive(Debug, Serialize)]
pub struct VerifyDnsResponse {
    pub success: bool,
    pub verified: bool,
    pub root_certificate: Option<String>,
    pub error: Option<String>,
}

/// Request to register an agent under an organization
#[derive(Debug, Deserialize)]
pub struct RegisterOrgAgentRequest {
    pub organization_id: Uuid,
    pub agent_amid: String,
    pub agent_signing_public_key: String,
    pub agent_exchange_public_key: String,
    pub display_name: Option<String>,
    pub capabilities: Vec<String>,
    pub admin_amid: String,
    pub admin_signature: String,
    pub timestamp: chrono::DateTime<Utc>,
}

/// Response for org agent registration
#[derive(Debug, Serialize)]
pub struct RegisterOrgAgentResponse {
    pub success: bool,
    pub agent_certificate: Option<String>,
    pub error: Option<String>,
}

/// Organization lookup response
#[derive(Debug, Serialize)]
pub struct OrgLookup {
    pub id: Uuid,
    pub name: String,
    pub domain: String,
    pub verified: bool,
    pub agent_count: i64,
    pub created_at: chrono::DateTime<Utc>,
}

/// Generate a DNS challenge value
fn generate_dns_challenge() -> String {
    let rng = SystemRandom::new();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes).expect("Failed to generate random bytes");

    // Format: agentmesh-verify=<base64_value>
    format!("agentmesh-verify={}", BASE64.encode(&bytes))
}

/// Register a new organization
pub async fn register_org(
    state: web::Data<Arc<AppState>>,
    req: web::Json<RegisterOrgRequest>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    info!("Organization registration request for domain: {}", req.domain);

    // Verify admin signature
    if let Err(auth_err) = auth::verify_registration_signature(
        &req.admin_amid,
        &req.admin_signing_public_key,
        &req.signature,
        &req.timestamp,
    ) {
        warn!("Org registration signature failed: {:?}", auth_err);
        return HttpResponse::Unauthorized().json(RegisterOrgResponse {
            success: false,
            organization_id: None,
            dns_challenge: None,
            error: Some(format!("Signature verification failed: {}", auth_err)),
        });
    }

    // Check if domain already registered
    if let Ok(Some(_)) = get_org_by_domain(pool, &req.domain).await {
        return HttpResponse::Conflict().json(RegisterOrgResponse {
            success: false,
            organization_id: None,
            dns_challenge: None,
            error: Some("Domain already registered".to_string()),
        });
    }

    // Generate DNS challenge
    let challenge = generate_dns_challenge();
    let challenge_expires = Utc::now() + Duration::hours(24);

    // Create organization record
    let org_id = Uuid::new_v4();
    let dns_challenge = DnsChallenge {
        record_type: "TXT".to_string(),
        record_name: format!("_agentmesh.{}", req.domain),
        record_value: challenge.clone(),
        expires_at: challenge_expires,
    };

    match create_organization(pool, org_id, &req.name, &req.domain, &req.admin_amid, &challenge).await {
        Ok(_) => {
            info!("Organization {} created with ID {}", req.domain, org_id);
            HttpResponse::Created().json(RegisterOrgResponse {
                success: true,
                organization_id: Some(org_id),
                dns_challenge: Some(dns_challenge),
                error: None,
            })
        }
        Err(e) => {
            error!("Failed to create organization: {}", e);
            HttpResponse::InternalServerError().json(RegisterOrgResponse {
                success: false,
                organization_id: None,
                dns_challenge: None,
                error: Some("Failed to create organization".to_string()),
            })
        }
    }
}

/// Verify DNS TXT record for organization
pub async fn verify_dns(
    state: web::Data<Arc<AppState>>,
    req: web::Json<VerifyDnsRequest>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    // Get organization
    let org = match get_org_by_id(pool, req.organization_id).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            return HttpResponse::NotFound().json(VerifyDnsResponse {
                success: false,
                verified: false,
                root_certificate: None,
                error: Some("Organization not found".to_string()),
            });
        }
        Err(e) => {
            error!("Database error: {}", e);
            return HttpResponse::InternalServerError().json(VerifyDnsResponse {
                success: false,
                verified: false,
                root_certificate: None,
                error: Some("Internal error".to_string()),
            });
        }
    };

    // Verify admin
    if org.admin_amid != req.admin_amid {
        return HttpResponse::Unauthorized().json(VerifyDnsResponse {
            success: false,
            verified: false,
            root_certificate: None,
            error: Some("Not the organization admin".to_string()),
        });
    }

    // Check if already verified
    if org.dns_verified {
        return HttpResponse::Ok().json(VerifyDnsResponse {
            success: true,
            verified: true,
            root_certificate: org.root_certificate,
            error: None,
        });
    }

    // Perform DNS lookup
    let expected_challenge = match &org.dns_challenge {
        Some(c) => c,
        None => {
            return HttpResponse::BadRequest().json(VerifyDnsResponse {
                success: false,
                verified: false,
                root_certificate: None,
                error: Some("No DNS challenge found".to_string()),
            });
        }
    };

    let record_name = format!("_agentmesh.{}", org.domain);
    let verified = match verify_dns_txt_record(&record_name, expected_challenge).await {
        Ok(v) => v,
        Err(e) => {
            warn!("DNS verification failed for {}: {}", org.domain, e);
            return HttpResponse::Ok().json(VerifyDnsResponse {
                success: true,
                verified: false,
                root_certificate: None,
                error: Some(format!("DNS verification failed: {}", e)),
            });
        }
    };

    if !verified {
        return HttpResponse::Ok().json(VerifyDnsResponse {
            success: true,
            verified: false,
            root_certificate: None,
            error: Some("DNS TXT record not found or doesn't match".to_string()),
        });
    }

    // Generate root certificate
    let root_cert = generate_org_root_certificate(&org);

    // Update organization
    if let Err(e) = mark_org_verified(pool, req.organization_id, &root_cert).await {
        error!("Failed to update organization: {}", e);
        return HttpResponse::InternalServerError().json(VerifyDnsResponse {
            success: false,
            verified: false,
            root_certificate: None,
            error: Some("Failed to update organization".to_string()),
        });
    }

    info!("Organization {} verified successfully", org.domain);

    HttpResponse::Ok().json(VerifyDnsResponse {
        success: true,
        verified: true,
        root_certificate: Some(root_cert),
        error: None,
    })
}

/// Register an agent under an organization
pub async fn register_org_agent(
    state: web::Data<Arc<AppState>>,
    req: web::Json<RegisterOrgAgentRequest>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    // Get organization
    let org = match get_org_by_id(pool, req.organization_id).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            return HttpResponse::NotFound().json(RegisterOrgAgentResponse {
                success: false,
                agent_certificate: None,
                error: Some("Organization not found".to_string()),
            });
        }
        Err(e) => {
            error!("Database error: {}", e);
            return HttpResponse::InternalServerError().json(RegisterOrgAgentResponse {
                success: false,
                agent_certificate: None,
                error: Some("Internal error".to_string()),
            });
        }
    };

    // Verify organization is verified
    if !org.dns_verified {
        return HttpResponse::BadRequest().json(RegisterOrgAgentResponse {
            success: false,
            agent_certificate: None,
            error: Some("Organization not verified".to_string()),
        });
    }

    // Verify admin
    if org.admin_amid != req.admin_amid {
        return HttpResponse::Unauthorized().json(RegisterOrgAgentResponse {
            success: false,
            agent_certificate: None,
            error: Some("Not the organization admin".to_string()),
        });
    }

    // Create agent record with organization tier
    let agent_id = Uuid::new_v4();
    match create_org_agent(
        pool,
        agent_id,
        &req.agent_amid,
        &req.agent_signing_public_key,
        &req.agent_exchange_public_key,
        req.organization_id,
        req.display_name.as_deref(),
        &req.capabilities,
    ).await {
        Ok(_) => {
            // Generate agent certificate
            let agent_cert = generate_agent_certificate(&org, &req.agent_amid);

            info!("Agent {} registered under organization {}", req.agent_amid, org.domain);

            HttpResponse::Created().json(RegisterOrgAgentResponse {
                success: true,
                agent_certificate: Some(agent_cert),
                error: None,
            })
        }
        Err(e) => {
            error!("Failed to create agent: {}", e);
            HttpResponse::InternalServerError().json(RegisterOrgAgentResponse {
                success: false,
                agent_certificate: None,
                error: Some("Failed to create agent".to_string()),
            })
        }
    }
}

/// Get organization by domain
pub async fn lookup_org(
    state: web::Data<Arc<AppState>>,
    query: web::Query<OrgQuery>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    match get_org_by_domain(pool, &query.domain).await {
        Ok(Some(org)) => {
            let agent_count = get_org_agent_count(pool, org.id).await.unwrap_or(0);
            HttpResponse::Ok().json(OrgLookup {
                id: org.id,
                name: org.name,
                domain: org.domain,
                verified: org.dns_verified,
                agent_count,
                created_at: org.created_at,
            })
        }
        Ok(None) => HttpResponse::NotFound().json(serde_json::json!({
            "error": "Organization not found"
        })),
        Err(e) => {
            error!("Database error: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Internal error"
            }))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct OrgQuery {
    pub domain: String,
}

// Database functions

async fn create_organization(
    pool: &PgPool,
    id: Uuid,
    name: &str,
    domain: &str,
    admin_amid: &str,
    dns_challenge: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO organizations (id, name, domain, admin_amid, dns_challenge, dns_verified, created_at)
        VALUES ($1, $2, $3, $4, $5, false, NOW())
        "#,
        id,
        name,
        domain,
        admin_amid,
        dns_challenge
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn get_org_by_domain(pool: &PgPool, domain: &str) -> Result<Option<Organization>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT id, name, domain, root_certificate, dns_challenge, dns_verified, admin_amid, created_at, verified_at
        FROM organizations
        WHERE domain = $1
        "#,
        domain
    )
    .fetch_optional(pool)
    .await?;

    Ok(match row {
        Some(r) => Some(Organization {
            id: r.id,
            name: r.name,
            domain: r.domain,
            root_certificate: r.root_certificate,
            dns_challenge: r.dns_challenge,
            dns_verified: r.dns_verified,
            admin_amid: r.admin_amid.unwrap_or_default(),
            created_at: r.created_at,
            verified_at: r.verified_at,
        }),
        None => None,
    })
}

async fn get_org_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Organization>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT id, name, domain, root_certificate, dns_challenge, dns_verified, admin_amid, created_at, verified_at
        FROM organizations
        WHERE id = $1
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;

    Ok(match row {
        Some(r) => Some(Organization {
            id: r.id,
            name: r.name,
            domain: r.domain,
            root_certificate: r.root_certificate,
            dns_challenge: r.dns_challenge,
            dns_verified: r.dns_verified,
            admin_amid: r.admin_amid.unwrap_or_default(),
            created_at: r.created_at,
            verified_at: r.verified_at,
        }),
        None => None,
    })
}

async fn mark_org_verified(pool: &PgPool, id: Uuid, root_cert: &str) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE organizations
        SET dns_verified = true, root_certificate = $2, verified_at = NOW()
        WHERE id = $1
        "#,
        id,
        root_cert
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn get_org_agent_count(pool: &PgPool, org_id: Uuid) -> Result<i64, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT COUNT(*) as count FROM agents WHERE organization_id = $1"#,
        org_id
    )
    .fetch_one(pool)
    .await?;
    Ok(row.count.unwrap_or(0))
}

async fn create_org_agent(
    pool: &PgPool,
    id: Uuid,
    amid: &str,
    signing_public_key: &str,
    exchange_public_key: &str,
    organization_id: Uuid,
    display_name: Option<&str>,
    capabilities: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO agents (id, amid, signing_public_key, exchange_public_key, tier, organization_id, display_name, capabilities, status, reputation_score, created_at, updated_at, last_seen)
        VALUES ($1, $2, $3, $4, 'organization', $5, $6, $7, 'offline', 0.7, NOW(), NOW(), NOW())
        ON CONFLICT (amid) DO UPDATE SET
            tier = 'organization',
            organization_id = EXCLUDED.organization_id,
            updated_at = NOW()
        "#,
        id,
        amid,
        signing_public_key,
        exchange_public_key,
        organization_id,
        display_name,
        capabilities
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Verify DNS TXT record
async fn verify_dns_txt_record(record_name: &str, expected_value: &str) -> Result<bool, String> {
    // Use trust-dns-resolver or similar
    // For now, use a simple approach via DNS-over-HTTPS
    let url = format!(
        "https://dns.google/resolve?name={}&type=TXT",
        urlencoding::encode(record_name)
    );

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("Accept", "application/dns-json")
        .send()
        .await
        .map_err(|e| format!("DNS request failed: {}", e))?;

    let data: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("DNS response parsing failed: {}", e))?;

    // Check for TXT records in the response
    if let Some(answers) = data.get("Answer").and_then(|a| a.as_array()) {
        for answer in answers {
            if answer.get("type").and_then(|t| t.as_i64()) == Some(16) { // TXT record
                if let Some(txt_data) = answer.get("data").and_then(|d| d.as_str()) {
                    // Remove surrounding quotes if present
                    let txt_value = txt_data.trim_matches('"');
                    if txt_value == expected_value {
                        return Ok(true);
                    }
                }
            }
        }
    }

    Ok(false)
}

/// Generate organization root certificate
fn generate_org_root_certificate(org: &Organization) -> String {
    let cert = serde_json::json!({
        "version": 1,
        "type": "org_root_certificate",
        "organization_id": org.id,
        "organization_name": org.name,
        "domain": org.domain,
        "issued_at": Utc::now().to_rfc3339(),
        "expires_at": (Utc::now() + Duration::days(365)).to_rfc3339(),
        "issuer": "agentmesh-registry",
        "tier": "organization",
    });

    BASE64.encode(serde_json::to_string(&cert).unwrap().as_bytes())
}

/// Generate agent certificate under an organization
fn generate_agent_certificate(org: &Organization, agent_amid: &str) -> String {
    let cert = serde_json::json!({
        "version": 1,
        "type": "agent_certificate",
        "agent_amid": agent_amid,
        "organization_id": org.id,
        "organization_domain": org.domain,
        "issued_at": Utc::now().to_rfc3339(),
        "expires_at": (Utc::now() + Duration::days(365)).to_rfc3339(),
        "issuer": "agentmesh-registry",
        "tier": "organization",
        "chain": ["agentmesh-registry", org.domain.clone(), agent_amid],
    });

    BASE64.encode(serde_json::to_string(&cert).unwrap().as_bytes())
}
