// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Kars Controller — Kubernetes operator for sandboxed OpenClaw agents.
//!
//! Watches `KarsSandbox` and `KarsPairing` custom resources and reconciles:
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
mod config_hash;
mod crd;
#[allow(dead_code)]
// CRD-installation pipeline (Phase 1 close-out + future kubectl-claw-attest) consumes these helpers.
mod crd_validations;
mod egress_allowlist_compile;
mod egress_approval;
mod egress_approval_compile;
mod egress_approval_reconciler;
mod fedcred;
mod fedcred_reaper;
mod field_managers;
mod helm_drift;
mod inference_policy;
mod inference_policy_compile;
mod inference_policy_reconciler;
mod kars_eval;
mod kars_eval_reconciler;
mod kars_memory;
mod kars_memory_compile;
mod kars_memory_reconciler;
mod leader_election;
mod mcp_server;
mod mcp_server_reconciler;
mod mesh_peer;
mod metrics;
mod metrics_server;
mod pairing;
mod pairing_reconciler;
mod policy_canonical;
mod policy_fetcher;
mod providers;
mod reconciler;
mod signer_policy;
mod status;
#[allow(dead_code)] // helpers consumed by tool_policy_reconciler + future slices.
mod tool_policy;
mod tool_policy_compile;
mod tool_policy_reconciler;
mod trust_graph;
mod trust_graph_compile;
mod trust_graph_reconciler;

use anyhow::Result;
use kube::Client;
use std::sync::Arc;
use tokio::sync::oneshot;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Multiple transitive deps enable both `ring` and `aws-lc-rs`
    // rustls providers. rustls 0.23.40+ refuses to auto-detect when
    // both are available, so pick one explicitly before any TLS
    // work. `aws_lc_rs` matches the workspace default.
    if rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .is_err()
    {
        tracing::debug!("rustls CryptoProvider already installed");
    }

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kars_controller=info".into()),
        )
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    tracing::info!("Kars Controller starting");

    // P2 #12: stamp the controller config hash early so it appears
    // in the very first log line operators look at and is exposed
    // on /metrics before any reconciler fires.
    let config_hash = config_hash::compute_from_env();
    config_hash::record_config_hash(&config_hash);
    tracing::info!(
        controller_config_hash = %config_hash,
        "controller config hash computed; surfaced as kars_controller_config_info"
    );

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
    let kars_memory_handle = {
        let client = client.clone();
        tokio::spawn(async move { kars_memory_reconciler::run(client).await })
    };
    let kars_eval_handle = {
        let client = client.clone();
        tokio::spawn(async move { kars_eval_reconciler::run(client).await })
    };
    let trust_graph_handle = {
        let client = client.clone();
        tokio::spawn(async move { trust_graph_reconciler::run(client).await })
    };
    let egress_approval_handle = {
        let client = client.clone();
        tokio::spawn(async move { egress_approval_reconciler::run(client).await })
    };

    // S12.d: SignerPolicy ConfigMap watcher. Installs a process-global
    // handle so `policy_fetcher::maybe_verify_allowlist` resolves
    // identity-pinning policy from the live cluster ConfigMap. Falls
    // back to env vars (`KARS_SIGNER_*`) when the ConfigMap is
    // absent — that's the emergency-override path. Malformed
    // ConfigMaps surface as `SignerPolicyMalformed` and **do not**
    // silently fall back to env (operator must fix the cluster
    // config). Watcher is namespace-scoped to the controller's own
    // namespace + filtered to the singleton object name, so RBAC stays
    // narrow.
    let signer_policy_handle = signer_policy::SharedSignerPolicy::new();
    signer_policy::install_global(signer_policy_handle.clone());
    let signer_policy_watcher_handle = {
        let client = client.clone();
        let shared = signer_policy_handle.clone();
        let ns = std::env::var("POD_NAMESPACE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "kars-system".to_string());
        tokio::spawn(async move { signer_policy::run(client, ns, shared).await })
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
                // The peer reads KARS_MESH_PROVIDER internally and uses
                // the AGT wire envelope for federation features.
                tracing::info!("Mesh peer enabled — starting relay connection");
                mesh_peer::run(client).await
            } else {
                tracing::warn!(
                    "Mesh peer disabled (MESH_PEER_ENABLED={}). External agent pairing will NOT work. \
                     Re-run `kars up` (without --no-mesh-peer) to enable federation.",
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

    // S7.E: optional Prometheus metrics server. Default ON; opt out
    // via `CONTROLLER_METRICS_ADDR=disabled` (or empty). Failures
    // here are non-fatal — the controller's reconcile loops are the
    // primary product; metrics are observability sugar on top.
    let metrics_handle = {
        match metrics_server::bind_addr_from_env() {
            Some(addr) => Some(tokio::spawn(async move {
                if let Err(e) = metrics_server::run(addr).await {
                    tracing::error!(error = %e, "controller metrics server exited");
                }
            })),
            None => {
                tracing::info!(
                    "controller metrics server disabled (CONTROLLER_METRICS_ADDR=disabled)"
                );
                None
            }
        }
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

    // Metrics server is fire-and-forget: failures shouldn't kill the
    // controller's reconcile loops. Drop the handle to detach.
    let _ = metrics_handle;

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
        res = kars_memory_handle => {
            res??;
        }
        res = kars_eval_handle => {
            res??;
        }
        res = trust_graph_handle => {
            res??;
        }
        res = egress_approval_handle => {
            res??;
        }
        res = mesh_peer_handle => {
            res??;
        }
        res = fedcred_reaper_handle => {
            res??;
        }
        res = signer_policy_watcher_handle => {
            // SignerPolicy watcher exiting is non-fatal but we
            // propagate so the controller restarts and re-establishes
            // the watch — avoids serving stale policy state forever
            // after a transient kube-apiserver blip.
            res??;
        }
    }

    Ok(())
}
