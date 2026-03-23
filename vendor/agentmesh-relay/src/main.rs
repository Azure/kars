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

use connection::ConnectionManager;
use store_forward::StoreForward;

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

    // Create shared state
    let connection_manager = Arc::new(ConnectionManager::new());
    let store_forward = Arc::new(StoreForward::new());

    // Create shutdown channel
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // Start cleanup task for expired stored messages
    let sf_clone = store_forward.clone();
    tokio::spawn(async move {
        sf_clone.cleanup_expired_loop().await;
    });

    // Bind TCP listener
    let listener = TcpListener::bind(&addr).await?;
    info!("AgentMesh Relay Server listening on {}", addr);
    info!("Protocol version: agentmesh/0.1");

    // Accept connections
    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                let cm = connection_manager.clone();
                let sf = store_forward.clone();
                let mut shutdown_rx = shutdown_tx.subscribe();

                tokio::spawn(async move {
                    tokio::select! {
                        result = connection::handle_connection(stream, peer_addr, cm, sf) => {
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
