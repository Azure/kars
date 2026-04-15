//! Controller mesh peer — connects the controller to AgentMesh relay as a
//! long-running peer, enabling external agents to pair and request offloads.
//!
//! The controller's mesh identity (Ed25519) is persisted in a K8s Secret.
//! On startup, it connects to the relay via WebSocket and listens for:
//! - `pair_request` — validates pairing token, binds AMID, responds
//! - `offload_request` — validates pairing, creates ClawSandbox CRD
//! - `offload_cancel` — cancels an active offload
//!
//! For the pairing ceremony, messages use a simplified protocol (the pairing
//! token provides out-of-band trust). Full Signal Protocol E2E encryption
//! for offload data transfer is handled by the offload sandbox itself.

use anyhow::{Context as _, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use k8s_openapi::api::core::v1::Secret;
use kube::{
    Client, ResourceExt,
    api::{Api, Patch, PatchParams, PostParams},
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::pairing::{ClawPairing, phase};

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Controller mesh identity — Ed25519 keypair + derived AMID.
#[derive(Clone)]
pub struct MeshIdentity {
    pub amid: String,
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
}

impl MeshIdentity {
    /// Generate a new random identity.
    pub fn generate() -> Self {
        let mut rng = rand::rng();
        let mut key_bytes = [0u8; 32];
        rng.fill_bytes(&mut key_bytes);
        let signing_key = SigningKey::from_bytes(&key_bytes);
        let verifying_key = signing_key.verifying_key();
        let amid = derive_amid(&verifying_key);
        Self {
            amid,
            signing_key,
            verifying_key,
        }
    }

    /// Load from raw key bytes.
    pub fn from_bytes(secret_key_bytes: &[u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(secret_key_bytes);
        let verifying_key = signing_key.verifying_key();
        let amid = derive_amid(&verifying_key);
        Self {
            amid,
            signing_key,
            verifying_key,
        }
    }

    /// Sign a timestamp string for relay authentication.
    pub fn sign_timestamp(&self, timestamp: &str) -> String {
        let signature = self.signing_key.sign(timestamp.as_bytes());
        BASE64.encode(signature.to_bytes())
    }

    /// Get base64-encoded public key.
    pub fn public_key_b64(&self) -> String {
        BASE64.encode(self.verifying_key.to_bytes())
    }
}

/// Derive AMID from Ed25519 public key: base58(sha256(pubkey)[:20])
fn derive_amid(verifying_key: &VerifyingKey) -> String {
    let hash = Sha256::digest(verifying_key.to_bytes());
    bs58::encode(&hash[..20]).into_string()
}

// ---------------------------------------------------------------------------
// K8s Secret persistence
// ---------------------------------------------------------------------------

const IDENTITY_SECRET_NAME: &str = "controller-mesh-identity";
const IDENTITY_NAMESPACE: &str = "azureclaw-system";

/// Load or create the controller's mesh identity from a K8s Secret.
pub async fn load_or_create_identity(client: &Client) -> Result<MeshIdentity> {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), IDENTITY_NAMESPACE);

    // Try loading existing
    match secrets.get(IDENTITY_SECRET_NAME).await {
        Ok(secret) => {
            if let Some(data) = &secret.data
                && let Some(key_bytes) = data.get("signing_key")
            {
                let bytes = &key_bytes.0;
                if bytes.len() == 32 {
                    let key: [u8; 32] = bytes[..32].try_into().unwrap();
                    let identity = MeshIdentity::from_bytes(&key);
                    tracing::info!(amid = %identity.amid, "Loaded mesh identity from Secret");
                    return Ok(identity);
                }
            }
            tracing::warn!("Mesh identity Secret exists but is malformed — regenerating");
        }
        Err(kube::Error::Api(ae)) if ae.code == 404 => {
            tracing::info!("No mesh identity Secret found — generating new identity");
        }
        Err(e) => {
            return Err(e).context("Failed to read mesh identity Secret");
        }
    }

    // Generate new identity
    let identity = MeshIdentity::generate();
    tracing::info!(amid = %identity.amid, "Generated new controller mesh identity");

    // Persist to K8s Secret
    let secret = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": IDENTITY_SECRET_NAME,
            "namespace": IDENTITY_NAMESPACE
        },
        "data": {
            "signing_key": BASE64.encode(identity.signing_key.to_bytes()),
            "amid": BASE64.encode(identity.amid.as_bytes())
        }
    }))?;
    let secrets: Api<Secret> = Api::namespaced(client.clone(), IDENTITY_NAMESPACE);
    secrets
        .create(&PostParams::default(), &secret)
        .await
        .context("Failed to create mesh identity Secret")?;

    Ok(identity)
}

