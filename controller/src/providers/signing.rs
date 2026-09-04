// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Governance Receipt signing provider (kars Bridge V0, Inc 3).
//!
//! This is an **allowlisted crypto wrapper** (see `ci/no-custom-crypto.sh`):
//! it is the single file whose job is to turn an in-toto Statement into a
//! signed [DSSE] envelope using Ed25519. No crypto primitives leak outside
//! this module — callers hand it canonical JSON bytes and receive a
//! [`DsseEnvelope`] they can persist verbatim.
//!
//! ## Trust model (V0, honest)
//!
//! - The controller holds a long-lived Ed25519 keypair, persisted to the
//!   `controller-receipt-identity` Secret in `kars-system` (mirrors the mesh
//!   peer identity). It is generated on first start.
//! - The **public** key is published, out of band, to the
//!   `kars-receipt-pubkey` ConfigMap in `kars-system`. A verifier
//!   (`kars receipt verify`) trusts *that* anchor, never a key embedded in a
//!   receipt — so swapping the key inside a forged receipt does not help an
//!   attacker.
//! - This is **local signing**. There is no external transparency log / KMS
//!   anchor yet; that is the V1 upgrade and the receipt says so verbatim
//!   (the `regulatory` claim class is `OMITTED`). We never imply more
//!   assurance than we deliver.
//!
//! ## Wire format
//!
//! The signed payload is the [DSSE Pre-Authentication Encoding][PAE] over the
//! canonical in-toto Statement JSON with payload type
//! `application/vnd.in-toto+json`. Ed25519 signatures are deterministic, so
//! the same Statement always yields byte-identical output — which is exactly
//! what lets a verifier re-derive the Statement from the live `KarsTask` and
//! confirm it matches before checking the signature.
//!
//! [DSSE]: https://github.com/secure-systems-lab/dsse
//! [PAE]: https://github.com/secure-systems-lab/dsse/blob/master/protocol.md

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::{
    Client,
    api::{Api, Patch, PatchParams, PostParams},
};
use rand::RngCore;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::mesh_peer::IDENTITY_NAMESPACE;

/// Secret holding the controller's receipt-signing private key.
const IDENTITY_SECRET_NAME: &str = "controller-receipt-identity";
/// ConfigMap publishing the verifier trust anchor (public key + key id).
pub const PUBKEY_CONFIGMAP_NAME: &str = "kars-receipt-pubkey";
/// DSSE payload type for in-toto Statements.
pub const DSSE_PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";
/// Signing scheme identifier embedded in receipts for forward-compat.
pub const SIGNING_SCHEME: &str = "DSSEv1+ed25519";
/// Server-Side Apply field manager for receipt-signing writes.
const FIELD_MANAGER: &str = "kars-controller/receipt-signing";

/// A DSSE envelope, serialized verbatim into a `KarsReceipt`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DsseEnvelope {
    /// Base64 of the in-toto Statement JSON (the signed payload).
    pub payload: String,
    /// Always [`DSSE_PAYLOAD_TYPE`].
    pub payload_type: String,
    /// One Ed25519 signature in V0.
    pub signatures: Vec<DsseSignature>,
}

/// A single DSSE signature line.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DsseSignature {
    /// Hex SHA-256 fingerprint of the signing public key.
    pub keyid: String,
    /// Base64 of the 64-byte Ed25519 signature over the PAE.
    pub sig: String,
}

/// The controller's receipt-signing identity.
#[derive(Clone)]
pub struct ReceiptSigner {
    signing_key: SigningKey,
    /// Hex SHA-256 fingerprint of the public key — the receipt `keyid`.
    pub key_id: String,
}

