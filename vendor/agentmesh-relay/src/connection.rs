use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{accept_async, WebSocketStream};
use futures_util::{StreamExt, SinkExt, stream::SplitSink, stream::SplitStream};
use tokio_tungstenite::tungstenite::Message;
use dashmap::DashMap;
use uuid::Uuid;
use chrono::Utc;
use tracing::{info, warn, error, debug};

use crate::types::*;
use crate::store_forward::StoreForward;
use crate::auth;

/// Ping interval in seconds (Railway/cloud providers kill idle connections after 30-60s)
const PING_INTERVAL_SECS: u64 = 25;
/// Ping timeout - if no pong received within this time, disconnect
const PING_TIMEOUT_SECS: u64 = 10;

/// Manages all active agent connections
pub struct ConnectionManager {
    /// Map of AMID -> sender channel for that agent
    connections: DashMap<Amid, mpsc::UnboundedSender<RelayMessage>>,
    /// Agent metadata
    agents: DashMap<Amid, AgentConnection>,
    /// Rate limiting state per AMID
    rate_limits: DashMap<Amid, RateLimitState>,
    /// Configuration
    config: RelayConfig,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            connections: DashMap::new(),
            agents: DashMap::new(),
            rate_limits: DashMap::new(),
            config: RelayConfig::default(),
        }
    }

    /// Register a new connection
    pub fn register(
        &self,
        amid: Amid,
        sender: mpsc::UnboundedSender<RelayMessage>,
        p2p_capable: bool,
    ) -> Uuid {
        let session_id = Uuid::new_v4();
        let now = Utc::now();

        self.connections.insert(amid.clone(), sender);
        self.agents.insert(amid.clone(), AgentConnection {
            amid: amid.clone(),
            session_id,
            status: PresenceStatus::Online,
            connected_at: now,
            last_activity: now,
            p2p_capable,
        });
        self.rate_limits.insert(amid, RateLimitState::default());

        session_id
    }

    /// Unregister a connection
    pub fn unregister(&self, amid: &Amid) {
        self.connections.remove(amid);
        self.agents.remove(amid);
        self.rate_limits.remove(amid);
    }

    /// Update agent presence status
    pub fn update_status(&self, amid: &Amid, status: PresenceStatus) {
        if let Some(mut agent) = self.agents.get_mut(amid) {
            agent.status = status;
            agent.last_activity = Utc::now();
        }
    }

    /// Get agent status
    pub fn get_status(&self, amid: &Amid) -> Option<PresenceStatus> {
        self.agents.get(amid).map(|a| a.status)
    }

    /// Get agent info
    pub fn get_agent(&self, amid: &Amid) -> Option<AgentConnection> {
        self.agents.get(amid).map(|a| a.clone())
    }

    /// Check if an agent is connected
    pub fn is_connected(&self, amid: &Amid) -> bool {
        self.connections.contains_key(amid)
    }

    /// Send a message to an agent
    pub fn send_to(&self, amid: &Amid, message: RelayMessage) -> bool {
        if let Some(sender) = self.connections.get(amid) {
            sender.send(message).is_ok()
        } else {
            false
        }
    }

    /// Check and update rate limits
    /// Returns Ok if within limits, Err with retry time if exceeded
    pub fn check_rate_limit(&self, amid: &Amid, is_knock: bool) -> Result<(), u32> {
        let now = Utc::now();

        let mut state = self.rate_limits.entry(amid.clone()).or_default();

        // Reset if we're in a new minute
        if now.signed_duration_since(state.minute_start).num_seconds() >= 60 {
            state.messages_this_minute = 0;
            state.knocks_this_minute = 0;
            state.minute_start = now;
        }

        // Check limits
        if is_knock {
            if state.knocks_this_minute >= self.config.rate_limit_knocks_per_minute {
                let seconds_left = 60 - now.signed_duration_since(state.minute_start).num_seconds();
                return Err(seconds_left as u32);
            }
            state.knocks_this_minute += 1;
        } else {
            if state.messages_this_minute >= self.config.rate_limit_messages_per_minute {
                let seconds_left = 60 - now.signed_duration_since(state.minute_start).num_seconds();
                return Err(seconds_left as u32);
            }
            state.messages_this_minute += 1;
        }

        Ok(())
    }

    /// Get count of connected agents
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }
}

