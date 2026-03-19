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

use crate::crd::{ClawSandbox, SandboxConfig};

/// Build pod security context, conditionally including SELinux options and
/// choosing between RuntimeDefault and Localhost seccomp profiles.
/// For Kata (confidential), we use RuntimeDefault since the VM provides isolation.
fn build_pod_security_context(cfg: &SandboxConfig) -> serde_json::Value {
    // Standard and Confidential use RuntimeDefault seccomp:
    //   standard     — basic container isolation, kernel-default syscall filter
    //   confidential — Kata VM boundary is the isolation layer
    // Enhanced uses custom Localhost seccomp (azureclaw-strict) for strict syscall allowlist
    let seccomp = if cfg.isolation == "confidential"
        || cfg.isolation == "standard"
        || cfg.seccomp_profile == "RuntimeDefault"
        || cfg.seccomp_profile.is_empty()
    {
        json!({ "type": "RuntimeDefault" })
    } else {
        json!({
            "type": "Localhost",
            "localhostProfile": format!("profiles/{}.json", cfg.seccomp_profile)
        })
    };

    let mut ctx = json!({
        "runAsNonRoot": cfg.run_as_non_root,
        "runAsUser": 1000,
        "runAsGroup": 1000,
        "fsGroup": 1000,
        "seccompProfile": seccomp
    });

    // Only set seLinuxOptions if a non-empty context is specified
    if !cfg.selinux_context.is_empty() {
        ctx.as_object_mut().unwrap().insert(
            "seLinuxOptions".into(),
            json!({ "type": cfg.selinux_context }),
        );
    }

    ctx
}

