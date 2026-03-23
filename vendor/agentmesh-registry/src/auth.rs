use ed25519_dalek::{Signature, VerifyingKey, Verifier};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use sha2::{Sha256, Digest};
use chrono::{DateTime, Utc, Duration};
use thiserror::Error;

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

    #[error("Invalid timestamp format")]
    InvalidTimestamp,
}

/// Derive AMID from public key
/// AMID = base58(sha256(public_key)[:20])
pub fn derive_amid(public_key: &[u8; 32]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(public_key);
    let hash = hasher.finalize();

    // Take first 20 bytes
    let truncated: [u8; 20] = hash[..20].try_into().unwrap();

    // Base58 encode
    bs58::encode(truncated).into_string()
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

/// Verify that an agent owns the AMID they claim by verifying their signature
/// The message signed is the ISO timestamp string (passed as-is to preserve format)
pub fn verify_registration_signature(
    amid: &str,
    public_key_b64: &str,
    signature_b64: &str,
    timestamp_str: &str,
) -> Result<(), AuthError> {
    // Parse timestamp to validate it's a valid ISO timestamp
    let timestamp = DateTime::parse_from_rfc3339(timestamp_str)
        .or_else(|_| {
            // Try parsing ISO format with Z suffix (JavaScript's toISOString format)
            DateTime::parse_from_str(timestamp_str, "%Y-%m-%dT%H:%M:%S%.fZ")
                .map(|dt| dt.with_timezone(&Utc).fixed_offset())
        })
        .map_err(|_| AuthError::InvalidTimestamp)?
        .with_timezone(&Utc);

    // Check timestamp is within acceptable window (5 minutes)
    let now = Utc::now();
    let age = now.signed_duration_since(timestamp);

    if age > Duration::minutes(5) {
        return Err(AuthError::TimestampTooOld);
    }

    if age < Duration::minutes(-1) {
        return Err(AuthError::TimestampInFuture);
    }

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

    // Verify AMID matches public key
    let derived_amid = derive_amid(&public_key_array);
    if derived_amid != amid {
        return Err(AuthError::AmidMismatch);
    }

    // Decode signature
    let signature_bytes = BASE64.decode(signature_b64)
        .map_err(|_| AuthError::InvalidSignatureFormat)?;

    let signature_array: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| AuthError::InvalidSignatureFormat)?;

    let signature = Signature::from_bytes(&signature_array);

    // Message is the ORIGINAL timestamp string (not reformatted!)
    // This is critical for signature verification - the exact bytes that were signed
    let message = timestamp_str;

    // Verify signature
    verifying_key
        .verify(message.as_bytes(), &signature)
        .map_err(|_| AuthError::SignatureVerificationFailed)?;

    Ok(())
}

/// Verify a signature on arbitrary data (for status/capabilities updates)
/// Requires looking up the public key from the database
pub fn verify_update_signature(
    public_key_b64: &str,
    raw_timestamp: &str,
    signature_b64: &str,
) -> Result<(), AuthError> {
    // Parse the raw timestamp for time-window validation
    let timestamp = DateTime::parse_from_rfc3339(raw_timestamp)
        .or_else(|_| DateTime::parse_from_str(raw_timestamp, "%Y-%m-%dT%H:%M:%S%.fZ"))
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| AuthError::InvalidTimestamp)?;

    // Check timestamp is within acceptable window (5 minutes)
    let now = Utc::now();
    let age = now.signed_duration_since(timestamp);

    if age > Duration::minutes(5) {
        return Err(AuthError::TimestampTooOld);
    }

    if age < Duration::minutes(-1) {
        return Err(AuthError::TimestampInFuture);
    }

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

    // Verify signature against the ORIGINAL timestamp string
    verifying_key
        .verify(raw_timestamp.as_bytes(), &signature)
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
    }

    #[test]
    fn test_registration_signature_verification() {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let public_key = signing_key.verifying_key();

        let amid = derive_amid(&public_key.to_bytes());
        let timestamp_str = Utc::now().to_rfc3339();

        let signature = signing_key.sign(timestamp_str.as_bytes());

        let public_key_b64 = BASE64.encode(public_key.to_bytes());
        let signature_b64 = BASE64.encode(signature.to_bytes());

        let result = verify_registration_signature(
            &amid,
            &public_key_b64,
            &signature_b64,
            &timestamp_str,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_prefixed_key_verification() {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let public_key = signing_key.verifying_key();

        let amid = derive_amid(&public_key.to_bytes());
        let timestamp_str = Utc::now().to_rfc3339();

        let signature = signing_key.sign(timestamp_str.as_bytes());

        // Use prefixed key format
        let public_key_b64 = format!("ed25519:{}", BASE64.encode(public_key.to_bytes()));
        let signature_b64 = BASE64.encode(signature.to_bytes());

        let result = verify_registration_signature(
            &amid,
            &public_key_b64,
            &signature_b64,
            &timestamp_str,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn test_javascript_timestamp_format() {
        // Test that JavaScript's toISOString format works
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let public_key = signing_key.verifying_key();

        let amid = derive_amid(&public_key.to_bytes());
        // JavaScript toISOString format: 2026-02-01T16:25:10.555Z
        let timestamp_str = "2026-02-01T16:25:10.555Z";

        let signature = signing_key.sign(timestamp_str.as_bytes());

        let public_key_b64 = BASE64.encode(public_key.to_bytes());
        let signature_b64 = BASE64.encode(signature.to_bytes());

        // This should fail on timestamp validation (too old) but not on format parsing
        let result = verify_registration_signature(
            &amid,
            &public_key_b64,
            &signature_b64,
            timestamp_str,
        );

        // We expect TimestampTooOld since we're using a fixed old timestamp
        assert!(matches!(result, Err(AuthError::TimestampTooOld)));
    }
}
