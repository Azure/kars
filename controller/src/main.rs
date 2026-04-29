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
mod backoff;
mod claw_eval;
mod claw_eval_compile;
mod claw_eval_reconciler;
mod claw_memory;
mod claw_memory_compile;
mod claw_memory_reconciler;
mod crd;
#[allow(dead_code)]
// CRD-installation pipeline (Phase 1 close-out + future kubectl-claw-attest) consumes these helpers.
mod crd_validations;
mod fedcred;
mod fedcred_reaper;
mod field_managers;
mod helm_drift;
mod inference_policy;
mod inference_policy_compile;
mod inference_policy_reconciler;
mod leader_election;
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
use tokio::sync::oneshot;
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

    // S7.C: controller-wide leader election. With `replicas: 2` shipped
    // in the Helm chart, both pods would otherwise reconcile in
    // parallel and double-emit Foundry agent creates / status patches /
    // events. Default-on; opt out with `LEADER_ELECTION_ENABLED=false`
    // for dev/kind clusters where holding a Lease is unnecessary.
    let leader_election_enabled = !matches!(
        std::env::var("LEADER_ELECTION_ENABLED")
            .unwrap_or_else(|_| "true".into())
            .to_ascii_lowercase()
            .as_str(),
        "false" | "0" | "no" | "off"
    );

    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    let leader_handle = if leader_election_enabled {
        let cfg = leader_election::LeaderElectionConfig::from_env();
        tracing::info!(
            lease = %cfg.lease_name,
            namespace = %cfg.namespace,
            identity = %cfg.identity,
            "leader-election: enabled; waiting to acquire controller lease before spawning reconcilers"
        );
        let client = client.clone();
        Some(tokio::spawn(async move {
            leader_election::acquire_and_hold(client, cfg, ready_tx).await
        }))
    } else {
        tracing::warn!(
            "leader-election: disabled via LEADER_ELECTION_ENABLED=false. \
             Running both replicas in parallel will double-write status \
             and emit duplicate events. Only safe for single-replica deployments."
        );
        // Fire ready immediately so the reconciler bundle starts.
        let _ = ready_tx.send(());
        None
    };

    // Block reconciler spawn until either we acquire the lease or the
    // leader-election task fails. If acquire_and_hold returns before
    // signalling readiness, its `ready_tx` is dropped and our await
    // here observes RecvError — we propagate the leader task's error.
    match ready_rx.await {
        Ok(()) => {
            if leader_handle.is_some() {
                tracing::info!("leader-election: ready; proceeding to spawn reconcilers");
            }
        }
        Err(_) => {
            // Sender was dropped — leader task exited (or, if leader
            // election was disabled, the ready_tx was sent + dropped
            // and we got Ok above; this branch only applies to the
            // leader-task-crashed-before-signalling case).
            if let Some(h) = leader_handle {
                return match h.await {
                    Ok(inner) => inner,
                    Err(e) => Err(e.into()),
                };
            }
            return Err(anyhow::anyhow!(
                "leader-election: ready channel closed without signal and no leader handle"
            ));
        }
    }

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
    let claw_eval_handle = {
        let client = client.clone();
        tokio::spawn(async move { claw_eval_reconciler::run(client).await })
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

    // Convert Option<JoinHandle> to a future that pends forever when
    // leader election is disabled. This keeps the `tokio::select!`
    // arms uniform regardless of mode.
    let leader_future = async move {
        match leader_handle {
            Some(h) => match h.await {
                Ok(inner) => inner,
                Err(e) => Err(e.into()),
            },
            None => std::future::pending::<Result<()>>().await,
        }
    };
    tokio::pin!(leader_future);

    tokio::select! {
        res = &mut leader_future => {
            // Lost leadership (renewal failed) -> propagate so the pod
            // restarts and re-enters the election. Standard fail-stop
            // pattern for K8s controllers.
            res?;
        }
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
        res = claw_eval_handle => {
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
