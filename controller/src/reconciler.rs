//! Controller reconciliation loop — watches ClawSandbox CRDs and reconciles state.
//!
//! When a ClawSandbox is created or updated, the reconciler:
//! 1. Creates an isolated namespace
//! 2. Deploys a ServiceAccount with Workload Identity
//! 3. Applies default-deny NetworkPolicy + allowlist
//! 4. Deploys the OpenClaw sandbox pod with security constraints
//! 5. Updates CRD status

use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, ServiceAccount},
    networking::v1::NetworkPolicy,
};
use kube::{
    api::{Api, ListParams, Patch, PatchParams},
    runtime::controller::{Action, Controller},
    Client, ResourceExt,
};
use serde_json::json;
use std::sync::Arc;
use tokio::time::Duration;

use crate::crd::ClawSandbox;

/// Custom error type that bridges serde_json and kube errors.
#[derive(Debug, thiserror::Error)]
enum ReconcileError {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("JSON serialization error: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

/// Shared controller context.
struct Context {
    client: Client,
}

/// Main reconciliation function — called whenever a ClawSandbox changes.
async fn reconcile(sandbox: Arc<ClawSandbox>, ctx: Arc<Context>) -> Result<Action, ReconcileError> {
    let name = sandbox.name_any();
    let sandbox_ns = format!("azureclaw-{name}");
    let client = &ctx.client;

    tracing::info!("Reconciling ClawSandbox {name}");

    let spec = sandbox.spec.clone();
    let sandbox_config = spec.sandbox.unwrap_or_default();
    let inference_config = spec.inference.unwrap_or_default();
    let openclaw_config = spec.openclaw.unwrap_or_default();

    // ── Step 1: Create namespace ─────────────────────────────────────────
    let ns_api: Api<Namespace> = Api::all(client.clone());
    let ns: Namespace = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Namespace",
        "metadata": {
            "name": sandbox_ns,
            "labels": {
                "app.kubernetes.io/name": "azureclaw",
                "app.kubernetes.io/component": "sandbox",
                "azureclaw.azure.com/sandbox": name,
                "pod-security.kubernetes.io/enforce": "restricted",
                "pod-security.kubernetes.io/audit": "restricted",
                "pod-security.kubernetes.io/warn": "restricted"
            }
        }
    }))?;
    ns_api
        .patch(
            &sandbox_ns,
            &PatchParams::apply("azureclaw-controller"),
            &Patch::Apply(ns),
        )
        .await?;

    // ── Step 2: Create ServiceAccount with Workload Identity ─────────────
    let sa_api: Api<ServiceAccount> = Api::namespaced(client.clone(), &sandbox_ns);
    let sa: ServiceAccount = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "ServiceAccount",
        "metadata": {
            "name": "sandbox",
            "namespace": sandbox_ns,
            "labels": {
                "azureclaw.azure.com/sandbox": name
            },
            "annotations": {
                "azure.workload.identity/client-id": "" // Filled by azureclaw init
            }
        }
    }))?;
    sa_api
        .patch(
            "sandbox",
            &PatchParams::apply("azureclaw-controller"),
            &Patch::Apply(sa),
        )
        .await?;

    // ── Step 3: Apply default-deny NetworkPolicy + allowlist ─────────────
    let np_api: Api<NetworkPolicy> = Api::namespaced(client.clone(), &sandbox_ns);
    let mut egress_rules = vec![
        // Always allow DNS
        json!({
            "to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}}}],
            "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]
        }),
        // Always allow inference router
        json!({
            "to": [{"namespaceSelector": {"matchLabels": {"app.kubernetes.io/name": "azureclaw"}},
                     "podSelector": {"matchLabels": {"azureclaw.azure.com/component": "inference-router"}}}],
            "ports": [{"protocol": "TCP", "port": 8443}]
        }),
    ];

    // Add user-defined allowed endpoints
    if let Some(ref policy) = spec.network_policy {
        if let Some(ref endpoints) = policy.allowed_endpoints {
            for ep in endpoints {
                let port = ep.port.unwrap_or(443);
                egress_rules.push(json!({
                    "to": [{"ipBlock": {"cidr": "0.0.0.0/0"}}],
                    "ports": [{"protocol": "TCP", "port": port}]
                }));
            }
        }
    }

    let netpol: NetworkPolicy = serde_json::from_value(json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
            "name": "sandbox-policy",
            "namespace": sandbox_ns,
            "labels": {"azureclaw.azure.com/sandbox": name}
        },
        "spec": {
            "podSelector": {"matchLabels": {"azureclaw.azure.com/component": "sandbox"}},
            "policyTypes": ["Egress", "Ingress"],
            "egress": egress_rules,
            "ingress": []
        }
    }))?;
    np_api
        .patch(
            "sandbox-policy",
            &PatchParams::apply("azureclaw-controller"),
            &Patch::Apply(netpol),
        )
        .await?;

    // ── Step 4: Deploy sandbox pod ───────────────────────────────────────
    let image = openclaw_config
        .image
        .unwrap_or_else(|| "azureclaw.azurecr.io/openclaw-sandbox:latest".into());

    let deploy_api: Api<Deployment> = Api::namespaced(client.clone(), &sandbox_ns);
    let deployment: Deployment = serde_json::from_value(json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": name,
            "namespace": sandbox_ns,
            "labels": {
                "azureclaw.azure.com/sandbox": name,
                "azureclaw.azure.com/component": "sandbox"
            }
        },
        "spec": {
            "replicas": 1,
            "selector": {
                "matchLabels": {"azureclaw.azure.com/sandbox": name}
            },
            "template": {
                "metadata": {
                    "labels": {
                        "azureclaw.azure.com/sandbox": name,
                        "azureclaw.azure.com/component": "sandbox",
                        "azure.workload.identity/use": "true"
                    }
                },
                "spec": {
                    "serviceAccountName": "sandbox",
                    "securityContext": {
                        "runAsNonRoot": sandbox_config.run_as_non_root,
                        "runAsUser": 1000,
                        "runAsGroup": 1000,
                        "fsGroup": 1000,
                        "seccompProfile": {
                            "type": "Localhost",
                            "localhostProfile": format!("profiles/{}.json", sandbox_config.seccomp_profile)
                        },
                        "seLinuxOptions": {
                            "type": sandbox_config.selinux_context
                        }
                    },
                    "containers": [{
                        "name": "openclaw",
                        "image": image,
                        "ports": [{"containerPort": 18789}],
                        "env": [
                            {"name": "OPENCLAW_MODEL", "value": inference_config.model},
                            {"name": "INFERENCE_ROUTER_URL", "value": "https://azureclaw-inference-router.azureclaw-system.svc.cluster.local:8443"}
                        ],
                        "securityContext": {
                            "allowPrivilegeEscalation": sandbox_config.allow_privilege_escalation,
                            "readOnlyRootFilesystem": sandbox_config.read_only_root_filesystem,
                            "capabilities": {"drop": ["ALL"]}
                        },
                        "volumeMounts": [
                            {"name": "sandbox-data", "mountPath": "/sandbox"},
                            {"name": "tmp", "mountPath": "/tmp"}
                        ],
                        "resources": spec.resources.as_ref().map(|r| json!({
                            "requests": r.requests,
                            "limits": r.limits
                        })).unwrap_or(json!({
                            "requests": {"cpu": "500m", "memory": "1Gi"},
                            "limits": {"cpu": "2", "memory": "4Gi"}
                        }))
                    }],
                    "volumes": [
                        {"name": "sandbox-data", "emptyDir": {}},
                        {"name": "tmp", "emptyDir": {"medium": "Memory", "sizeLimit": "1Gi"}}
                    ],
                    "tolerations": [{
                        "key": "azureclaw.azure.com/sandbox",
                        "operator": "Equal",
                        "value": "true",
                        "effect": "NoSchedule"
                    }],
                    "nodeSelector": {
                        "azureclaw.azure.com/pool": "sandbox"
                    }
                }
            }
        }
    }))?;
    deploy_api
        .patch(
            &name,
            &PatchParams::apply("azureclaw-controller"),
            &Patch::Apply(deployment),
        )
        .await?;

    // ── Step 5: Update status ────────────────────────────────────────────
    let sandbox_api: Api<ClawSandbox> =
        Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
    let status = json!({
        "status": {
            "phase": "Running",
            "namespace": sandbox_ns,
            "sandboxPod": format!("{name}-*"),
            "inferenceEndpoint": format!("https://azureclaw-inference-router.azureclaw-system.svc.cluster.local:8443"),
            "pendingApprovals": 0
        }
    });
    let _ = sandbox_api
        .patch_status(&name, &PatchParams::apply("azureclaw-controller"), &Patch::Merge(status))
        .await;

    tracing::info!("ClawSandbox {name} reconciled successfully");
    Ok(Action::requeue(Duration::from_secs(300)))
}

/// Error policy — what to do when reconciliation fails.
fn error_policy(sandbox: Arc<ClawSandbox>, error: &ReconcileError, _ctx: Arc<Context>) -> Action {
    tracing::error!(
        "Reconciliation error for {}: {:?}",
        sandbox.name_any(),
        error
    );
    Action::requeue(Duration::from_secs(30))
}

/// Run the controller — blocks forever, watching ClawSandbox CRDs.
pub async fn run(client: Client) -> Result<()> {
    let sandboxes: Api<ClawSandbox> = Api::all(client.clone());

    // Verify CRD is installed
    sandboxes.list(&ListParams::default().limit(1)).await
        .map_err(|e| anyhow::anyhow!("ClawSandbox CRD not found — install the Helm chart first: {e}"))?;
    tracing::info!("ClawSandbox CRD found — starting controller");

    let ctx = Arc::new(Context { client });

    Controller::new(sandboxes, kube::runtime::watcher::Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::info!("Reconciled {:?}", o),
                Err(e) => tracing::warn!("Reconcile failed: {e:?}"),
            }
        })
        .await;

    Ok(())
}
