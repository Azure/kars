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
use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::api::core::v1::Secret;
use kube::{
    Client,
    api::{Api, Patch, PatchParams, PostParams},
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;

mod offload;
mod pair;


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
        #[serde(default)]
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
    /// Sent by the parent (external agent) when an offload has terminated
    /// (completed, errored, or user-cancelled) and the ClawSandbox CRD can
    /// be torn down. Without this, the reconciler keeps the offload pod
    /// alive until idle-timeout, which wastes cluster capacity.
    #[serde(rename = "offload_cleanup")]
    OffloadCleanup {
        request_id: String,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
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
    /// Persistent outbox for background tasks (pod watchers, resumed watchers)
    /// to send messages to peers via the active relay connection. Survives
    /// reconnects: enqueued messages buffered here are drained when the main
    /// loop re-establishes the WebSocket.
    ///
    /// Use `enqueue_outbound()` to enqueue — NEVER open a second relay WS
    /// with the same AMID (the relay enforces one-connection-per-AMID and
    /// will supersede the main connection, knocking the controller offline).
    outbox_tx: tokio::sync::mpsc::UnboundedSender<OutboundMsg>,
    /// Monotonic leader epoch. Incremented each time this pod acquires the
    /// mesh-peer leader lease. Watchers capture the epoch at spawn; stale
    /// epochs are dropped at the drain site so a stranded watcher from a
    /// previous leader tenure cannot corrupt peer state after failover.
    leader_epoch: AtomicU64,
}

/// A message queued for outbound delivery via the active relay connection.
/// Enqueued by background tasks; drained by the main event loop.
#[derive(Debug)]
struct OutboundMsg {
    to: String,
    msg: FederationMessage,
    epoch: u64,
}

const LEASE_NAME: &str = "azureclaw-mesh-peer-leader";
const LEASE_DURATION_SECS: i32 = 30;
const LEASE_RENEW_SECS: u64 = 10;

// ---------------------------------------------------------------------------
// Leader election via Kubernetes Lease
// ---------------------------------------------------------------------------

/// Get a unique holder identity for this pod (hostname or random fallback).
fn holder_identity() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| format!("mesh-peer-{}", std::process::id()))
}

