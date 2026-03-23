use chrono::{Utc, Duration};
use tracing::{info, warn};
use crate::models::TrustTier;

/// Certificate validity periods by tier
const VERIFIED_CERT_VALIDITY_DAYS: i64 = 365;      // 1 year
const ORGANIZATION_CERT_VALIDITY_DAYS: i64 = 365;  // 1 year
const SESSION_CERT_VALIDITY_DAYS: i64 = 7;         // 7 days

/// Issue an agent certificate
///
/// Generates a PEM-encoded certificate for verified agents.
/// In production, this would use proper X.509 certificate generation.
pub fn issue_agent_certificate(
    amid: &str,
    signing_public_key: &str,
    tier: TrustTier,
) -> Result<String, String> {
    let validity_days = match tier {
        TrustTier::Verified => VERIFIED_CERT_VALIDITY_DAYS,
        TrustTier::Organization => ORGANIZATION_CERT_VALIDITY_DAYS,
        TrustTier::Anonymous => {
            return Err("Cannot issue certificate for anonymous tier".to_string());
        }
    };

    let issued_at = Utc::now();
    let expires_at = issued_at + Duration::days(validity_days);

    // Generate certificate in simplified format
    // In production, use proper X.509 certificate generation with ed25519-dalek
    let cert_data = serde_json::json!({
        "version": 1,
        "type": "agent",
        "amid": amid,
        "tier": format!("{:?}", tier),
        "signing_public_key": signing_public_key,
        "issued_at": issued_at.to_rfc3339(),
        "expires_at": expires_at.to_rfc3339(),
        "issuer": "AgentMesh Registry",
        "serial_number": format!("{:x}", rand::random::<u64>()),
    });

    // Encode as base64 for transport (simulates PEM encoding)
    let cert_json = serde_json::to_string(&cert_data)
        .map_err(|e| format!("Failed to serialize certificate: {}", e))?;

    let encoded = base64::encode(&cert_json);

    // Format as PEM-like structure
    let pem = format!(
        "-----BEGIN AGENTMESH CERTIFICATE-----\n{}\n-----END AGENTMESH CERTIFICATE-----",
        encoded.chars()
            .collect::<Vec<char>>()
            .chunks(64)
            .map(|c| c.iter().collect::<String>())
            .collect::<Vec<String>>()
            .join("\n")
    );

    info!(
        "Issued {:?} certificate for {} (expires: {})",
        tier, amid, expires_at.to_rfc3339()
    );

    Ok(pem)
}

/// Issue an organization certificate after DNS verification
pub fn issue_organization_certificate(
    org_name: &str,
    domain: &str,
    root_public_key: &str,
) -> Result<String, String> {
    let issued_at = Utc::now();
    let expires_at = issued_at + Duration::days(ORGANIZATION_CERT_VALIDITY_DAYS);

    let cert_data = serde_json::json!({
        "version": 1,
        "type": "organization",
        "organization": org_name,
        "domain": domain,
        "root_public_key": root_public_key,
        "issued_at": issued_at.to_rfc3339(),
        "expires_at": expires_at.to_rfc3339(),
        "issuer": "AgentMesh Registry",
        "serial_number": format!("{:x}", rand::random::<u64>()),
        "is_ca": true,
    });

    let cert_json = serde_json::to_string(&cert_data)
        .map_err(|e| format!("Failed to serialize certificate: {}", e))?;

    let encoded = base64::encode(&cert_json);

    let pem = format!(
        "-----BEGIN AGENTMESH CERTIFICATE-----\n{}\n-----END AGENTMESH CERTIFICATE-----",
        encoded.chars()
            .collect::<Vec<char>>()
            .chunks(64)
            .map(|c| c.iter().collect::<String>())
            .collect::<Vec<String>>()
            .join("\n")
    );

    info!(
        "Issued organization certificate for {} ({}) (expires: {})",
        org_name, domain, expires_at.to_rfc3339()
    );

    Ok(pem)
}

/// Validate a certificate (check expiration and format)
pub fn validate_certificate(pem: &str) -> Result<CertificateInfo, String> {
    // Extract base64 content from PEM
    let content = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<String>();

    let decoded = base64::decode(&content)
        .map_err(|e| format!("Failed to decode certificate: {}", e))?;

    let cert_data: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|e| format!("Failed to parse certificate: {}", e))?;

    let expires_at = cert_data.get("expires_at")
        .and_then(|v| v.as_str())
        .ok_or("Missing expires_at field")?;

    let expires = chrono::DateTime::parse_from_rfc3339(expires_at)
        .map_err(|e| format!("Invalid expires_at format: {}", e))?;

    if Utc::now() > expires {
        return Err("Certificate has expired".to_string());
    }

    Ok(CertificateInfo {
        amid: cert_data.get("amid").and_then(|v| v.as_str()).map(String::from),
        organization: cert_data.get("organization").and_then(|v| v.as_str()).map(String::from),
        tier: cert_data.get("tier").and_then(|v| v.as_str()).map(String::from),
        expires_at: expires.with_timezone(&Utc),
        serial_number: cert_data.get("serial_number").and_then(|v| v.as_str()).map(String::from),
        is_ca: cert_data.get("is_ca").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

/// Certificate information extracted from a validated certificate
#[derive(Debug)]
pub struct CertificateInfo {
    pub amid: Option<String>,
    pub organization: Option<String>,
    pub tier: Option<String>,
    pub expires_at: chrono::DateTime<Utc>,
    pub serial_number: Option<String>,
    pub is_ca: bool,
}

/// Add certificate revocation endpoint support
pub fn revoke_certificate(serial_number: &str) -> Result<(), String> {
    // In production, this would add to a CRL or OCSP responder
    info!("Certificate revoked: {}", serial_number);
    Ok(())
}

/// Check if a certificate is revoked
pub fn is_certificate_revoked(serial_number: &str) -> bool {
    // In production, check against CRL or OCSP
    false
}
