//! AzureClaw Controller — Kubernetes operator for sandboxed OpenClaw agents.
//!
//! Watches `ClawSandbox` custom resources and reconciles:
//! - Isolated namespace per sandbox
//! - OpenClaw agent pod with security constraints (seccomp, SELinux, read-only rootfs)
//! - NetworkPolicy (default-deny + allowlist from CRD spec)
//! - Envoy sidecar for L7 egress filtering
//! - Workload Identity bindings for Azure service access
//! - Inference router configuration
//!
//! Built with kube-rs (CNCF Sandbox).

mod crd;
mod reconciler;

use anyhow::Result;
use kube::Client;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "azureclaw_controller=info".into()))
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    tracing::info!("AzureClaw Controller starting");

    let client = Client::try_default().await?;
    reconciler::run(client).await?;

    Ok(())
}
