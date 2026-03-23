use ed25519_dalek::{Signature, VerifyingKey, Verifier};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use sha2::{Sha256, Digest};
use chrono::{DateTime, Utc, Duration};
use thiserror::Error;

use crate::types::Amid;

#[derive(Error, Debug)]
pub enum AuthError {
    #[error("Invalid signature format")]
    InvalidSignatureFormat,

    #[error("Invalid public key format")]
    InvalidPublicKeyFormat,

    #[error("Signature verification failed")]
    SignatureVerificationFailed,

    #[error("Timestamp too old (replay protection)")]
    TimestampTooOld,

    #[error("Timestamp in future")]
    TimestampInFuture,

    #[error("AMID mismatch")]
    AmidMismatch,
}

/// Strip key type prefix if present (e.g., "ed25519:" or "x25519:")
fn strip_key_prefix(key: &str) -> &str {
    if let Some(stripped) = key.strip_prefix("ed25519:") {
        stripped
    } else if let Some(stripped) = key.strip_prefix("x25519:") {
        stripped
    } else {
        key
    }
}

/// Verify that an agent owns the AMID they claim
/// The signature is over the raw timestamp string, proving they have the private key.
/// We accept the raw string so we verify against exactly what the SDK signed
/// (avoids chrono re-serialization changing "Z" to "+00:00" etc.).
pub fn verify_connection_signature(
    amid: &Amid,
    public_key_b64: &str,
    signature_b64: &str,
    raw_timestamp: &str,
) -> Result<(), AuthError> {
    // Parse the raw timestamp for time-window validation
    let timestamp = DateTime::parse_from_rfc3339(raw_timestamp)
        .or_else(|_| DateTime::parse_from_str(raw_timestamp, "%Y-%m-%dT%H:%M:%S%.fZ"))
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| AuthError::TimestampTooOld)?;

    // Check timestamp is within acceptable window (5 minutes)
    let now = Utc::now();
    let age = now.signed_duration_since(timestamp);

    if age > Duration::minutes(5) {
        return Err(AuthError::TimestampTooOld);
    }

    if age < Duration::minutes(-1) {
        return Err(AuthError::TimestampInFuture);
    }

    // Strip key prefix if present (backwards compatible)
    let key_b64 = strip_key_prefix(public_key_b64);

    // Decode public key
    let public_key_bytes = BASE64.decode(key_b64)
        .map_err(|_| AuthError::InvalidPublicKeyFormat)?;

    let public_key_array: [u8; 32] = public_key_bytes
        .try_into()
        .map_err(|_| AuthError::InvalidPublicKeyFormat)?;

    let verifying_key = VerifyingKey::from_bytes(&public_key_array)
        .map_err(|_| AuthError::InvalidPublicKeyFormat)?;

    // Verify AMID matches public key
    let derived_amid = derive_amid(&public_key_array);
    if derived_amid != *amid {
        return Err(AuthError::AmidMismatch);
    }

    // Decode signature
    let signature_bytes = BASE64.decode(signature_b64)
        .map_err(|_| AuthError::InvalidSignatureFormat)?;

    let signature_array: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| AuthError::InvalidSignatureFormat)?;

    let signature = Signature::from_bytes(&signature_array);

    // Verify signature against the ORIGINAL timestamp string the SDK signed
    verifying_key
        .verify(raw_timestamp.as_bytes(), &signature)
        .map_err(|_| AuthError::SignatureVerificationFailed)?;

    Ok(())
}

/// Derive AMID from public key
/// AMID = base58(sha256(public_key)[:20])
pub fn derive_amid(public_key: &[u8; 32]) -> Amid {
    let mut hasher = Sha256::new();
    hasher.update(public_key);
    let hash = hasher.finalize();

    // Take first 20 bytes
    let truncated: [u8; 20] = hash[..20].try_into().unwrap();

    // Base58 encode
    bs58::encode(truncated).into_string()
}

/// Verify a signature on arbitrary data
pub fn verify_signature(
    public_key_b64: &str,
    data: &[u8],
    signature_b64: &str,
) -> Result<(), AuthError> {
    // Strip key prefix if present
    let key_b64 = strip_key_prefix(public_key_b64);

    // Decode public key
    let public_key_bytes = BASE64.decode(key_b64)
        .map_err(|_| AuthError::InvalidPublicKeyFormat)?;

    let public_key_array: [u8; 32] = public_key_bytes
        .try_into()
        .map_err(|_| AuthError::InvalidPublicKeyFormat)?;

    let verifying_key = VerifyingKey::from_bytes(&public_key_array)
        .map_err(|_| AuthError::InvalidPublicKeyFormat)?;

    // Decode signature
    let signature_bytes = BASE64.decode(signature_b64)
        .map_err(|_| AuthError::InvalidSignatureFormat)?;

    let signature_array: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| AuthError::InvalidSignatureFormat)?;

    let signature = Signature::from_bytes(&signature_array);

    // Verify
    verifying_key
        .verify(data, &signature)
        .map_err(|_| AuthError::SignatureVerificationFailed)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    #[test]
    fn test_amid_derivation() {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let public_key = signing_key.verifying_key();

        let amid = derive_amid(&public_key.to_bytes());

        // AMID should be non-empty base58 string
        assert!(!amid.is_empty());
        assert!(amid.chars().all(|c| {
            c.is_alphanumeric() && c != '0' && c != 'O' && c != 'I' && c != 'l'
        }));
    }

    #[test]
    fn test_signature_verification() {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let public_key = signing_key.verifying_key();

        let amid = derive_amid(&public_key.to_bytes());
        // Use JS-style ISO timestamp (what the SDK produces)
        let raw_timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        let signature = signing_key.sign(raw_timestamp.as_bytes());

        let public_key_b64 = BASE64.encode(public_key.to_bytes());
        let signature_b64 = BASE64.encode(signature.to_bytes());

        let result = verify_connection_signature(
            &amid,
            &public_key_b64,
            &signature_b64,
            &raw_timestamp,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_signature_verification_rfc3339() {
        // Also test with chrono's to_rfc3339() format (backwards compat)
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let public_key = signing_key.verifying_key();

        let amid = derive_amid(&public_key.to_bytes());
        let raw_timestamp = Utc::now().to_rfc3339();

        let signature = signing_key.sign(raw_timestamp.as_bytes());

        let public_key_b64 = BASE64.encode(public_key.to_bytes());
        let signature_b64 = BASE64.encode(signature.to_bytes());

        let result = verify_connection_signature(
            &amid,
            &public_key_b64,
            &signature_b64,
            &raw_timestamp,
        );

        assert!(result.is_ok());
    }
}