/// Handle a single WebSocket connection
pub async fn handle_connection(
    stream: TcpStream,
    peer_addr: SocketAddr,
    manager: Arc<ConnectionManager>,
    store_forward: Arc<StoreForward>,
) -> anyhow::Result<()> {
    // Upgrade to WebSocket
    let ws_stream = accept_async(stream).await?;
    info!("WebSocket connection from {}", peer_addr);

    let (write, read) = ws_stream.split();

    // Channel for sending messages to this connection
    let (tx, rx) = mpsc::unbounded_channel::<RelayMessage>();

    // Handle authentication first
    let (amid, session_id) = match handle_auth(read, &tx, &manager).await {
        Ok((ws_read, amid, session_id, p2p_capable)) => {
            // Register connection
            let session_id = manager.register(amid.clone(), tx.clone(), p2p_capable);

            // Send connected response with pending message count
            let pending_count = store_forward.get_pending_count(&amid);
            let _ = tx.send(RelayMessage::Connected {
                session_id,
                pending_messages: pending_count as u32,
            });

            // Deliver any stored messages
            let stored = store_forward.retrieve(&amid);
            for msg in stored {
                let _ = tx.send(RelayMessage::Receive {
                    from: msg.from,
                    encrypted_payload: msg.encrypted_payload,
                    message_type: msg.message_type,
                    timestamp: msg.timestamp,
                    ice_candidates: None,
                });
            }

            info!("Agent {} connected (session: {})", amid, session_id);

            // Shared state for ping/pong tracking
            let pong_received = Arc::new(AtomicBool::new(true));
            let pong_received_clone = pong_received.clone();

            // Channel for WebSocket messages (both RelayMessages and raw pings)
            let (ws_tx, ws_rx) = mpsc::unbounded_channel::<Message>();
            let ws_tx_clone = ws_tx.clone();

            // Forward RelayMessages to WebSocket message channel
            let relay_rx_task = {
                let ws_tx = ws_tx.clone();
                tokio::spawn(async move {
                    let mut rx = rx;
                    while let Some(msg) = rx.recv().await {
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if ws_tx.send(Message::Text(json)).is_err() {
                                break;
                            }
                        }
                    }
                })
            };

            // Spawn ping task for keepalive
            let ping_task = {
                let amid = amid.clone();
                let manager = manager.clone();
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));
                    loop {
                        interval.tick().await;

                        // Check if we received pong from last ping
                        if !pong_received_clone.load(Ordering::Relaxed) {
                            warn!("Agent {} did not respond to ping, disconnecting", amid);
                            manager.unregister(&amid);
                            break;
                        }

                        // Send new ping
                        pong_received_clone.store(false, Ordering::Relaxed);
                        if ws_tx_clone.send(Message::Ping(vec![])).is_err() {
                            break;
                        }
                        debug!("Sent ping to agent {}", amid);
                    }
                })
            };

            // Spawn handlers
            tokio::spawn(handle_outgoing(write, ws_rx));
            tokio::spawn(handle_incoming(ws_read, amid.clone(), manager.clone(), store_forward.clone(), pong_received));

            (amid, session_id)
        }
        Err(e) => {
            warn!("Authentication failed from {}: {}", peer_addr, e);
            return Err(anyhow::anyhow!(e));
        }
    };

    Ok(())
}

/// Handle the authentication handshake
async fn handle_auth(
    mut read: SplitStream<WebSocketStream<TcpStream>>,
    tx: &mpsc::UnboundedSender<RelayMessage>,
    manager: &ConnectionManager,
) -> Result<(SplitStream<WebSocketStream<TcpStream>>, Amid, Uuid, bool), &'static str> {
    // Wait for Connect message
    let msg = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        read.next()
    ).await
        .map_err(|_| "Timeout waiting for auth")?
        .ok_or("Connection closed before auth")?
        .map_err(|_| "WebSocket error")?;

    let text = match msg {
        Message::Text(t) => t,
        _ => return Err("Expected text message"),
    };

    let connect_msg: RelayMessage = serde_json::from_str(&text)
        .map_err(|_| "Invalid JSON")?;

    match connect_msg {
        RelayMessage::Connect { protocol, amid, public_key, signature, timestamp, p2p_capable } => {
            // Verify protocol version (accept 0.1 for backwards compat, 0.2 is preferred)
            if protocol != "agentmesh/0.1" && protocol != "agentmesh/0.2" {
                let _ = tx.send(RelayMessage::Error {
                    code: ErrorCode::ProtocolMismatch,
                    message: format!("Expected agentmesh/0.1 or 0.2, got {}", protocol),
                    retry_after_seconds: None,
                });
                return Err("Protocol mismatch");
            }

            // Verify signature proves ownership of AMID
            if let Err(auth_err) = auth::verify_connection_signature(
                &amid,
                &public_key,
                &signature,
                &timestamp,
            ) {
                warn!("Signature verification failed for {}: {:?}", amid, auth_err);
                let _ = tx.send(RelayMessage::Error {
                    code: ErrorCode::InvalidSignature,
                    message: format!("Signature verification failed: {}", auth_err),
                    retry_after_seconds: None,
                });
                return Err("Signature verification failed");
            }

            debug!("Signature verified for agent {}", amid);
            let session_id = Uuid::new_v4();
            Ok((read, amid, session_id, p2p_capable))
        }
        _ => {
            let _ = tx.send(RelayMessage::Error {
                code: ErrorCode::InvalidMessage,
                message: "Expected Connect message".to_string(),
                retry_after_seconds: None,
            });
            Err("Expected Connect message")
        }
    }
}

