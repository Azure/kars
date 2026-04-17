use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tracing::{info, error, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod auth;
mod connection;
mod message;
mod store_forward;
mod types;
mod ice;
mod registry_verify;

use connection::ConnectionManager;
use store_forward::StoreForward;
use registry_verify::RegistryVerifier;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "agentmesh_relay=info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Railway provides PORT env var
    let port = std::env::var("PORT")
        .unwrap_or_else(|_| "8765".to_string());
    let addr = format!("0.0.0.0:{}", port);

    let addr: SocketAddr = addr.parse()?;

    // Health endpoint port (separate from WebSocket to avoid "Handshake not finished" from K8s probes)
    let health_port: u16 = std::env::var("HEALTH_PORT")
        .unwrap_or_else(|_| "8766".to_string())
        .parse()
        .unwrap_or(8766);

    // Create shared state
    let connection_manager = Arc::new(ConnectionManager::new());
    let store_forward = Arc::new(StoreForward::new());
    let registry_verifier = Arc::new(RegistryVerifier::from_env());

    // Create shutdown channel
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // Start cleanup task for expired stored messages
    let sf_clone = store_forward.clone();
    tokio::spawn(async move {
        sf_clone.cleanup_expired_loop().await;
    });

    // Start HTTP health endpoint (for K8s readiness/liveness probes)
    let cm_health = connection_manager.clone();
    let sf_health = store_forward.clone();
    tokio::spawn(async move {
        serve_health(health_port, cm_health, sf_health).await;
    });

    // Bind TCP listener
    let listener = TcpListener::bind(&addr).await?;
    info!("AgentMesh Relay Server listening on {}", addr);
    info!("Health endpoint on :{}", health_port);
    info!("Protocol version: agentmesh/0.2");

    // Accept connections
    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                let cm = connection_manager.clone();
                let sf = store_forward.clone();
                let rv = registry_verifier.clone();
                let mut shutdown_rx = shutdown_tx.subscribe();

                tokio::spawn(async move {
                    tokio::select! {
                        result = connection::handle_connection(stream, peer_addr, cm, sf, rv) => {
                            if let Err(e) = result {
                                warn!("Connection error from {}: {}", peer_addr, e);
                            }
                        }
                        _ = shutdown_rx.recv() => {
                            info!("Shutting down connection from {}", peer_addr);
                        }
                    }
                });
            }
            Err(e) => {
                error!("Failed to accept connection: {}", e);
            }
        }
    }
}

/// Serve a minimal HTTP health endpoint on a separate port.
/// This prevents K8s TCP readiness probes from triggering "Handshake not finished"
/// errors on the WebSocket port.
async fn serve_health(
    port: u16,
    manager: Arc<ConnectionManager>,
    store_forward: Arc<StoreForward>,
) {
    use tokio::io::AsyncWriteExt;

    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind health endpoint on :{}: {}", port, e);
            return;
        }
    };

    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => continue,
        };

        let connected = manager.connection_count();
        let sf_stats = store_forward.stats();

        let body = format!(
            r#"{{"status":"healthy","protocol":"agentmesh/0.2","connected_agents":{},"stored_messages":{},"agents_with_pending":{}}}"#,
            connected,
            sf_stats.total_messages,
            sf_stats.agents_with_pending,
        );

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body,
        );

        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    }
}