/// Returns (runtimeClassName, nodeSelector) based on the isolation level.
///   standard   → runc on clawpool, no custom seccomp
///   enhanced   → runc on clawpool + Localhost seccomp (azureclaw-strict)
///   confidential → Kata VM isolation on katapool
fn isolation_scheduling(isolation: &str) -> (Option<&'static str>, &'static str) {
    match isolation {
        "confidential" => (Some("kata-vm-isolation"), "sandbox-kata"),
        _ => (None, "sandbox"), // standard + enhanced both on clawpool
    }
}

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
    /// Workload Identity client ID — injected via AZURE_WI_CLIENT_ID env
    wi_client_id: String,
    /// Inference router image — injected via INFERENCE_ROUTER_IMAGE env
    inference_router_image: String,
    /// Sandbox image — injected via SANDBOX_IMAGE env
    sandbox_image: String,
    /// Azure OpenAI endpoint — injected via AZURE_OPENAI_ENDPOINT env
    openai_endpoint: String,
    /// Foundry Models endpoint — injected via FOUNDRY_ENDPOINT env
    foundry_endpoint: String,
    /// Kubelet MI client ID for IMDS fallback — injected via IMDS_CLIENT_ID env
    imds_client_id: String,
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
                "azure.workload.identity/client-id": ctx.wi_client_id
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
        // Allow DNS — target the kube-dns ClusterIP directly (works with all CNIs
        // including Cilium where namespace selectors don't match service VIPs)
        json!({
            "to": [
                {"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}}},
                {"ipBlock": {"cidr": "10.0.0.10/32"}}
            ],
            "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]
        }),
        // Allow IMDS (Azure Instance Metadata Service) for managed identity token acquisition
        json!({
            "to": [{"ipBlock": {"cidr": "169.254.169.254/32"}}],
            "ports": [{"protocol": "TCP", "port": 80}]
        }),
        // Always allow inference router sidecar (localhost, but explicit for clarity)
        json!({
            "to": [{"namespaceSelector": {"matchLabels": {"app.kubernetes.io/name": "azureclaw"}},
                     "podSelector": {"matchLabels": {"azureclaw.azure.com/component": "inference-router"}}}],
            "ports": [{"protocol": "TCP", "port": 8443}]
        }),
        // Allow HTTPS egress for Workload Identity (login.microsoftonline.com)
        // and Azure OpenAI / Foundry / APIM. Firewall rules on AOAI side
        // restrict access to only the AKS egress IP.
        json!({
            "to": [{"ipBlock": {"cidr": "0.0.0.0/0", "except": ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]}}],
            "ports": [{"protocol": "TCP", "port": 443}]
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
        .unwrap_or_else(|| ctx.sandbox_image.clone());

    let (runtime_class, pool_label) = isolation_scheduling(&sandbox_config.isolation);

    let pull_policy = if image.ends_with(":latest") { "Always" } else { "IfNotPresent" };

    let deploy_api: Api<Deployment> = Api::namespaced(client.clone(), &sandbox_ns);

    // Build the pod spec — runtimeClassName only set for Kata (confidential)
    let mut pod_spec = json!({
        "serviceAccountName": "sandbox",
        "securityContext": build_pod_security_context(&sandbox_config),
        "containers": [
                        {
                            "name": "openclaw",
                            "image": image,
                            "imagePullPolicy": pull_policy,
                            "ports": [{"containerPort": 18789, "name": "gateway"}],
                            "env": [
                                {"name": "OPENCLAW_MODEL", "value": inference_config.model},
                                {"name": "AZURE_OPENAI_ENDPOINT", "value": &ctx.openai_endpoint},
                                {"name": "AZURECLAW_AUTH_MODE", "value": "workload-identity"}
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
                            })),
                            "livenessProbe": {
                                "exec": {
                                    "command": ["sh", "-c", "test -f /proc/1/status"]
                                },
                                "initialDelaySeconds": 15,
                                "periodSeconds": 30
                            },
                            "readinessProbe": {
                                "exec": {
                                    "command": ["sh", "-c", "test -f /proc/1/status"]
                                },
                                "initialDelaySeconds": 5,
                                "periodSeconds": 10
                            }
                        },
                        {
                            "name": "inference-router",
                            "image": &ctx.inference_router_image,
                            "ports": [
                                {"containerPort": 8443, "name": "inference"},
                                {"containerPort": 9090, "name": "metrics"}
                            ],
                            "env": [
                                {"name": "AZURE_OPENAI_ENDPOINT", "value": &ctx.openai_endpoint},
                                {"name": "FOUNDRY_ENDPOINT", "value": &ctx.foundry_endpoint},
                                {"name": "IMDS_CLIENT_ID", "value": &ctx.imds_client_id},
                                {"name": "AZURE_OPENAI_DEPLOYMENT", "value": &inference_config.model},
                                {"name": "AZURECLAW_AUTH_MODE", "value": "workload-identity"},
                                {"name": "AZURECLAW_CONTENT_SAFETY", "value": inference_config.content_safety.to_string()},
                                {"name": "AZURECLAW_PROMPT_SHIELDS", "value": inference_config.prompt_shields.to_string()},
                                {"name": "RUST_LOG", "value": "info,inference_router=debug"}
                            ],
                            "securityContext": {
                                "allowPrivilegeEscalation": false,
                                "readOnlyRootFilesystem": true,
                                "capabilities": {"drop": ["ALL"]}
                            },
                            "resources": {
                                "requests": {"cpu": "100m", "memory": "64Mi"},
                                "limits": {"cpu": "500m", "memory": "256Mi"}
                            },
                            "livenessProbe": {
                                "httpGet": {"path": "/healthz", "port": "inference"},
                                "initialDelaySeconds": 5,
                                "periodSeconds": 15
                            },
                            "readinessProbe": {
                                "httpGet": {"path": "/healthz", "port": "inference"},
                                "initialDelaySeconds": 3,
                                "periodSeconds": 5
                            }
                        }
                    ],
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
                        "azureclaw.azure.com/pool": pool_label
                    }
    });

    // Set runtimeClassName for Kata (confidential) isolation
    if let Some(rc) = runtime_class {
        pod_spec.as_object_mut().unwrap().insert(
            "runtimeClassName".into(),
            json!(rc),
        );
    }

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
                "spec": pod_spec
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

    let wi_client_id = std::env::var("AZURE_WI_CLIENT_ID")
        .unwrap_or_default();
    let inference_router_image = std::env::var("INFERENCE_ROUTER_IMAGE")
        .unwrap_or_else(|_| "azureclawacr.azurecr.io/azureclaw-inference-router:0.1.0".into());
    let sandbox_image = std::env::var("SANDBOX_IMAGE")
        .unwrap_or_else(|_| "azureclawacr.azurecr.io/openclaw-sandbox:latest".into());
    let openai_endpoint = std::env::var("AZURE_OPENAI_ENDPOINT")
        .unwrap_or_default();
    let foundry_endpoint = std::env::var("FOUNDRY_ENDPOINT")
        .unwrap_or_default();
    let imds_client_id = std::env::var("IMDS_CLIENT_ID")
        .unwrap_or_default();

    if !foundry_endpoint.is_empty() {
        tracing::info!("Using Foundry Models endpoint: {foundry_endpoint}");
    }
    if !imds_client_id.is_empty() {
        tracing::info!("IMDS auth enabled with kubelet MI: {imds_client_id}");
    }

    let ctx = Arc::new(Context {
        client,
        wi_client_id,
        inference_router_image,
        sandbox_image,
        openai_endpoint,
        foundry_endpoint,
        imds_client_id,
    });

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
