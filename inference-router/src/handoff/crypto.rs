//! Handoff state crypto — serialization, encryption, integrity hash.
//!
//! Extracted from `handoff/mod.rs` per §4.2 hotspot decomposition. Keeps
//! the AES-GCM + HKDF code path in one auditable place and shrinks the
//! parent file. No behaviour change: every public name is re-exported
//! from `crate::handoff` to preserve compatibility for existing
//! callers (`crate::routes::*`, `crate::main`, `crate::handoff::tests`).
//!
//! **Crypto custody.** Today's wire format is AES-256-GCM with HKDF-
//! SHA256 key derivation and SHA-256 integrity hash (Phase H1 from the
//! handoff plan). The plan §4.1 slates a follow-up that routes this
//! through `providers/signing.rs` once the Signing seam covers AEAD;
//! until then this module is the single allow-listed home for the
//! handoff blob cipher (see `ci/no-custom-crypto.sh`).

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use hkdf::Hkdf;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

use super::HandoffState;

// ── Constants ────────────────────────────────────────────────────────────────

/// Schema version stamped into every encrypted handoff blob.
pub const HANDOFF_STATE_VERSION: u32 = 1;

/// HKDF context tag — bound into the derived key so blobs from a
/// different protocol family cannot collide.
pub(crate) const HKDF_INFO: &[u8] = b"azureclaw-handoff-v1";

/// AES-GCM nonce length (96 bits, per NIST SP 800-38D).
pub(crate) const AES_NONCE_BYTES: usize = 12;

// ── Encrypted blob ──────────────────────────────────────────────────────────

/// Encrypted handoff state blob (AES-256-GCM).
#[derive(Debug, Serialize, Deserialize)]
pub struct EncryptedHandoffBlob {
    /// Schema version.
    pub version: u32,
    /// AES-256-GCM nonce (base64).
    pub nonce: String,
    /// Encrypted + compressed state (base64).
    pub ciphertext: String,
    /// HKDF salt (base64) — needed by receiver to derive the same key.
    pub hkdf_salt: String,
    /// SHA-256 of plaintext for pre-decryption integrity check (hex).
    pub verification_hash: String,
}

// ── Hashing ─────────────────────────────────────────────────────────────────

/// SHA-256 hash as hex string. Used by `compute_verification_hash` and
/// other handoff helpers.
pub(crate) fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02x}")).collect()
}

/// Compute verification hash of a plaintext state blob.
pub fn compute_verification_hash(plaintext: &[u8]) -> String {
    hex_sha256(plaintext)
}

// ── Serialization (gzip+JSON) ───────────────────────────────────────────────

/// Serialize a HandoffState to compressed JSON bytes.
pub fn serialize_state(state: &HandoffState) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(state).map_err(|e| format!("JSON serialize: {e}"))?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&json)
        .map_err(|e| format!("gzip compress: {e}"))?;
    encoder.finish().map_err(|e| format!("gzip finish: {e}"))
}

/// Deserialize compressed JSON bytes back to HandoffState.
pub fn deserialize_state(compressed: &[u8]) -> Result<HandoffState, String> {
    let mut decoder = GzDecoder::new(compressed);
    let mut json = Vec::new();
    decoder
        .read_to_end(&mut json)
        .map_err(|e| format!("gzip decompress: {e}"))?;
    serde_json::from_slice(&json).map_err(|e| format!("JSON deserialize: {e}"))
}

// ── AEAD ────────────────────────────────────────────────────────────────────

/// Encrypt state blob with AES-256-GCM.
///
/// Key is derived via HKDF from a shared secret (DH exchange between agents).
/// For Phase H1, the shared secret is the handoff token itself (CLI knows both sides).
/// Phase H2+ replaces this with actual X25519 DH shared secret.
pub fn encrypt_state(
    plaintext: &[u8],
    shared_secret: &[u8],
    salt: &[u8],
) -> Result<EncryptedHandoffBlob, String> {
    // Derive AES-256 key via HKDF-SHA256.
    let hk = Hkdf::<Sha256>::new(Some(salt), shared_secret);
    let mut key_bytes = [0u8; 32];
    hk.expand(HKDF_INFO, &mut key_bytes)
        .map_err(|e| format!("HKDF expand: {e}"))?;

    let cipher = Aes256Gcm::new_from_slice(&key_bytes).map_err(|e| format!("AES key init: {e}"))?;

    // Random 96-bit nonce.
    let mut nonce_bytes = [0u8; AES_NONCE_BYTES];
    rand::rng().fill(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| format!("AES-GCM encrypt: {e}"))?;

    let verification_hash = hex_sha256(plaintext);

    Ok(EncryptedHandoffBlob {
        version: HANDOFF_STATE_VERSION,
        nonce: BASE64.encode(nonce_bytes),
        ciphertext: BASE64.encode(&ciphertext),
        hkdf_salt: BASE64.encode(salt),
        verification_hash,
    })
}