impl ReceiptSigner {
    /// Construct from 32 raw secret-key bytes.
    pub fn from_bytes(secret_key_bytes: &[u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(secret_key_bytes);
        let key_id = fingerprint(&signing_key.verifying_key());
        Self {
            signing_key,
            key_id,
        }
    }

    /// Generate a fresh random identity.
    pub fn generate() -> Self {
        let mut rng = rand::rng();
        let mut key_bytes = [0u8; 32];
        rng.fill_bytes(&mut key_bytes);
        Self::from_bytes(&key_bytes)
    }

    /// Base64 of the 32-byte Ed25519 public key (published to the anchor CM).
    pub fn public_key_b64(&self) -> String {
        BASE64.encode(self.signing_key.verifying_key().to_bytes())
    }

    /// Sign canonical in-toto Statement JSON, producing a DSSE envelope.
    pub fn sign_statement(&self, statement_json: &[u8]) -> DsseEnvelope {
        let pae = pae(DSSE_PAYLOAD_TYPE, statement_json);
        let signature = self.signing_key.sign(&pae);
        DsseEnvelope {
            payload: BASE64.encode(statement_json),
            payload_type: DSSE_PAYLOAD_TYPE.to_string(),
            signatures: vec![DsseSignature {
                keyid: self.key_id.clone(),
                sig: BASE64.encode(signature.to_bytes()),
            }],
        }
    }

    /// Sign raw note bytes with Ed25519, returning the base64 signature.
    ///
    /// Used for the inclusion-log **signed checkpoint** (a "signed tree head"):
    /// a compact, signed commitment to the log's size + head hash that clients
    /// and an external witness can pin without the full chain. Deterministic
    /// (Ed25519) so re-signing the same note is byte-identical.
    pub fn sign_note(&self, note: &[u8]) -> String {
        BASE64.encode(self.signing_key.sign(note).to_bytes())
    }
}

/// Hex SHA-256 fingerprint of an Ed25519 public key.
fn fingerprint(verifying_key: &VerifyingKey) -> String {
    let hash = Sha256::digest(verifying_key.to_bytes());
    let mut out = String::with_capacity(64);
    for b in hash.iter() {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// DSSE Pre-Authentication Encoding:
/// `"DSSEv1" SP len(type) SP type SP len(body) SP body`.
///
/// This is the standard DSSE framing, not a bespoke construction — it exists
/// so the signature is unambiguously bound to both the payload type and the
/// payload, defeating type-confusion attacks.
pub fn pae(payload_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload_type.len() + body.len() + 32);
    out.extend_from_slice(b"DSSEv1 ");
    out.extend_from_slice(payload_type.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload_type.as_bytes());
    out.push(b' ');
    out.extend_from_slice(body.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(body);
    out
}

/// Load the controller's receipt identity from its Secret, generating and
/// persisting one on first start, then publish the public-key anchor
/// ConfigMap so verifiers can check signatures out of band.
pub async fn load_or_create(client: &Client) -> Result<ReceiptSigner> {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), IDENTITY_NAMESPACE);

    let signer = match secrets.get(IDENTITY_SECRET_NAME).await {
        Ok(secret) => {
            let key = secret
                .data
                .as_ref()
                .and_then(|d| d.get("signing_key"))
                .and_then(|b| <[u8; 32]>::try_from(b.0.as_slice()).ok());
            match key {
                Some(bytes) => {
                    let signer = ReceiptSigner::from_bytes(&bytes);
                    tracing::info!(key_id = %signer.key_id, "Loaded receipt-signing identity");
                    signer
                }
                None => {
                    tracing::warn!("Receipt identity Secret malformed — regenerating");
                    create_identity(&secrets).await?
                }
            }
        }
        Err(kube::Error::Api(ae)) if ae.code == 404 => {
            tracing::info!("No receipt identity Secret — generating new one");
            create_identity(&secrets).await?
        }
        Err(e) => return Err(e).context("reading receipt identity Secret"),
    };

    publish_pubkey(client, &signer).await?;
    Ok(signer)
}

/// Generate a new identity and persist it to the Secret.
async fn create_identity(secrets: &Api<Secret>) -> Result<ReceiptSigner> {
    let signer = ReceiptSigner::generate();
    let secret: Secret = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": IDENTITY_SECRET_NAME,
            "namespace": IDENTITY_NAMESPACE,
        },
        "data": {
            "signing_key": BASE64.encode(signer.signing_key.to_bytes()),
            "key_id": BASE64.encode(signer.key_id.as_bytes()),
        }
    }))?;
    secrets
        .create(&PostParams::default(), &secret)
        .await
        .context("creating receipt identity Secret")?;
    tracing::info!(key_id = %signer.key_id, "Generated new receipt-signing identity");
    Ok(signer)
}

/// Publish the public key + key id to the `kars-receipt-pubkey` ConfigMap.
/// This is the out-of-band trust anchor a verifier reads — never the key
/// inside a receipt.
async fn publish_pubkey(client: &Client, signer: &ReceiptSigner) -> Result<()> {
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), IDENTITY_NAMESPACE);
    let cm: ConfigMap = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": PUBKEY_CONFIGMAP_NAME,
            "namespace": IDENTITY_NAMESPACE,
            "labels": {
                "app.kubernetes.io/name": "kars",
                "app.kubernetes.io/component": "receipt-trust-anchor",
            },
        },
        "data": {
            "keyId": signer.key_id,
            "publicKey": signer.public_key_b64(),
            "scheme": SIGNING_SCHEME,
            "payloadType": DSSE_PAYLOAD_TYPE,
        }
    }))?;
    cms.patch(
        PUBKEY_CONFIGMAP_NAME,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&cm),
    )
    .await
    .context("publishing receipt pubkey ConfigMap")?;
    tracing::info!(key_id = %signer.key_id, "Published receipt trust anchor ConfigMap");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;

    #[test]
    fn pae_matches_dsse_spec() {
        // Reference vector shape from the DSSE spec: framing is
        // "DSSEv1 " + len + " " + type + " " + len + " " + body.
        let got = pae("application/vnd.in-toto+json", b"{}");
        let expected = b"DSSEv1 28 application/vnd.in-toto+json 2 {}";
        assert_eq!(got, expected);
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let signer = ReceiptSigner::generate();
        let statement = br#"{"_type":"https://in-toto.io/Statement/v1"}"#;
        let env = signer.sign_statement(statement);

        // A verifier reconstructs the PAE and checks the signature against the
        // published public key — exactly what `kars receipt verify` does.
        let pub_bytes: [u8; 32] = BASE64
            .decode(signer.public_key_b64())
            .unwrap()
            .try_into()
            .unwrap();
        let vk = VerifyingKey::from_bytes(&pub_bytes).unwrap();
        let sig_bytes: [u8; 64] = BASE64
            .decode(&env.signatures[0].sig)
            .unwrap()
            .try_into()
            .unwrap();
        let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
        let pae = pae(DSSE_PAYLOAD_TYPE, statement);
        assert!(vk.verify(&pae, &sig).is_ok());
        assert_eq!(env.signatures[0].keyid, signer.key_id);
    }

    #[test]
    fn signatures_are_deterministic() {
        // Ed25519 is deterministic: re-signing the same Statement yields the
        // same bytes, so receipt emission is idempotent and a verifier can
        // re-derive the exact artifact.
        let signer = ReceiptSigner::generate();
        let statement = br#"{"subject":[{"name":"ns/task"}]}"#;
        let a = signer.sign_statement(statement);
        let b = signer.sign_statement(statement);
        assert_eq!(a.signatures[0].sig, b.signatures[0].sig);
    }

    #[test]
    fn fingerprint_is_hex_sha256() {
        let signer = ReceiptSigner::generate();
        assert_eq!(signer.key_id.len(), 64);
        assert!(signer.key_id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
