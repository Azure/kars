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
use k8s_openapi::api::core::v1::{Pod, Secret};
use kube::{
    Client, ResourceExt,
    api::{Api, ListParams, Patch, PatchParams, PostParams},
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
pub(crate) const IDENTITY_NAMESPACE: &str = "azureclaw-system";

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
// Federation protocol messages (carried inside encrypted_payload as base64 JSON)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum FederationMessage {
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
    #[serde(rename = "offload_request")]
    OffloadRequest {
        task: String,
        #[serde(default)]
        files: Vec<String>,
        #[serde(default)]
        file_count: usize,
        #[serde(default)]
        total_bytes: u64,
        #[serde(default)]
        file_contents: Vec<FileContent>,
        #[serde(default)]
        preferences: Option<OffloadPreferences>,
        request_id: String,
        timestamp: String,
    },
    #[serde(rename = "offload_status")]
    OffloadStatus {
        request_id: String,
        phase: String,
        message: String,
        /// Sandbox name (set when phase == "ready") — the external agent uses
        /// this to discover the sandbox on the mesh and talk to it directly.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sandbox_name: Option<String>,
    },
    #[serde(rename = "offload_done")]
    OffloadDone {
        request_id: String,
        summary: String,
        #[serde(default)]
        output_files: Vec<String>,
        #[serde(default)]
        output_file_contents: Vec<FileContent>,
        #[serde(default)]
        tokens_used: Option<TokenUsage>,
        #[serde(default)]
        duration_seconds: u64,
    },
    #[serde(rename = "offload_error")]
    OffloadError {
        request_id: String,
        error: String,
        phase: String,
    },
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct OffloadPreferences {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    max_tokens: Option<i64>,
    #[serde(default)]
    timeout_minutes: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct TokenUsage {
    #[serde(default)]
    prompt: u64,
    #[serde(default)]
    completion: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct FileContent {
    path: String,
    data_b64: String,
    size: u64,
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
    // Default to in-cluster relay service. External agents use the public ingress
    // URL (embedded in pairing tokens via `azureclaw pair generate --relay-url`).
    let relay_url = std::env::var("MESH_RELAY_URL")
        .unwrap_or_else(|_| "ws://agentmesh-relay.agentmesh.svc.cluster.local:8765".into());
    let cluster_name = std::env::var("CLUSTER_NAME").unwrap_or_else(|_| "azureclaw-cluster".into());

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
            if w.send(WsMessage::Text(
                serde_json::to_string(&ping).unwrap_or_default().into(),
            ))
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
    state: &Arc<MeshPeerState>,
    write: &Arc<
        RwLock<impl SinkExt<WsMessage, Error = tokio_tungstenite::tungstenite::Error> + Unpin>,
    >,
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
    state: &Arc<MeshPeerState>,
    write: &Arc<
        RwLock<impl SinkExt<WsMessage, Error = tokio_tungstenite::tungstenite::Error> + Unpin>,
    >,
    from_amid: &str,
    payload_b64: &str,
) -> Result<()> {
    // Decode base64 payload → JSON
    let payload_bytes = BASE64.decode(payload_b64).unwrap_or_default();
    let payload_str = String::from_utf8_lossy(&payload_bytes);

    let msg: FederationMessage = match serde_json::from_str(&payload_str) {
        Ok(m) => m,
        Err(_) => {
            tracing::debug!(from = %from_amid, "Unrecognized message format — ignoring");
            return Ok(());
        }
    };

    match msg {
        FederationMessage::PairRequest {
            secret,
            pubkey_ed25519,
            display_name,
            ..
        } => {
            tracing::info!(from = %from_amid, "Received pair_request");
            let response = handle_pair_request(
                state,
                from_amid,
                &secret,
                &pubkey_ed25519,
                display_name.as_deref(),
            )
            .await;
            send_to_peer(write, from_amid, &response).await?;
        }
        FederationMessage::OffloadRequest {
            task,
            files,
            file_count,
            total_bytes,
            file_contents,
            preferences,
            request_id,
            timestamp,
        } => {
            tracing::info!(
                from = %from_amid,
                request_id = %request_id,
                task_len = task.len(),
                files = file_count,
                inline_files = file_contents.len(),
                "Received offload_request"
            );
            handle_offload_request(
                state,
                write,
                from_amid,
                &request_id,
                &task,
                &files,
                total_bytes,
                &file_contents,
                preferences.as_ref(),
                &timestamp,
            )
            .await?;
        }
        FederationMessage::PairResponse { .. } => {
            tracing::debug!(from = %from_amid, "Ignoring pair_response (we are the controller)");
        }
        _ => {
            tracing::debug!(from = %from_amid, "Ignoring unhandled federation message");
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
) -> FederationMessage {
    let token_hash = hex_sha256(secret);

    // List all ClawPairing CRDs and find matching token_hash
    let pairings: Api<ClawPairing> = Api::namespaced(state.client.clone(), IDENTITY_NAMESPACE);
    let pairing_list = match pairings.list(&kube::api::ListParams::default()).await {
        Ok(list) => list,
        Err(e) => {
            tracing::error!("Failed to list ClawPairings: {e}");
            return pair_error("Internal error — could not verify token");
        }
    };

    // Find matching pairing
    let matching = pairing_list
        .items
        .iter()
        .find(|p| p.spec.token_hash == token_hash);
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
        return pair_error(&format!(
            "Pairing is {current_phase} — token already consumed or expired"
        ));
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

    FederationMessage::PairResponse {
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

fn pair_error(message: &str) -> FederationMessage {
    FederationMessage::PairResponse {
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
// Send helpers
// ---------------------------------------------------------------------------

/// Send a message to a peer via the current WebSocket (used in request handlers).
async fn send_to_peer<S>(
    write: &Arc<RwLock<S>>,
    to_amid: &str,
    msg: &FederationMessage,
) -> Result<()>
where
    S: SinkExt<WsMessage, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let json = serde_json::to_string(msg)?;
    let b64 = BASE64.encode(json.as_bytes());
    let send_msg = RelayMessage::Send {
        to: to_amid.to_string(),
        encrypted_payload: b64,
        message_type: "message".into(),
    };
    let mut w = write.write().await;
    w.send(WsMessage::Text(serde_json::to_string(&send_msg)?.into()))
        .await?;
    Ok(())
}

/// Send a message to a peer via a fresh relay connection.
/// Used by background tasks (pod watchers) that outlive the original WebSocket.
async fn send_via_relay(
    state: &MeshPeerState,
    to_amid: &str,
    msg: &FederationMessage,
) -> Result<()> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(&state.relay_url)
        .await
        .context("Failed to connect to relay for result relay")?;

    let (mut write, _read) = ws_stream.split();

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
    write
        .send(WsMessage::Text(serde_json::to_string(&connect_msg)?.into()))
        .await?;

    // Brief pause to let the relay process auth
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Send the actual message
    let json = serde_json::to_string(msg)?;
    let b64 = BASE64.encode(json.as_bytes());
    let send_msg = RelayMessage::Send {
        to: to_amid.to_string(),
        encrypted_payload: b64,
        message_type: "message".into(),
    };
    write
        .send(WsMessage::Text(serde_json::to_string(&send_msg)?.into()))
        .await?;

    // Close cleanly
    let _ = write.send(WsMessage::Close(None)).await;

    tracing::debug!(to = %to_amid, "Sent result via dedicated relay connection");
    Ok(())
}

// ---------------------------------------------------------------------------
// Offload orchestration
// ---------------------------------------------------------------------------

/// Handle an offload_request from a paired external agent.
/// Validates the pairing, checks budget/slots, creates file ConfigMap if needed,
/// creates a ClawSandbox CRD, watches pod completion, and relays results back.
#[allow(clippy::too_many_arguments)]
async fn handle_offload_request<S>(
    state: &Arc<MeshPeerState>,
    write: &Arc<RwLock<S>>,
    from_amid: &str,
    request_id: &str,
    task: &str,
    _files: &[String],
    _total_bytes: u64,
    _file_contents: &[FileContent],
    preferences: Option<&OffloadPreferences>,
    _timestamp: &str,
) -> Result<()>
where
    S: SinkExt<WsMessage, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    // Phase 1: Validate pairing
    send_to_peer(
        write,
        from_amid,
        &FederationMessage::OffloadStatus {
            request_id: request_id.into(),
            phase: "validating".into(),
            message: "Validating pairing and budget".into(),
            sandbox_name: None,
        },
    )
    .await?;

    let pairing = match validate_pairing_for_offload(state, from_amid).await {
        Ok(p) => p,
        Err(e) => {
            send_to_peer(
                write,
                from_amid,
                &FederationMessage::OffloadError {
                    request_id: request_id.into(),
                    error: e.clone(),
                    phase: "validating".into(),
                },
            )
            .await?;
            return Ok(());
        }
    };

    // Phase 2: Create offload sandbox
    send_to_peer(
        write,
        from_amid,
        &FederationMessage::OffloadStatus {
            request_id: request_id.into(),
            phase: "spawning".into(),
            message: "Creating offload sandbox".into(),
            sandbox_name: None,
        },
    )
    .await?;

    let sandbox_name = format!("offload-{}", &request_id[..8]);
    let model = preferences
        .and_then(|p| p.model.as_deref())
        .unwrap_or("gpt-4.1");
    let timeout_minutes = preferences.and_then(|p| p.timeout_minutes).unwrap_or(30);
    let namespace =
        std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());

    let spec = json!({
        "openclaw": {
            "version": "2026.3.13",
            "config": {
                "agent": {
                    "model": format!("azure/{model}")
                }
            }
        },
        "sandbox": {
            "isolation": "enhanced",
            "readOnlyRootFilesystem": true,
            "runAsNonRoot": true,
            "allowPrivilegeEscalation": false
        },
        "inference": {
            "provider": "azure-ai-foundry",
            "model": model,
            "contentSafety": true,
            "promptShields": true,
            "tokenBudget": {
                "daily": pairing.spec.token_budget,
                "perRequest": 32000
            }
        },
        "networkPolicy": {
            "defaultDeny": true,
            "approvalRequired": true,
            "learnEgress": false,
        },
        "governance": {
            "enabled": true,
            "toolPolicy": "default",
            "trustThreshold": 900,
            "trustedPeers": format!("offload-parent:{from_amid}"),
            "registryMode": "global"
        }
    });

    let crd = json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "ClawSandbox",
        "metadata": {
            "name": sandbox_name,
            "namespace": namespace,
            "labels": {
                "azureclaw.azure.com/spawned-by": "offload",
                "azureclaw.azure.com/offload-requester": from_amid,
                "azureclaw.azure.com/request-id": request_id,
            },
            "annotations": {
                "azureclaw.azure.com/offload-task": &task[..task.len().min(256)],
                "azureclaw.azure.com/offload-timeout": format!("{timeout_minutes}m"),
                "azureclaw.azure.com/offload-parent-amid": from_amid,
            }
        },
        "spec": spec,
    });

    // No OFFLOAD_MODE — sandbox starts as a full AzureClaw agent.
    // The external agent talks to it directly via existing mesh protocol
    // (mesh_send, mesh_transfer_file). AGT_TRUSTED_PEERS locks the sandbox
    // so only the paired external agent can communicate with it.
    let extra_env = json!({
        "OFFLOAD_REQUEST_ID": request_id,
        "OFFLOAD_PARENT_AMID": from_amid,
        "OFFLOAD_TIMEOUT_MINUTES": timeout_minutes.to_string(),
    });

    // Create via K8s API
    let api_resource = kube::api::ApiResource {
        group: "azureclaw.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "azureclaw.azure.com/v1alpha1".into(),
        kind: "ClawSandbox".into(),
        plural: "clawsandboxes".into(),
    };
    let api: Api<kube::api::DynamicObject> =
        Api::namespaced_with(state.client.clone(), &namespace, &api_resource);

    // Merge extra env into the CRD spec
    let mut crd_value = crd;
    if let Some(openclaw) = crd_value["spec"]["openclaw"].as_object_mut() {
        openclaw.insert("extraEnv".to_string(), extra_env);
    }

    let obj: kube::api::DynamicObject = match serde_json::from_value(crd_value) {
        Ok(o) => o,
        Err(e) => {
            send_to_peer(
                write,
                from_amid,
                &FederationMessage::OffloadError {
                    request_id: request_id.into(),
                    error: format!("Failed to build sandbox CRD: {e}"),
                    phase: "spawning".into(),
                },
            )
            .await?;
            return Ok(());
        }
    };

    match api.create(&PostParams::default(), &obj).await {
        Ok(_) => {
            tracing::info!(
                sandbox = %sandbox_name,
                requester = %from_amid,
                request_id = %request_id,
                "Offload sandbox created"
            );
        }
        Err(e) => {
            tracing::error!(
                sandbox = %sandbox_name,
                "Failed to create offload sandbox: {e}"
            );
            send_to_peer(
                write,
                from_amid,
                &FederationMessage::OffloadError {
                    request_id: request_id.into(),
                    error: format!("Failed to create sandbox: {e}"),
                    phase: "spawning".into(),
                },
            )
            .await?;
            return Ok(());
        }
    }

    // Update pairing usage
    let pairing_name = pairing.name_any();
    let pairings_api: Api<ClawPairing> = Api::namespaced(state.client.clone(), IDENTITY_NAMESPACE);
    let usage_patch = json!({
        "status": {
            "slotsUsed": pairing.status.as_ref().and_then(|s| s.slots_used).unwrap_or(0) + 1,
            "lastOffloadAt": chrono::Utc::now().to_rfc3339(),
        }
    });
    let _ = pairings_api
        .patch_status(
            &pairing_name,
            &PatchParams::apply("azureclaw-mesh-peer"),
            &Patch::Merge(usage_patch),
        )
        .await;

    // Phase 3: CRD created — notify requester sandbox is being scheduled
    send_to_peer(
        write,
        from_amid,
        &FederationMessage::OffloadStatus {
            request_id: request_id.into(),
            phase: "scheduled".into(),
            message: format!(
                "Sandbox '{sandbox_name}' created, waiting for it to start..."
            ),
            sandbox_name: None,
        },
    )
    .await?;

    // Phase 4: Watch for sandbox to become Running, then send sandbox name
    // back to the requester so they can talk to it directly via mesh.
    let watcher_state = Arc::clone(state);
    let watcher_amid = from_amid.to_string();
    let watcher_request_id = request_id.to_string();
    let watcher_sandbox = sandbox_name.clone();
    let watcher_ns = namespace.clone();

    tokio::spawn(async move {
        if let Err(e) = watch_sandbox_ready(
            &watcher_state,
            &watcher_amid,
            &watcher_request_id,
            &watcher_sandbox,
            &watcher_ns,
        )
        .await
        {
            tracing::error!(
                request_id = %watcher_request_id,
                "Sandbox ready watcher failed: {e:#}"
            );
        }
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// Sandbox ready watcher
// ---------------------------------------------------------------------------

/// Watch for an offload sandbox pod to become Running, then send its name back
/// to the requester so they can talk to it directly via the mesh.
///
/// The controller's job ends here — the external agent and sandbox communicate
/// directly via the existing mesh protocol (mesh_send, mesh_transfer_file).
/// AGT_TRUSTED_PEERS on the sandbox ensures only the paired agent can talk to it.
async fn watch_sandbox_ready(
    state: &Arc<MeshPeerState>,
    requester_amid: &str,
    request_id: &str,
    sandbox_name: &str,
    namespace: &str,
) -> Result<()> {
    let pods: Api<Pod> = Api::namespaced(state.client.clone(), namespace);
    let label_selector = format!("azureclaw.azure.com/request-id={request_id}");
    // 5 minutes should be enough for pod scheduling + container pull + startup
    let timeout = Duration::from_secs(300);

    tracing::info!(
        request_id = %request_id,
        sandbox = %sandbox_name,
        namespace = %namespace,
        "Watching for offload sandbox to become ready"
    );

    let ready_pod = tokio::time::timeout(timeout, async {
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let list = pods
                .list(&ListParams::default().labels(&label_selector))
                .await;
            let pod_list = match list {
                Ok(l) => l,
                Err(e) => {
                    tracing::debug!(request_id = %request_id, "Pod list error: {e}");
                    continue;
                }
            };

            for pod in &pod_list {
                if let Some(status) = &pod.status {
                    let phase = status.phase.as_deref().unwrap_or("");
                    match phase {
                        "Running" => return Ok::<Pod, anyhow::Error>(pod.clone()),
                        "Failed" => {
                            anyhow::bail!("Sandbox pod failed before becoming ready");
                        }
                        _ => {}
                    }
                }
            }
        }
    })
    .await;

    match ready_pod {
        Ok(Ok(_pod)) => {
            tracing::info!(
                request_id = %request_id,
                sandbox = %sandbox_name,
                "Offload sandbox is running — sending name to requester"
            );

            // Send sandbox name so the external agent can discover it on the mesh
            // and talk to it directly via mesh_send / mesh_transfer_file
            send_via_relay(
                state,
                requester_amid,
                &FederationMessage::OffloadStatus {
                    request_id: request_id.into(),
                    phase: "ready".into(),
                    message: format!("Sandbox '{sandbox_name}' is running — send files and task directly via mesh"),
                    sandbox_name: Some(sandbox_name.into()),
                },
            )
            .await?;
        }
        Ok(Err(e)) => {
            tracing::error!(request_id = %request_id, "Sandbox failed: {e}");
            send_via_relay(
                state,
                requester_amid,
                &FederationMessage::OffloadError {
                    request_id: request_id.into(),
                    error: format!("Sandbox failed to start: {e}"),
                    phase: "spawning".into(),
                },
            )
            .await?;
        }
        Err(_) => {
            tracing::warn!(request_id = %request_id, "Sandbox did not become ready within 5 minutes");
            send_via_relay(
                state,
                requester_amid,
                &FederationMessage::OffloadError {
                    request_id: request_id.into(),
                    error: "Sandbox timed out waiting to become ready".into(),
                    phase: "spawning".into(),
                },
            )
            .await?;
        }
    }

    Ok(())
}

/// Validate that an AMID has an active pairing with available slots and budget.
async fn validate_pairing_for_offload(
    state: &MeshPeerState,
    from_amid: &str,
) -> Result<ClawPairing, String> {
    let pairings: Api<ClawPairing> = Api::namespaced(state.client.clone(), IDENTITY_NAMESPACE);
    let pairing_list = pairings
        .list(&kube::api::ListParams::default())
        .await
        .map_err(|e| format!("Internal error — could not list pairings: {e}"))?;

    let matching = pairing_list
        .items
        .into_iter()
        .find(|p| p.status.as_ref().and_then(|s| s.bound_amid.as_deref()) == Some(from_amid));

    let pairing = matching.ok_or_else(|| {
        format!("No pairing found for AMID {from_amid}. Pair first with mesh_pair.")
    })?;

    let status = pairing.status.as_ref();
    let current_phase = status.and_then(|s| s.phase.as_deref()).unwrap_or("");

    if current_phase != phase::ACTIVE {
        return Err(format!("Pairing is '{current_phase}' — must be Active"));
    }

    // Check expiry
    if let Ok(expiry) = chrono::DateTime::parse_from_rfc3339(&pairing.spec.expires_at)
        && chrono::Utc::now() >= expiry.to_utc()
    {
        return Err("Pairing has expired".into());
    }

    // Check slots
    let slots_used = status.and_then(|s| s.slots_used).unwrap_or(0);
    if slots_used >= pairing.spec.slots_max {
        return Err(format!(
            "No available slots ({slots_used}/{} used)",
            pairing.spec.slots_max
        ));
    }

    // Check capability
    if !pairing.spec.capabilities.contains(&"offload".to_string()) {
        return Err("Pairing does not include 'offload' capability".into());
    }

    Ok(pairing)
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
            FederationMessage::PairResponse {
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
        let msg = FederationMessage::PairRequest {
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
        let decoded: FederationMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            FederationMessage::PairRequest { secret, .. } => assert_eq!(secret, "abc"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn offload_request_serialization_roundtrip() {
        let msg = FederationMessage::OffloadRequest {
            task: "Analyze the dataset".into(),
            files: vec!["data.csv".into()],
            file_count: 1,
            total_bytes: 4096,
            file_contents: vec![],
            preferences: Some(OffloadPreferences {
                model: Some("gpt-4.1".into()),
                max_tokens: None,
                timeout_minutes: Some(15),
            }),
            request_id: "req-001".into(),
            timestamp: "2026-04-15T14:00:00Z".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("offload_request"));
        assert!(json.contains("Analyze the dataset"));

        let decoded: FederationMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            FederationMessage::OffloadRequest {
                task, request_id, ..
            } => {
                assert_eq!(task, "Analyze the dataset");
                assert_eq!(request_id, "req-001");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn offload_status_serialization() {
        let msg = FederationMessage::OffloadStatus {
            request_id: "req-001".into(),
            phase: "running".into(),
            message: "Task in progress".into(),
            sandbox_name: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("offload_status"));
        assert!(json.contains("running"));
        // sandbox_name omitted when None
        assert!(!json.contains("sandbox_name"));

        // With sandbox_name
        let msg2 = FederationMessage::OffloadStatus {
            request_id: "req-002".into(),
            phase: "ready".into(),
            message: "Sandbox ready".into(),
            sandbox_name: Some("offload-abc123".into()),
        };
        let json2 = serde_json::to_string(&msg2).unwrap();
        assert!(json2.contains("offload-abc123"));
    }

    #[test]
    fn offload_error_serialization() {
        let msg = FederationMessage::OffloadError {
            request_id: "req-001".into(),
            error: "Budget exceeded".into(),
            phase: "validating".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: FederationMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            FederationMessage::OffloadError { error, .. } => {
                assert_eq!(error, "Budget exceeded");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn offload_done_serialization_roundtrip() {
        let msg = FederationMessage::OffloadDone {
            request_id: "req-done".into(),
            summary: "Task completed successfully".into(),
            output_files: vec!["report.md".into(), "data.json".into()],
            output_file_contents: vec![],
            tokens_used: Some(TokenUsage {
                prompt: 1500,
                completion: 800,
            }),
            duration_seconds: 120,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("offload_done"));
        assert!(json.contains("report.md"));

        let decoded: FederationMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            FederationMessage::OffloadDone {
                summary,
                output_files,
                duration_seconds,
                ..
            } => {
                assert_eq!(summary, "Task completed successfully");
                assert_eq!(output_files.len(), 2);
                assert_eq!(duration_seconds, 120);
            }
            _ => panic!("Wrong variant"),
        }
    }
}