/// Decrypt an encrypted handoff blob.
pub fn decrypt_state(blob: &EncryptedHandoffBlob, shared_secret: &[u8]) -> Result<Vec<u8>, String> {
    let salt = BASE64
        .decode(&blob.hkdf_salt)
        .map_err(|e| format!("decode salt: {e}"))?;
    let nonce_bytes = BASE64
        .decode(&blob.nonce)
        .map_err(|e| format!("decode nonce: {e}"))?;
    let ciphertext = BASE64
        .decode(&blob.ciphertext)
        .map_err(|e| format!("decode ciphertext: {e}"))?;

    if nonce_bytes.len() != AES_NONCE_BYTES {
        return Err(format!(
            "invalid nonce length: {} (expected {AES_NONCE_BYTES})",
            nonce_bytes.len()
        ));
    }

    // Derive same key via HKDF.
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared_secret);
    let mut key_bytes = [0u8; 32];
    hk.expand(HKDF_INFO, &mut key_bytes)
        .map_err(|e| format!("HKDF expand: {e}"))?;

    let cipher = Aes256Gcm::new_from_slice(&key_bytes).map_err(|e| format!("AES key init: {e}"))?;

    let nonce = Nonce::from_slice(&nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| "AES-GCM decryption failed — wrong key or tampered ciphertext".to_string())?;

    // Verify integrity.
    let hash = hex_sha256(&plaintext);
    if hash != blob.verification_hash {
        return Err(format!(
            "integrity check failed: computed={} expected={}",
            &hash[..16],
            &blob.verification_hash[..16]
        ));
    }

    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_sha256_known_vector() {
        // RFC 6234 test vector: SHA-256("hello world").
        let h = hex_sha256(b"hello world");
        assert_eq!(
            h,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let secret = b"shared-secret-32-bytes-XXXXXXXXX";
        let salt = b"random-salt-16!!";
        let pt = b"some plaintext payload";
        let blob = encrypt_state(pt, secret, salt).expect("encrypt");
        let got = decrypt_state(&blob, secret).expect("decrypt");
        assert_eq!(got, pt);
        assert_eq!(blob.version, HANDOFF_STATE_VERSION);
    }

    #[test]
    fn decrypt_rejects_tampered_ciphertext() {
        let secret = b"shared-secret-32-bytes-XXXXXXXXX";
        let salt = b"random-salt-16!!";
        let pt = b"some plaintext payload";
        let mut blob = encrypt_state(pt, secret, salt).expect("encrypt");

        // Flip one bit in ciphertext (decode, mutate, re-encode).
        let mut raw = BASE64.decode(&blob.ciphertext).expect("b64");
        raw[0] ^= 0x01;
        blob.ciphertext = BASE64.encode(&raw);

        let err = decrypt_state(&blob, secret).expect_err("must reject tampered");
        assert!(
            err.contains("AES-GCM decryption failed"),
            "expected AEAD failure, got: {err}"
        );
    }

    #[test]
    fn decrypt_rejects_wrong_key() {
        let salt = b"random-salt-16!!";
        let blob = encrypt_state(
            b"hello",
            b"shared-secret-32-bytes-XXXXXXXXX",
            salt,
        )
        .expect("encrypt");
        let err = decrypt_state(&blob, b"different-secret-32bytes-XXXXXXX")
            .expect_err("must reject wrong key");
        assert!(err.contains("AES-GCM decryption failed"), "got: {err}");
    }

    #[test]
    fn decrypt_rejects_invalid_nonce_length() {
        let salt = b"random-salt-16!!";
        let mut blob = encrypt_state(b"hello", b"shared-secret-32-bytes-XXXXXXXXX", salt)
            .expect("encrypt");
        // Truncate the nonce.
        blob.nonce = BASE64.encode([0u8; 8]);
        let err = decrypt_state(&blob, b"shared-secret-32-bytes-XXXXXXXXX")
            .expect_err("must reject short nonce");
        assert!(err.contains("invalid nonce length"), "got: {err}");
    }

    #[test]
    fn decrypt_rejects_tampered_verification_hash() {
        // GCM auth-tag should already catch tampering, but the
        // verification_hash is a defence-in-depth integrity check on
        // plaintext. If somebody substitutes a fully-valid encryption
        // of a *different* plaintext while keeping the original
        // verification_hash, decrypt must reject.
        let secret = b"shared-secret-32-bytes-XXXXXXXXX";
        let salt = b"random-salt-16!!";
        let blob_a = encrypt_state(b"alpha", secret, salt).expect("encrypt a");
        let mut blob_b = encrypt_state(b"beta", secret, salt).expect("encrypt b");
        // Substitute A's verification_hash into B's blob.
        blob_b.verification_hash = blob_a.verification_hash.clone();
        let err = decrypt_state(&blob_b, secret).expect_err("must reject mismatched hash");
        assert!(err.contains("integrity check failed"), "got: {err}");
    }

    #[test]
    fn compute_verification_hash_matches_hex_sha256() {
        let pt = b"identical inputs must hash identically";
        assert_eq!(compute_verification_hash(pt), hex_sha256(pt));
    }
}
