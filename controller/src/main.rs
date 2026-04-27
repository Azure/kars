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

mod a2a_agent;
mod a2a_agent_compile;
mod a2a_agent_reconciler;
mod claw_memory;
mod claw_memory_compile;
mod claw_memory_reconciler;
mod crd;
#[allow(dead_code)]
// CRD-installation pipeline (Phase 1 close-out + future kubectl-claw-attest) consumes these helpers.
mod crd_validations;
mod fedcred;
mod fedcred_reaper;
mod helm_drift;
mod inference_policy;
mod inference_policy_compile;
mod inference_policy_reconciler;
mod mcp_server;
mod mcp_server_reconciler;
mod mesh_peer;
mod pairing;
mod pairing_reconciler;
mod providers;
mod reconciler;
mod status;
#[allow(dead_code)] // helpers consumed by tool_policy_reconciler + future slices.
mod tool_policy;
mod tool_policy_compile;
mod tool_policy_reconciler;

use anyhow::Result;
use kube::Client;
use std::sync::Arc;
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
    let mcp_server_handle = {
        let client = client.clone();
        tokio::spawn(async move { mcp_server_reconciler::run(client).await })
    };
    let tool_policy_handle = {
        let client = client.clone();
        tokio::spawn(async move { tool_policy_reconciler::run(client).await })
    };
    let a2a_agent_handle = {
        let client = client.clone();
        tokio::spawn(async move { a2a_agent_reconciler::run(client).await })
    };
    let inference_policy_handle = {
        let client = client.clone();
        tokio::spawn(async move { inference_policy_reconciler::run(client).await })
    };
    let claw_memory_handle = {
        let client = client.clone();
        tokio::spawn(async move { claw_memory_reconciler::run(client).await })
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

    // Periodic federated-credential garbage collector. Activates only when
    // FedCredConfig env vars are present (same condition as auto-create).
    // Idempotent — safe to run on every replica.
    let fedcred_reaper_handle = {
        let client = client.clone();
        tokio::spawn(async move {
            match fedcred::FedCredConfig::from_env() {
                Some(cfg) => {
                    let mgr = Arc::new(fedcred::FedCredManager::new(cfg));
                    fedcred_reaper::run(client, mgr).await
                }
                None => {
                    tracing::info!(
                        "Fedcred reaper disabled (FedCred env vars missing — auto-create also off)"
                    );
                    std::future::pending::<Result<()>>().await
                }
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
        res = mcp_server_handle => {
            res??;
        }
        res = tool_policy_handle => {
            res??;
        }
        res = a2a_agent_handle => {
            res??;
        }
        res = inference_policy_handle => {
            res??;
        }
        res = claw_memory_handle => {
            res??;
        }
        res = mesh_peer_handle => {
            res??;
        }
        res = fedcred_reaper_handle => {
            res??;
        }
    }

    Ok(())
}
