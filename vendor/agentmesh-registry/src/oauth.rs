use actix_web::{web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{info, warn, error};
use chrono::{Utc, Duration};
use uuid::Uuid;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ring::rand::{SystemRandom, SecureRandom};

use crate::AppState;

/// Helper to return 503 Service Unavailable during startup
fn service_unavailable() -> HttpResponse {
    HttpResponse::ServiceUnavailable()
        .content_type("application/json")
        .body(r#"{"error":"Service is starting up","status":"starting"}"#)
}

/// OAuth provider configuration
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub github_client_id: Option<String>,
    pub github_client_secret: Option<String>,
    pub google_client_id: Option<String>,
    pub google_client_secret: Option<String>,
    pub callback_base_url: String,
}

impl OAuthConfig {
    pub fn from_env() -> Self {
        Self {
            github_client_id: std::env::var("GITHUB_CLIENT_ID").ok(),
            github_client_secret: std::env::var("GITHUB_CLIENT_SECRET").ok(),
            google_client_id: std::env::var("GOOGLE_CLIENT_ID").ok(),
            google_client_secret: std::env::var("GOOGLE_CLIENT_SECRET").ok(),
            callback_base_url: std::env::var("OAUTH_CALLBACK_BASE_URL")
                .unwrap_or_else(|_| "https://agentmesh.online".to_string()),
        }
    }
}

/// Available OAuth providers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    GitHub,
    Google,
}

impl std::fmt::Display for OAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OAuthProvider::GitHub => write!(f, "github"),
            OAuthProvider::Google => write!(f, "google"),
        }
    }
}

/// OAuth state stored in database
#[derive(Debug, Clone)]
pub struct OAuthState {
    pub state: String,
    pub amid: String,
    pub provider: String,
    pub created_at: chrono::DateTime<Utc>,
    pub expires_at: chrono::DateTime<Utc>,
}

/// Request to start OAuth flow
#[derive(Debug, Deserialize)]
pub struct AuthorizeRequest {
    pub amid: String,
    pub provider: String,
    pub signature: String,
    pub timestamp: chrono::DateTime<Utc>,
}

/// Response with authorization URL
#[derive(Debug, Serialize)]
pub struct AuthorizeResponse {
    pub authorization_url: String,
    pub state: String,
    pub expires_in: u32,
}

/// OAuth callback parameters
#[derive(Debug, Deserialize)]
pub struct CallbackParams {
    pub code: String,
    pub state: String,
}

/// Verification result
#[derive(Debug, Serialize)]
pub struct VerificationResult {
    pub success: bool,
    pub amid: String,
    pub provider: String,
    pub verified_identity: Option<VerifiedIdentity>,
    pub certificate: Option<String>,
    pub error: Option<String>,
}

/// Verified identity information from OAuth
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedIdentity {
    pub provider: String,
    pub provider_id: String,
    pub email: Option<String>,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub verified_at: chrono::DateTime<Utc>,
}

/// Provider info response
#[derive(Debug, Serialize)]
pub struct ProviderInfo {
    pub name: String,
    pub enabled: bool,
    pub display_name: String,
}

/// List of available providers
#[derive(Debug, Serialize)]
pub struct ProvidersResponse {
    pub providers: Vec<ProviderInfo>,
}

/// GitHub user response from API
#[derive(Debug, Deserialize)]
struct GitHubUser {
    id: i64,
    login: String,
    name: Option<String>,
    email: Option<String>,
}

/// GitHub access token response
#[derive(Debug, Deserialize)]
struct GitHubTokenResponse {
    access_token: String,
    token_type: String,
}

/// Google userinfo response
#[derive(Debug, Deserialize)]
struct GoogleUserInfo {
    sub: String,
    email: Option<String>,
    email_verified: Option<bool>,
    name: Option<String>,
}

/// Validated user info returned from token validation
#[derive(Debug, Clone)]
pub struct ValidatedUser {
    pub provider: String,
    pub provider_id: String,
    pub email: Option<String>,
    pub name: Option<String>,
}