// ---------------------------------------------------------------------------
// Relay protocol messages (subset needed for controller)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RelayMessage {
    Connect {
        protocol: String,
        amid: String,
        public_key: String,
        signature: String,
        timestamp: String,
        #[serde(default)]
        p2p_capable: bool,
    },
    Connected {
        session_id: String,
        pending_messages: u32,
    },
    Send {
        to: String,
        encrypted_payload: String,
        message_type: String,
    },
    Receive {
        from: String,
        encrypted_payload: String,
        message_type: String,
        timestamp: String,
    },
    Ping {
        timestamp: String,
    },
    Pong {
        timestamp: String,
    },
    Error {
        code: String,
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Pairing protocol messages (carried inside encrypted_payload as base64 JSON)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum PairingMessage {
    #[serde(rename = "pair_request")]
    PairRequest {
        secret: String,
        pubkey_ed25519: String,
        #[serde(default)]
        pubkey_x25519: Option<String>,
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default)]
        capabilities_requested: Option<Vec<String>>,
    },
    #[serde(rename = "pair_response")]
    PairResponse {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        cluster_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        controller_amid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        capabilities_granted: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        slots: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        token_budget: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        expires_at: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Mesh peer state
// ---------------------------------------------------------------------------

struct MeshPeerState {
    identity: MeshIdentity,
    client: Client,
    relay_url: String,
    cluster_name: String,
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

/// Run the controller mesh peer. Connects to the relay, listens for messages,
/// and handles pairing/offload requests. Reconnects automatically on disconnect.
pub async fn run(client: Client) -> Result<()> {
    let relay_url = std::env::var("MESH_RELAY_URL")
        .unwrap_or_else(|_| "wss://relay.agentmesh.online/v1/connect".into());
    let cluster_name =
        std::env::var("CLUSTER_NAME").unwrap_or_else(|_| "azureclaw-cluster".into());

    let identity = load_or_create_identity(&client).await?;
    tracing::info!(
        amid = %identity.amid,
        relay = %relay_url,
        "Controller mesh peer starting"
    );

    let state = Arc::new(MeshPeerState {
        identity,
        client,
        relay_url,
        cluster_name,
    });

    // Reconnect loop
    loop {
        match connect_and_listen(state.clone()).await {
            Ok(()) => {
                tracing::info!("Relay connection closed gracefully — reconnecting in 5s");
            }
            Err(e) => {
                tracing::warn!("Relay connection error: {e:#} — reconnecting in 10s");
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Connect to relay and process messages until disconnection.
async fn connect_and_listen(state: Arc<MeshPeerState>) -> Result<()> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(&state.relay_url)
        .await
        .context("Failed to connect to relay")?;

    let (write, read) = ws_stream.split();
    let write = Arc::new(RwLock::new(write));

    // Authenticate
    let timestamp = chrono::Utc::now().to_rfc3339();
    let signature = state.identity.sign_timestamp(&timestamp);
    let connect_msg = RelayMessage::Connect {
        protocol: "agentmesh/0.2".into(),
        amid: state.identity.amid.clone(),
        public_key: state.identity.public_key_b64(),
        signature,
        timestamp,
        p2p_capable: false,
    };
    {
        let mut w = write.write().await;
        w.send(WsMessage::Text(serde_json::to_string(&connect_msg)?.into()))
            .await?;
    }

    // Spawn keepalive
    let write_ping = write.clone();
    let ping_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let ping = RelayMessage::Ping {
                timestamp: chrono::Utc::now().to_rfc3339(),
            };
            let mut w = write_ping.write().await;
            if w.send(WsMessage::Text(serde_json::to_string(&ping).unwrap_or_default().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // Process incoming messages
    let mut read = read;
    while let Some(msg) = read.next().await {
        let msg = msg.context("WebSocket read error")?;
        match msg {
            WsMessage::Text(text) => {
                if let Err(e) = handle_message(&state, &write, &text).await {
                    tracing::warn!("Error handling message: {e:#}");
                }
            }
            WsMessage::Ping(data) => {
                let mut w = write.write().await;
                let _ = w.send(WsMessage::Pong(data)).await;
            }
            WsMessage::Close(_) => {
                tracing::info!("Relay sent close frame");
                break;
            }
            _ => {}
        }
    }

    ping_handle.abort();
    Ok(())
}

/// Handle a single relay message.
async fn handle_message(
    state: &MeshPeerState,
    write: &Arc<RwLock<impl SinkExt<WsMessage, Error = tokio_tungstenite::tungstenite::Error> + Unpin>>,
    text: &str,
) -> Result<()> {
    let msg: RelayMessage = serde_json::from_str(text)?;
    match msg {
        RelayMessage::Connected {
            session_id,
            pending_messages,
        } => {
            tracing::info!(
                session_id = %session_id,
                pending = pending_messages,
                "Connected to relay as {}",
                state.identity.amid
            );
        }
        RelayMessage::Receive {
            from,
            encrypted_payload,
            message_type,
            ..
        } => {
            if message_type == "message" || message_type == "optimistic_message" {
                handle_peer_message(state, write, &from, &encrypted_payload).await?;
            } else {
                tracing::debug!(from = %from, msg_type = %message_type, "Ignoring relay message");
            }
        }
        RelayMessage::Pong { .. } => {}
        RelayMessage::Error { code, message } => {
            tracing::warn!(code = %code, "Relay error: {message}");
        }
        _ => {}
    }
    Ok(())
}

/// Handle a message from a peer. Decode and dispatch by type.
async fn handle_peer_message(
    state: &MeshPeerState,
    write: &Arc<RwLock<impl SinkExt<WsMessage, Error = tokio_tungstenite::tungstenite::Error> + Unpin>>,
    from_amid: &str,
    payload_b64: &str,
) -> Result<()> {
    // Decode base64 payload → JSON
    let payload_bytes = BASE64.decode(payload_b64).unwrap_or_default();
    let payload_str = String::from_utf8_lossy(&payload_bytes);

    let msg: PairingMessage = match serde_json::from_str(&payload_str) {
        Ok(m) => m,
        Err(_) => {
            tracing::debug!(from = %from_amid, "Unrecognized message format — ignoring");
            return Ok(());
        }
    };

    match msg {
        PairingMessage::PairRequest {
            secret,
            pubkey_ed25519,
            display_name,
            ..
        } => {
            tracing::info!(from = %from_amid, "Received pair_request");
            let response =
                handle_pair_request(state, from_amid, &secret, &pubkey_ed25519, display_name.as_deref())
                    .await;
            // Send response back
            let response_json = serde_json::to_string(&response)?;
            let response_b64 = BASE64.encode(response_json.as_bytes());
            let send_msg = RelayMessage::Send {
                to: from_amid.to_string(),
                encrypted_payload: response_b64,
                message_type: "message".into(),
            };
            let mut w = write.write().await;
            w.send(WsMessage::Text(serde_json::to_string(&send_msg)?.into()))
                .await?;
        }
        PairingMessage::PairResponse { .. } => {
            tracing::debug!(from = %from_amid, "Ignoring pair_response (we are the controller)");
        }
    }

    Ok(())
}

/// Validate a pair_request against ClawPairing CRDs.
async fn handle_pair_request(
    state: &MeshPeerState,
    from_amid: &str,
    secret: &str,
    pubkey_ed25519: &str,
    display_name: Option<&str>,
) -> PairingMessage {
    let token_hash = hex_sha256(secret);

    // List all ClawPairing CRDs and find matching token_hash
    let pairings: Api<ClawPairing> =
        Api::namespaced(state.client.clone(), IDENTITY_NAMESPACE);
    let pairing_list = match pairings
        .list(&kube::api::ListParams::default())
        .await
    {
        Ok(list) => list,
        Err(e) => {
            tracing::error!("Failed to list ClawPairings: {e}");
            return pair_error("Internal error — could not verify token");
        }
    };

    // Find matching pairing
    let matching = pairing_list.items.iter().find(|p| p.spec.token_hash == token_hash);
    let pairing = match matching {
        Some(p) => p,
        None => {
            tracing::warn!(from = %from_amid, "Pair request with invalid token");
            return pair_error("Invalid pairing token");
        }
    };

    let pairing_name = pairing.name_any();
    let current_phase = pairing
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("");

    // Verify phase
    if current_phase != phase::PENDING {
        tracing::warn!(
            pairing = %pairing_name,
            phase = %current_phase,
            "Pair request for non-pending pairing"
        );
        return pair_error(&format!("Pairing is {current_phase} — token already consumed or expired"));
    }

    // Verify not expired
    if let Ok(expiry) = chrono::DateTime::parse_from_rfc3339(&pairing.spec.expires_at)
        && chrono::Utc::now() >= expiry.to_utc()
    {
        return pair_error("Pairing token has expired");
    }

    // Bind the AMID to this pairing
    let now = chrono::Utc::now().to_rfc3339();
    let patch = json!({
        "status": {
            "phase": phase::ACTIVE,
            "boundAmid": from_amid,
            "boundPubkeyEd25519": pubkey_ed25519,
            "pairedAt": now
        }
    });

    if let Err(e) = pairings
        .patch_status(
            &pairing_name,
            &PatchParams::apply("azureclaw-mesh-peer"),
            &Patch::Merge(patch),
        )
        .await
    {
        tracing::error!(pairing = %pairing_name, "Failed to update pairing status: {e}");
        return pair_error("Internal error — could not bind identity");
    }

    tracing::info!(
        pairing = %pairing_name,
        amid = %from_amid,
        display_name = %display_name.unwrap_or("—"),
        "Pairing successful — AMID bound"
    );

    PairingMessage::PairResponse {
        success: true,
        cluster_name: Some(state.cluster_name.clone()),
        controller_amid: Some(state.identity.amid.clone()),
        capabilities_granted: Some(pairing.spec.capabilities.clone()),
        slots: Some(pairing.spec.slots_max),
        token_budget: Some(pairing.spec.token_budget),
        expires_at: Some(pairing.spec.expires_at.clone()),
        error: None,
    }
}

fn pair_error(message: &str) -> PairingMessage {
    PairingMessage::PairResponse {
        success: false,
        cluster_name: None,
        controller_amid: None,
        capabilities_granted: None,
        slots: None,
        token_budget: None,
        expires_at: None,
        error: Some(message.into()),
    }
}

fn hex_sha256(input: &str) -> String {
    let hash = Sha256::digest(input.as_bytes());
    hex::encode(hash)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_amid_is_deterministic() {
        let key_bytes = [42u8; 32];
        let signing_key = SigningKey::from_bytes(&key_bytes);
        let vk = signing_key.verifying_key();
        let amid1 = derive_amid(&vk);
        let amid2 = derive_amid(&vk);
        assert_eq!(amid1, amid2);
        assert!(!amid1.is_empty());
    }

    #[test]
    fn derive_amid_is_base58() {
        let identity = MeshIdentity::generate();
        // Base58 alphabet: 123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz
        for c in identity.amid.chars() {
            assert!(
                "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz".contains(c),
                "AMID contains non-base58 char: {c}"
            );
        }
    }

    #[test]
    fn different_keys_produce_different_amids() {
        let id1 = MeshIdentity::generate();
        let id2 = MeshIdentity::generate();
        assert_ne!(id1.amid, id2.amid);
    }

    #[test]
    fn sign_timestamp_produces_valid_base64() {
        let identity = MeshIdentity::generate();
        let sig = identity.sign_timestamp("2026-04-15T13:00:00Z");
        assert!(BASE64.decode(&sig).is_ok());
        // Ed25519 signature is 64 bytes → 88 base64 chars
        assert_eq!(BASE64.decode(&sig).unwrap().len(), 64);
    }

    #[test]
    fn public_key_b64_is_32_bytes() {
        let identity = MeshIdentity::generate();
        let pk = identity.public_key_b64();
        let bytes = BASE64.decode(&pk).unwrap();
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn hex_sha256_matches() {
        let hash = hex_sha256("test-secret");
        assert_eq!(hash.len(), 64); // SHA-256 = 32 bytes = 64 hex chars
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn pair_error_response() {
        let resp = pair_error("test error");
        match resp {
            PairingMessage::PairResponse {
                success,
                error,
                cluster_name,
                ..
            } => {
                assert!(!success);
                assert_eq!(error.as_deref(), Some("test error"));
                assert!(cluster_name.is_none());
            }
            _ => panic!("Expected PairResponse"),
        }
    }

    #[test]
    fn pairing_message_serialization() {
        let msg = PairingMessage::PairRequest {
            secret: "abc".into(),
            pubkey_ed25519: "def".into(),
            pubkey_x25519: None,
            display_name: Some("test".into()),
            capabilities_requested: Some(vec!["offload".into()]),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("pair_request"));
        assert!(json.contains("abc"));

        // Roundtrip
        let decoded: PairingMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            PairingMessage::PairRequest { secret, .. } => assert_eq!(secret, "abc"),
            _ => panic!("Wrong variant"),
        }
    }
}