/// Handle outgoing messages to this connection
async fn handle_outgoing(
    mut write: SplitSink<WebSocketStream<TcpStream>, Message>,
    mut rx: mpsc::UnboundedReceiver<Message>,
) {
    while let Some(msg) = rx.recv().await {
        if let Err(e) = write.send(msg).await {
            warn!("Failed to send message: {}", e);
            break;
        }
    }
}

/// Handle incoming messages from this connection
async fn handle_incoming(
    mut read: SplitStream<WebSocketStream<TcpStream>>,
    amid: Amid,
    manager: Arc<ConnectionManager>,
    store_forward: Arc<StoreForward>,
    pong_received: Arc<AtomicBool>,
) {
    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!("WebSocket error from {}: {}", amid, e);
                break;
            }
        };

        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => {
                info!("Agent {} disconnected", amid);
                break;
            }
            Message::Ping(_) => {
                // Pong is handled automatically by tungstenite
                continue;
            }
            Message::Pong(_) => {
                // Received pong response to our ping - mark as alive
                pong_received.store(true, Ordering::Relaxed);
                debug!("Received pong from agent {}", amid);
                continue;
            }
            _ => continue,
        };

        let relay_msg: RelayMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                debug!("Invalid message from {}: {}", amid, e);
                continue;
            }
        };

        handle_relay_message(&amid, relay_msg, &manager, &store_forward).await;
    }

    // Clean up on disconnect
    manager.unregister(&amid);
    info!("Agent {} unregistered", amid);
}

/// Process a relay message
async fn handle_relay_message(
    from: &Amid,
    msg: RelayMessage,
    manager: &ConnectionManager,
    store_forward: &StoreForward,
) {
    match msg {
        RelayMessage::Send { to, encrypted_payload, message_type, ice_candidates } => {
            // Rate limit check
            let is_knock = message_type == MessageType::Knock;
            if let Err(retry_after) = manager.check_rate_limit(from, is_knock) {
                manager.send_to(from, RelayMessage::Error {
                    code: ErrorCode::RateLimited,
                    message: "Rate limit exceeded".to_string(),
                    retry_after_seconds: Some(retry_after),
                });
                return;
            }

            // Try to deliver directly
            let receive_msg = RelayMessage::Receive {
                from: from.clone(),
                encrypted_payload: encrypted_payload.clone(),
                message_type,
                timestamp: Utc::now(),
                ice_candidates,
            };

            if manager.send_to(&to, receive_msg) {
                debug!("Delivered message from {} to {}", from, to);
            } else {
                // Store for later delivery
                let stored = StoredMessage {
                    id: Uuid::new_v4(),
                    from: from.clone(),
                    to: to.clone(),
                    encrypted_payload,
                    message_type,
                    timestamp: Utc::now(),
                    expires_at: Utc::now() + chrono::Duration::hours(72),
                };

                if store_forward.store(stored) {
                    debug!("Stored message from {} for offline agent {}", from, to);
                } else {
                    manager.send_to(from, RelayMessage::Error {
                        code: ErrorCode::RecipientOffline,
                        message: format!("Agent {} is offline and message queue is full", to),
                        retry_after_seconds: None,
                    });
                }
            }
        }

        RelayMessage::Presence { status } => {
            manager.update_status(from, status);
            debug!("Agent {} status updated to {:?}", from, status);
        }

        RelayMessage::PresenceQuery { amid } => {
            let (status, last_seen) = if let Some(agent) = manager.get_agent(&amid) {
                (agent.status, Some(agent.last_activity))
            } else {
                (PresenceStatus::Offline, None)
            };

            manager.send_to(from, RelayMessage::PresenceResponse {
                amid,
                status,
                last_seen,
            });
        }

        RelayMessage::IceOffer { to, sdp, candidates } => {
            // Forward ICE offer for P2P negotiation
            if manager.send_to(&to, RelayMessage::IceOffer {
                to: from.clone(),  // Swap 'to' to be sender's AMID for receiver
                sdp,
                candidates,
            }) {
                debug!("Forwarded ICE offer from {} to {}", from, to);
            } else {
                manager.send_to(from, RelayMessage::Error {
                    code: ErrorCode::RecipientOffline,
                    message: format!("Cannot establish P2P: agent {} is offline", to),
                    retry_after_seconds: None,
                });
            }
        }

        RelayMessage::IceAnswer { to, sdp, candidates } => {
            // Forward ICE answer
            if manager.send_to(&to, RelayMessage::IceAnswer {
                to: from.clone(),
                sdp,
                candidates,
            }) {
                debug!("Forwarded ICE answer from {} to {}", from, to);
            }
        }

        RelayMessage::P2PEstablished { peer } => {
            info!("P2P connection established between {} and {}", from, peer);
            // Notify the peer
            manager.send_to(&peer, RelayMessage::P2PEstablished {
                peer: from.clone(),
            });
        }

        RelayMessage::Ping { timestamp } => {
            manager.send_to(from, RelayMessage::Pong { timestamp });
        }

        RelayMessage::Disconnect { reason } => {
            info!("Agent {} disconnecting: {}", from, reason);
            manager.unregister(from);
        }

        _ => {
            debug!("Ignoring message type from {}", from);
        }
    }
}