/// Validate an OAuth access token and return user info
/// This validates tokens that were previously obtained through the OAuth flow
pub async fn validate_oauth_token(token: &str) -> Result<ValidatedUser, String> {
    // Try GitHub first
    let client = reqwest::Client::new();

    // Try GitHub token
    let github_result = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", token))
        .header("User-Agent", "AgentMesh-Registry/0.2")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await;

    if let Ok(response) = github_result {
        if response.status().is_success() {
            if let Ok(user) = response.json::<GitHubUser>().await {
                return Ok(ValidatedUser {
                    provider: "github".to_string(),
                    provider_id: user.id.to_string(),
                    email: user.email,
                    name: user.name.or(Some(user.login)),
                });
            }
        }
    }

    // Try Google token
    let google_result = client
        .get("https://www.googleapis.com/oauth2/v3/userinfo")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await;

    if let Ok(response) = google_result {
        if response.status().is_success() {
            if let Ok(user) = response.json::<GoogleUserInfo>().await {
                return Ok(ValidatedUser {
                    provider: "google".to_string(),
                    provider_id: user.sub,
                    email: user.email,
                    name: user.name,
                });
            }
        }
    }

    Err("Invalid or expired OAuth token".to_string())
}

/// Google token response
#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    id_token: Option<String>,
    token_type: String,
    expires_in: u32,
}

/// Get available OAuth providers
pub async fn get_providers(
    config: web::Data<OAuthConfig>,
) -> impl Responder {
    let providers = vec![
        ProviderInfo {
            name: "github".to_string(),
            enabled: config.github_client_id.is_some() && config.github_client_secret.is_some(),
            display_name: "GitHub".to_string(),
        },
        ProviderInfo {
            name: "google".to_string(),
            enabled: config.google_client_id.is_some() && config.google_client_secret.is_some(),
            display_name: "Google".to_string(),
        },
    ];

    HttpResponse::Ok().json(ProvidersResponse { providers })
}

/// Generate a cryptographically secure state parameter
fn generate_state() -> String {
    let rng = SystemRandom::new();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes).expect("Failed to generate random bytes");
    BASE64.encode(&bytes)
}

/// Start OAuth authorization flow
pub async fn authorize(
    state: web::Data<Arc<AppState>>,
    config: web::Data<OAuthConfig>,
    req: web::Json<AuthorizeRequest>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    let provider = req.provider.to_lowercase();

    // Validate provider
    let authorization_url = match provider.as_str() {
        "github" => {
            let client_id = match &config.github_client_id {
                Some(id) => id,
                None => {
                    return HttpResponse::BadRequest().json(serde_json::json!({
                        "error": "GitHub OAuth not configured"
                    }));
                }
            };

            let oauth_state = generate_state();
            let redirect_uri = format!("{}/v1/auth/oauth/callback", config.callback_base_url);

            // Store state in database
            if let Err(e) = store_oauth_state(pool, &oauth_state, &req.amid, "github").await {
                error!("Failed to store OAuth state: {}", e);
                return HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "Failed to initiate OAuth flow"
                }));
            }

            let url = format!(
                "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&state={}&scope=read:user%20user:email",
                client_id,
                urlencoding::encode(&redirect_uri),
                urlencoding::encode(&oauth_state)
            );

            AuthorizeResponse {
                authorization_url: url,
                state: oauth_state,
                expires_in: 600, // 10 minutes
            }
        }
        "google" => {
            let client_id = match &config.google_client_id {
                Some(id) => id,
                None => {
                    return HttpResponse::BadRequest().json(serde_json::json!({
                        "error": "Google OAuth not configured"
                    }));
                }
            };

            let oauth_state = generate_state();
            let redirect_uri = format!("{}/v1/auth/oauth/callback", config.callback_base_url);

            // Store state in database
            if let Err(e) = store_oauth_state(pool, &oauth_state, &req.amid, "google").await {
                error!("Failed to store OAuth state: {}", e);
                return HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "Failed to initiate OAuth flow"
                }));
            }

            let url = format!(
                "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&state={}&response_type=code&scope=openid%20email%20profile",
                client_id,
                urlencoding::encode(&redirect_uri),
                urlencoding::encode(&oauth_state)
            );

            AuthorizeResponse {
                authorization_url: url,
                state: oauth_state,
                expires_in: 600,
            }
        }
        _ => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Unknown provider: {}", provider)
            }));
        }
    };

    info!("OAuth authorization initiated for {} via {}", req.amid, provider);
    HttpResponse::Ok().json(authorization_url)
}

