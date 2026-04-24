//! AzureClaw Controller — Kubernetes operator for sandboxed OpenClaw agents.
//!
//! Watches `ClawSandbox` and `ClawPairing` custom resources and reconciles:
//! - Isolated namespace per sandbox
//! - OpenClaw agent pod with security constraints (seccomp, SELinux, read-only rootfs)
//! - NetworkPolicy (default-deny + allowlist from CRD spec)
//! - iptables egress-guard for per-container network isolation
//! - Workload Identity bindings for Azure service access
//! - Inference router configuration
//! - Federation pairings for external agent cloud offload/handoff
//!
//! Built with kube-rs (CNCF Sandbox).

mod crd;
mod fedcred;
mod mesh_peer;
mod pairing;
mod pairing_reconciler;
mod providers;
mod reconciler;
mod status;

use anyhow::Result;
use kube::Client;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "azureclaw_controller=info".into()),
        )
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    tracing::info!("AzureClaw Controller starting");

    let client = Client::try_default().await?;

    // Run sandbox and pairing controllers concurrently.
    // Pairing controller is non-fatal — if CRD is missing, it exits gracefully.
    // Mesh peer defaults to ON (federation is required for external agent
    // pairing to work). Set MESH_PEER_ENABLED=false to opt out.
    let sandbox_handle = {
        let client = client.clone();
        tokio::spawn(async move { reconciler::run(client).await })
    };
    let pairing_handle = {
        let client = client.clone();
        tokio::spawn(async move { pairing_reconciler::run(client).await })
    };
    let mesh_peer_handle = {
        let client = client.clone();
        tokio::spawn(async move {
            // Default: enabled. Explicit "false" or "0" disables.
            let raw = std::env::var("MESH_PEER_ENABLED").unwrap_or_else(|_| "true".into());
            let enabled = !matches!(
                raw.to_ascii_lowercase().as_str(),
                "false" | "0" | "no" | "off"
            );
            if enabled {
                tracing::info!("Mesh peer enabled — starting relay connection");
                mesh_peer::run(client).await
            } else {
                tracing::warn!(
                    "Mesh peer disabled (MESH_PEER_ENABLED={}). External agent pairing will NOT work. \
                     Re-run `azureclaw up` (without --no-mesh-peer) to enable federation.",
                    raw
                );
                // Park forever — don't exit so select! doesn't trigger
                std::future::pending::<Result<()>>().await
            }
        })
    };

    tokio::select! {
        res = sandbox_handle => {
            res??;
        }
        res = pairing_handle => {
            res??;
        }
        res = mesh_peer_handle => {
            res??;
        }
    }

    Ok(())
}