/// Try to acquire or renew the mesh-peer leader Lease.
/// Returns true if this pod is the leader.
async fn try_acquire_lease(client: &Client, namespace: &str) -> bool {
    let leases: Api<Lease> = Api::namespaced(client.clone(), namespace);
    let holder = holder_identity();
    let now = Utc::now();
    let now_rfc3339 = now.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string();

    // Try to get existing lease
    match leases.get(LEASE_NAME).await {
        Ok(existing) => {
            let spec = existing.spec.as_ref();
            let current_holder = spec
                .and_then(|s| s.holder_identity.as_deref())
                .unwrap_or("");
            let renew_time = spec.and_then(|s| s.renew_time.as_ref());
            let duration = spec
                .and_then(|s| s.lease_duration_seconds)
                .unwrap_or(LEASE_DURATION_SECS);

            // Check if the lease is held by us or has expired
            let expired = match renew_time {
                Some(t) => {
                    let renew_secs = t.0.as_second();
                    let now_secs = now.timestamp();
                    (now_secs - renew_secs) > i64::from(duration)
                }
                None => true,
            };

            if current_holder == holder || expired {
                // Acquire or renew — only update acquireTime on fresh acquisition
                let mut patch_spec = json!({
                    "holderIdentity": holder,
                    "leaseDurationSeconds": LEASE_DURATION_SECS,
                    "renewTime": now_rfc3339,
                });
                if expired {
                    patch_spec["acquireTime"] = json!(now_rfc3339);
                }
                let patch = json!({ "spec": patch_spec });
                match leases
                    .patch(LEASE_NAME, &PatchParams::default(), &Patch::Merge(patch))
                    .await
                {
                    Ok(_) => true,
                    Err(e) => {
                        tracing::warn!("Failed to renew lease: {e}");
                        false
                    }
                }
            } else {
                tracing::debug!(
                    current_leader = %current_holder,
                    "Another replica holds the mesh peer lease"
                );
                false
            }
        }
        Err(kube::Error::Api(ae)) if ae.code == 404 => {
            // Create the lease
            let lease = serde_json::from_value(json!({
                "apiVersion": "coordination.k8s.io/v1",
                "kind": "Lease",
                "metadata": {
                    "name": LEASE_NAME,
                    "namespace": namespace,
                },
                "spec": {
                    "holderIdentity": holder,
                    "leaseDurationSeconds": LEASE_DURATION_SECS,
                    "acquireTime": now_rfc3339,
                    "renewTime": now_rfc3339,
                }
            }));
            match lease {
                Ok(l) => match leases.create(&PostParams::default(), &l).await {
                    Ok(_) => {
                        tracing::info!("Created mesh peer leader lease");
                        true
                    }
                    Err(e) => {
                        tracing::debug!("Failed to create lease (race): {e}");
                        false
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to build lease object: {e}");
                    false
                }
            }
        }
        Err(e) => {
            tracing::warn!("Failed to get lease: {e}");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

/// Run the controller mesh peer. Connects to the relay, listens for messages,
/// and handles pairing/offload requests. Reconnects automatically on disconnect.
///
/// Uses a Kubernetes Lease for leader election so only one replica connects
/// to the relay at a time (the relay maps one AMID → one session; two
/// replicas connecting as the same AMID causes connection fighting).
pub async fn run(client: Client) -> Result<()> {
    let relay_url = std::env::var("MESH_RELAY_URL")
        .unwrap_or_else(|_| "ws://agentmesh-relay.agentmesh.svc.cluster.local:8765".into());
    let cluster_name = std::env::var("CLUSTER_NAME").unwrap_or_else(|_| "azureclaw-cluster".into());
    let namespace =
        std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());

    let identity = load_or_create_identity(&client).await?;
    tracing::info!(
        amid = %identity.amid,
        relay = %relay_url,
        holder = %holder_identity(),
        "Controller mesh peer starting"
    );

    // Persistent outbox — created ONCE for the life of the controller. Survives
    // reconnects and leader failover. The receiver stays owned by this function
    // and is passed by mutable reference into each connect_and_listen iteration.
    let (outbox_tx, mut outbox_rx) = tokio::sync::mpsc::unbounded_channel::<OutboundMsg>();

    let state = Arc::new(MeshPeerState {
        identity,
        client,
        relay_url,
        cluster_name,
        outbox_tx,
        leader_epoch: AtomicU64::new(0),
    });

    loop {
        // Leader election — wait until we hold the lease
        if !try_acquire_lease(&state.client, &namespace).await {
            tokio::time::sleep(Duration::from_secs(LEASE_RENEW_SECS)).await;
            continue;
        }

        // Bump leader epoch so any enqueues from prior tenures (possibly
        // lingering in outbox_rx) are identified as stale and dropped.
        let new_epoch = state.leader_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        tracing::info!(
            holder = %holder_identity(),
            epoch = new_epoch,
            "Acquired mesh peer leader lease"
        );

        // Purge any stale outbox items queued under prior epochs before we
        // start servicing new work — keeps the drain site cheap.
        let mut purged = 0u32;
        while let Ok(stale) = outbox_rx.try_recv() {
            if stale.epoch < new_epoch {
                purged += 1;
            } else {
                // Shouldn't happen (we just bumped), but be defensive: put it back.
                let _ = state.outbox_tx.send(stale);
                break;
            }
        }
        if purged > 0 {
            tracing::warn!(
                purged,
                "Dropped stale outbox messages from prior leader tenure"
            );
        }

        // Resume any offload watchers stranded by a prior controller restart.
        // This fires exactly when we become leader so only one replica drives
        // the resume (watchers would otherwise double-send ready events).
        if let Err(e) = offload::resume_pending_offload_watchers(&state).await {
            tracing::warn!("Failed to resume pending offload watchers: {e:#}");
        }

        // Connect and listen, renewing the lease periodically
        let state_inner = state.clone();
        let lease_ns = namespace.clone();
        let lease_client = state.client.clone();
        let result = tokio::select! {
            res = connect_and_listen(state_inner, &mut outbox_rx, new_epoch) => res,
            _ = async {
                // Lease renewal loop — if renewal fails, we lost leadership
                loop {
                    tokio::time::sleep(Duration::from_secs(LEASE_RENEW_SECS)).await;
                    if !try_acquire_lease(&lease_client, &lease_ns).await {
                        tracing::warn!("Lost mesh peer leader lease — disconnecting");
                        break;
                    }
                }
            } => Err(anyhow::anyhow!("Lost leader lease")),
        };

        match result {
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
///
/// `outbox_rx` is the persistent receiver for background-task sends (pod
/// watchers). Held by the caller across reconnects so buffered messages
/// survive. `leader_epoch` is the epoch captured when we became leader;
/// stale outbox items (epoch < current) are dropped at drain time.
async fn connect_and_listen(
    state: Arc<MeshPeerState>,
    outbox_rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutboundMsg>,
    leader_epoch: u64,
) -> Result<()> {
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(&state.relay_url)
        .await
        .context("Failed to connect to relay")?;

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
    ws_stream
        .send(WsMessage::Text(serde_json::to_string(&connect_msg)?.into()))
        .await?;

    // Channel for outgoing messages — handlers and keepalive send through this.
    // The main loop owns the WebSocket and writes from this channel, ensuring
    // auto-queued Pong frames are flushed on every write/read cycle.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<WsMessage>();

    // Spawn keepalive (sends JSON-level Ping through the channel)
    let ping_tx = out_tx.clone();
    let ping_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let ping = RelayMessage::Ping {
                timestamp: chrono::Utc::now().to_rfc3339(),
            };
            if ping_tx
                .send(WsMessage::Text(
                    serde_json::to_string(&ping).unwrap_or_default().into(),
                ))
                .is_err()
            {
                break;
            }
        }
    });

    // Connection-ready flag: set true when we receive RelayMessage::Connected.
    // The outbox drain waits on this so we don't push messages before the
    // relay has authenticated us.
    let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Terminal-error signal: spawned message handlers push a reason here when
    // they see a fatal relay error (SESSION_REPLACED, PING_TIMEOUT). The main
    // loop breaks on receipt so the outer reconnect kicks in.
    let (terminate_tx, mut terminate_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // Main event loop — single owner of ws_stream so Pong auto-flush works.
    loop {
        tokio::select! {
            // Incoming WS frame
            frame = tokio::time::timeout(Duration::from_secs(90), ws_stream.next()) => {
                match frame {
                    Ok(Some(Ok(msg))) => {
                        match msg {
                            WsMessage::Text(text) => {
                                let state_clone = state.clone();
                                let tx_clone = out_tx.clone();
                                let connected_clone = connected.clone();
                                let term_clone = terminate_tx.clone();
                                tokio::spawn(async move {
                                    if let Err(e) =
                                        handle_message(&state_clone, &tx_clone, &connected_clone, &term_clone, &text).await
                                    {
                                        tracing::warn!("Error handling message: {e:#}");
                                    }
                                });
                            }
                            WsMessage::Ping(data) => {
                                tracing::debug!("WS Ping received — sending Pong");
                                ws_stream.send(WsMessage::Pong(data)).await.ok();
                            }
                            WsMessage::Pong(_) => {
                                tracing::debug!("WS Pong received");
                            }
                            WsMessage::Close(_) => {
                                tracing::info!("Relay sent close frame");
                                break;
                            }
                            WsMessage::Frame(_) => {
                                tracing::debug!("WS raw Frame received");
                            }
                            _ => {
                                tracing::debug!("WS other frame type received");
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        return Err(anyhow::anyhow!("WebSocket read error: {e:#}"));
                    }
                    Ok(None) => break, // stream ended
                    Err(_) => {
                        tracing::warn!("No data from relay in 90s — connection likely stale, reconnecting");
                        break;
                    }
                }
            }
            // Outgoing message from per-connection handlers (pair response, etc.)
            Some(msg) = out_rx.recv() => {
                ws_stream.send(msg).await?;
            }
            // Outgoing message from background tasks (pod watchers). Gated on
            // `connected` so we don't send before the relay authenticates us.
            Some(obmsg) = outbox_rx.recv(), if connected.load(Ordering::Acquire) => {
                if obmsg.epoch < leader_epoch {
                    tracing::debug!(
                        msg_epoch = obmsg.epoch,
                        current_epoch = leader_epoch,
                        to = %obmsg.to,
                        "Dropping stale outbox message from prior leader tenure"
                    );
                    continue;
                }
                match serialize_and_send_outbound(&mut ws_stream, &obmsg).await {
                    Ok(()) => {}
                    Err(e) => {
                        // Re-enqueue through the persistent outbox so the
                        // next connection can retry this exact message.
                        let _ = state.outbox_tx.send(obmsg);
                        ping_handle.abort();
                        return Err(e).context("Outbox send failed — saved for retry");
                    }
                }
            }
            // Fatal relay error — break out and let the outer loop reconnect.
            Some(reason) = terminate_rx.recv() => {
                tracing::warn!(reason = %reason, "Terminating relay connection due to fatal error");
                break;
            }
        }
    }

    // If we broke out with a send error, we already re-enqueued the message
    // into the persistent outbox — it'll be retried when the next connection
    // comes up.

    ping_handle.abort();
    Ok(())
}

/// Serialize a FederationMessage into the relay wire format and send it over
/// the current WebSocket. Matches the legacy `send_via_relay` wire format
/// (base64-encoded plain JSON in `encrypted_payload`, message_type="message").
async fn serialize_and_send_outbound(
    ws_stream: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    out: &OutboundMsg,
) -> Result<()> {
    let json = serde_json::to_string(&out.msg)?;
    let b64 = BASE64.encode(json.as_bytes());
    let send_msg = RelayMessage::Send {
        to: out.to.clone(),
        encrypted_payload: b64,
        message_type: "message".into(),
    };
    ws_stream
        .send(WsMessage::Text(serde_json::to_string(&send_msg)?.into()))
        .await?;
    Ok(())
}

/// Enqueue a federation message for delivery to a peer via the active relay
/// connection. Non-blocking; the message is buffered in the persistent outbox
/// and drained by the main event loop when connected.
///
/// `epoch` MUST be the leader epoch captured at the caller's spawn time —
/// stale epochs from prior leader tenures are dropped at the drain site to
/// prevent zombie watchers from corrupting peer state after failover.
///
/// Use this instead of opening a second WebSocket to the relay — doing that
/// with the same AMID will cause the relay to supersede the controller's
/// main connection (SESSION_REPLACED) and knock the controller mesh-offline.
fn enqueue_outbound(
    state: &Arc<MeshPeerState>,
    epoch: u64,
    to: &str,
    msg: FederationMessage,
) -> Result<()> {
    state
        .outbox_tx
        .send(OutboundMsg {
            to: to.to_string(),
            msg,
            epoch,
        })
        .map_err(|_| anyhow::anyhow!("mesh outbox closed (receiver dropped)"))
}

/// Handle a single relay message.
async fn handle_message(
    state: &Arc<MeshPeerState>,
    out_tx: &tokio::sync::mpsc::UnboundedSender<WsMessage>,
    connected: &Arc<std::sync::atomic::AtomicBool>,
    terminate_tx: &tokio::sync::mpsc::UnboundedSender<String>,
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
            connected.store(true, Ordering::Release);
        }
        RelayMessage::Receive {
            from,
            encrypted_payload,
            message_type,
            ..
        } => {
            if message_type == "message" || message_type == "optimistic_message" {
                handle_peer_message(state, out_tx, &from, &encrypted_payload).await?;
            } else {
                tracing::debug!(from = %from, msg_type = %message_type, "Ignoring relay message");
            }
        }
        RelayMessage::Pong { .. } => {}
        RelayMessage::Error { code, message } => {
            tracing::warn!(code = %code, "Relay error: {message}");
            // SESSION_REPLACED: our main connection was just killed by a newer
            // session for the same AMID (usually self-inflicted by a rogue
            // second WS). PING_TIMEOUT: relay gave up on us. Both are fatal —
            // trigger reconnect instead of silently continuing with a dead WS.
            if code == "SESSION_REPLACED" || code == "PING_TIMEOUT" {
                let _ = terminate_tx.send(format!("relay error {code}: {message}"));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Handle a message from a peer. Decode and dispatch by type.
async fn handle_peer_message(
    state: &Arc<MeshPeerState>,
    out_tx: &tokio::sync::mpsc::UnboundedSender<WsMessage>,
    from_amid: &str,
    payload_b64: &str,
) -> Result<()> {
    // Decode base64 payload → JSON
    let payload_bytes = BASE64.decode(payload_b64).unwrap_or_default();
    let payload_str = String::from_utf8_lossy(&payload_bytes);

    let msg: FederationMessage = match serde_json::from_str(&payload_str) {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!(from = %from_amid, err = %e, payload = %payload_str.chars().take(200).collect::<String>(), "Unrecognized message format — ignoring");
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
            let response = pair::handle_pair_request(
                state,
                from_amid,
                &secret,
                &pubkey_ed25519,
                display_name.as_deref(),
            )
            .await;
            send_to_peer(out_tx, from_amid, &response).await?;
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
            offload::handle_offload_request(
                state,
                out_tx,
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
        FederationMessage::OffloadCleanup {
            request_id, reason, ..
        } => {
            tracing::info!(
                from = %from_amid,
                request_id = %request_id,
                reason = reason.as_deref().unwrap_or("unspecified"),
                "Received offload_cleanup"
            );
            if let Err(e) = offload::handle_offload_cleanup(state, from_amid, &request_id).await {
                tracing::warn!(
                    request_id = %request_id,
                    err = %e,
                    "offload_cleanup handler failed"
                );
            }
        }
        _ => {
            tracing::debug!(from = %from_amid, "Ignoring unhandled federation message");
        }
    }

    Ok(())
}


fn hex_sha256(input: &str) -> String {
    let hash = Sha256::digest(input.as_bytes());
    hex::encode(hash)
}

// ---------------------------------------------------------------------------
// Send helpers
// ---------------------------------------------------------------------------

/// Send a message to a peer via the current WebSocket (used in request handlers).
async fn send_to_peer(
    out_tx: &tokio::sync::mpsc::UnboundedSender<WsMessage>,
    to_amid: &str,
    msg: &FederationMessage,
) -> Result<()> {
    let json = serde_json::to_string(msg)?;
    let b64 = BASE64.encode(json.as_bytes());
    let send_msg = RelayMessage::Send {
        to: to_amid.to_string(),
        encrypted_payload: b64,
        message_type: "message".into(),
    };
    out_tx
        .send(WsMessage::Text(serde_json::to_string(&send_msg)?.into()))
        .context("WebSocket channel closed")?;
    Ok(())
}

// NOTE: `send_via_relay` (which opened a second WebSocket with the same AMID)
// was removed. That pattern caused the relay to supersede the controller's
// main connection every time a background watcher sent a message, knocking
// the controller mesh-offline. Background tasks now use `enqueue_outbound`
// which routes through the persistent outbox drained by the active main WS.
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
        let resp = pair::pair_error("test error");
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