/// Handle OAuth callback
pub async fn callback(
    state: web::Data<Arc<AppState>>,
    config: web::Data<OAuthConfig>,
    params: web::Query<CallbackParams>,
) -> impl Responder {
    // Check readiness
    let pool = match state.require_ready() {
        Ok(p) => p,
        Err(_) => return service_unavailable(),
    };

    // Look up state in database
    let oauth_state = match get_oauth_state(pool, &params.state).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            warn!("OAuth callback with unknown state: {}", params.state);
            return HttpResponse::BadRequest().json(VerificationResult {
                success: false,
                amid: String::new(),
                provider: String::new(),
                verified_identity: None,
                certificate: None,
                error: Some("Invalid or expired state parameter".to_string()),
            });
        }
        Err(e) => {
            error!("Failed to look up OAuth state: {}", e);
            return HttpResponse::InternalServerError().json(VerificationResult {
                success: false,
                amid: String::new(),
                provider: String::new(),
                verified_identity: None,
                certificate: None,
                error: Some("Internal error".to_string()),
            });
        }
    };

    // Check expiration
    if Utc::now() > oauth_state.expires_at {
        return HttpResponse::BadRequest().json(VerificationResult {
            success: false,
            amid: oauth_state.amid,
            provider: oauth_state.provider,
            verified_identity: None,
            certificate: None,
            error: Some("OAuth state expired".to_string()),
        });
    }

    // Delete state (one-time use)
    let _ = delete_oauth_state(pool, &params.state).await;

    // Exchange code for token and get user info
    let verified_identity = match oauth_state.provider.as_str() {
        "github" => {
            match exchange_github_code(&config, &params.code).await {
                Ok(identity) => identity,
                Err(e) => {
                    error!("GitHub OAuth failed: {}", e);
                    return HttpResponse::BadRequest().json(VerificationResult {
                        success: false,
                        amid: oauth_state.amid,
                        provider: oauth_state.provider,
                        verified_identity: None,
                        certificate: None,
                        error: Some(format!("GitHub verification failed: {}", e)),
                    });
                }
            }
        }
        "google" => {
            match exchange_google_code(&config, &params.code).await {
                Ok(identity) => identity,
                Err(e) => {
                    error!("Google OAuth failed: {}", e);
                    return HttpResponse::BadRequest().json(VerificationResult {
                        success: false,
                        amid: oauth_state.amid,
                        provider: oauth_state.provider,
                        verified_identity: None,
                        certificate: None,
                        error: Some(format!("Google verification failed: {}", e)),
                    });
                }
            }
        }
        _ => {
            return HttpResponse::BadRequest().json(VerificationResult {
                success: false,
                amid: oauth_state.amid,
                provider: oauth_state.provider,
                verified_identity: None,
                certificate: None,
                error: Some("Unknown provider".to_string()),
            });
        }
    };

    // Store verification and upgrade tier
    if let Err(e) = store_verification(pool, &oauth_state.amid, &verified_identity).await {
        error!("Failed to store verification: {}", e);
        return HttpResponse::InternalServerError().json(VerificationResult {
            success: false,
            amid: oauth_state.amid,
            provider: oauth_state.provider,
            verified_identity: Some(verified_identity),
            certificate: None,
            error: Some("Failed to store verification".to_string()),
        });
    }

    // Generate certificate (placeholder - would use proper PKI)
    let certificate = generate_verification_certificate(&oauth_state.amid, &verified_identity);

    info!(
        "Agent {} verified via {} ({})",
        oauth_state.amid,
        verified_identity.provider,
        verified_identity.username.as_deref().unwrap_or("unknown")
    );

    HttpResponse::Ok().json(VerificationResult {
        success: true,
        amid: oauth_state.amid,
        provider: oauth_state.provider,
        verified_identity: Some(verified_identity),
        certificate: Some(certificate),
        error: None,
    })
}

/// Exchange GitHub authorization code for access token and get user info
async fn exchange_github_code(
    config: &OAuthConfig,
    code: &str,
) -> Result<VerifiedIdentity, String> {
    let client_id = config.github_client_id.as_ref().ok_or("GitHub not configured")?;
    let client_secret = config.github_client_secret.as_ref().ok_or("GitHub not configured")?;

    let client = reqwest::Client::new();

    // Exchange code for token
    let token_response: GitHubTokenResponse = client
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code),
        ])
        .send()
        .await
        .map_err(|e| format!("Token request failed: {}", e))?
        .json()
        .await
        .map_err(|e| format!("Token parsing failed: {}", e))?;

    // Get user info
    let user: GitHubUser = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", token_response.access_token))
        .header("User-Agent", "AgentMesh-Registry/1.0")
        .send()
        .await
        .map_err(|e| format!("User request failed: {}", e))?
        .json()
        .await
        .map_err(|e| format!("User parsing failed: {}", e))?;

    Ok(VerifiedIdentity {
        provider: "github".to_string(),
        provider_id: user.id.to_string(),
        email: user.email,
        username: Some(user.login),
        display_name: user.name,
        verified_at: Utc::now(),
    })
}

/// Exchange Google authorization code for access token and get user info
async fn exchange_google_code(
    config: &OAuthConfig,
    code: &str,
) -> Result<VerifiedIdentity, String> {
    let client_id = config.google_client_id.as_ref().ok_or("Google not configured")?;
    let client_secret = config.google_client_secret.as_ref().ok_or("Google not configured")?;
    let redirect_uri = format!("{}/v1/auth/oauth/callback", config.callback_base_url);

    let client = reqwest::Client::new();

    // Exchange code for token
    let token_response: GoogleTokenResponse = client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .map_err(|e| format!("Token request failed: {}", e))?
        .json()
        .await
        .map_err(|e| format!("Token parsing failed: {}", e))?;

    // Get user info
    let user_info: GoogleUserInfo = client
        .get("https://www.googleapis.com/oauth2/v3/userinfo")
        .header("Authorization", format!("Bearer {}", token_response.access_token))
        .send()
        .await
        .map_err(|e| format!("User request failed: {}", e))?
        .json()
        .await
        .map_err(|e| format!("User parsing failed: {}", e))?;

    // Verify email is confirmed
    if user_info.email_verified != Some(true) {
        return Err("Email not verified with Google".to_string());
    }

    Ok(VerifiedIdentity {
        provider: "google".to_string(),
        provider_id: user_info.sub,
        email: user_info.email,
        username: None,
        display_name: user_info.name,
        verified_at: Utc::now(),
    })
}

/// Store OAuth state in database
async fn store_oauth_state(
    pool: &PgPool,
    state: &str,
    amid: &str,
    provider: &str,
) -> Result<(), sqlx::Error> {
    let expires_at = Utc::now() + Duration::minutes(10);

    sqlx::query!(
        r#"
        INSERT INTO oauth_states (state, amid, provider, expires_at)
        VALUES ($1, $2, $3, $4)
        "#,
        state,
        amid,
        provider,
        expires_at
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// Get OAuth state from database
async fn get_oauth_state(
    pool: &PgPool,
    state: &str,
) -> Result<Option<OAuthState>, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        SELECT state, amid, provider, created_at, expires_at
        FROM oauth_states
        WHERE state = $1
        "#,
        state
    )
    .fetch_optional(pool)
    .await?;

    Ok(match result {
        Some(r) => Some(OAuthState {
            state: r.state,
            amid: r.amid,
            provider: r.provider,
            created_at: r.created_at,
            expires_at: r.expires_at,
        }),
        None => None,
    })
}

/// Delete OAuth state from database
async fn delete_oauth_state(
    pool: &PgPool,
    state: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM oauth_states WHERE state = $1", state)
        .execute(pool)
        .await?;
    Ok(())
}

/// Store verification result and upgrade agent tier
async fn store_verification(
    pool: &PgPool,
    amid: &str,
    identity: &VerifiedIdentity,
) -> Result<(), sqlx::Error> {
    // Store verification record
    sqlx::query!(
        r#"
        INSERT INTO agent_verifications (amid, provider, provider_id, email, username, display_name, verified_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (amid, provider) DO UPDATE SET
            provider_id = EXCLUDED.provider_id,
            email = EXCLUDED.email,
            username = EXCLUDED.username,
            display_name = EXCLUDED.display_name,
            verified_at = EXCLUDED.verified_at
        "#,
        amid,
        identity.provider,
        identity.provider_id,
        identity.email,
        identity.username,
        identity.display_name,
        identity.verified_at
    )
    .execute(pool)
    .await?;

    // Upgrade agent tier to Verified (1)
    sqlx::query!(
        r#"
        UPDATE agents SET tier = 'verified', updated_at = NOW()
        WHERE amid = $1 AND tier = 'anonymous'
        "#,
        amid
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// Generate a verification certificate (placeholder implementation)
fn generate_verification_certificate(amid: &str, identity: &VerifiedIdentity) -> String {
    // In production, this would use proper PKI with X.509 certificates
    // For now, return a JSON structure that can be verified
    let cert = serde_json::json!({
        "version": 1,
        "type": "verification_certificate",
        "amid": amid,
        "provider": identity.provider,
        "provider_id": identity.provider_id,
        "verified_at": identity.verified_at.to_rfc3339(),
        "expires_at": (identity.verified_at + Duration::days(365)).to_rfc3339(),
        "issuer": "agentmesh-registry",
        // In production, this would include a signature from the registry's private key
    });

    BASE64.encode(serde_json::to_string(&cert).unwrap().as_bytes())
}
