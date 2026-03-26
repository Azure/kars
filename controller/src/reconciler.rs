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
    core::v1::{ConfigMap, Namespace, Secret, Service, ServiceAccount},
    networking::v1::NetworkPolicy,
    rbac::v1::ClusterRoleBinding,
};
use kube::{
    api::{Api, DeleteParams, ListParams, Patch, PatchParams},
    runtime::controller::{Action, Controller},
    Client, ResourceExt,
};
use serde_json::json;
use std::sync::Arc;
use tokio::time::Duration;

use crate::crd::{ClawSandbox, SandboxConfig};
use crate::fedcred::{FedCredConfig, FedCredManager};

/// Build pod security context, conditionally including SELinux options and
/// choosing between RuntimeDefault and Localhost seccomp profiles.
/// For Kata (confidential), we use RuntimeDefault since the VM provides isolation.
pub(crate) fn build_pod_security_context(cfg: &SandboxConfig) -> serde_json::Value {
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
pub(crate) fn isolation_scheduling(isolation: &str) -> (Option<&'static str>, &'static str) {
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
    /// Foundry project endpoint for standalone APIs (Memory Store, IQ, agents)
    foundry_project_endpoint: String,
    /// JSON array of deployed model names — injected via FOUNDRY_DEPLOYMENTS env
    foundry_deployments: String,
    /// Kubelet MI client ID for IMDS fallback — injected via IMDS_CLIENT_ID env
    imds_client_id: String,
    /// Azure AI Content Safety endpoint — injected via CONTENT_SAFETY_ENDPOINT env
    content_safety_endpoint: String,
    /// Federated credential manager — creates Azure AD fedcreds for sub-agent namespaces.
    /// None if required env vars (AZURE_SUBSCRIPTION_ID, IDENTITY_NAME, etc.) are missing.
    fedcred: Option<FedCredManager>,
}

/// Main reconciliation function — called whenever a ClawSandbox changes.
async fn reconcile(sandbox: Arc<ClawSandbox>, ctx: Arc<Context>) -> Result<Action, ReconcileError> {
    let name = sandbox.name_any();
    let sandbox_ns = format!("azureclaw-{name}");
    let client = &ctx.client;

    tracing::info!("Reconciling ClawSandbox {name}");

    // ── Finalizer: cascading namespace deletion ──────────────────────────
    const FINALIZER: &str = "azureclaw.azure.com/namespace-cleanup";

    if sandbox.metadata.deletion_timestamp.is_some() {
        tracing::info!("ClawSandbox {name} is being deleted — cleaning up namespace {sandbox_ns}");

        // Delete the namespace (cascades to all resources within it)
        let ns_api: Api<Namespace> = Api::all(client.clone());
        match ns_api.delete(&sandbox_ns, &DeleteParams::default()).await {
            Ok(_) => tracing::info!("Namespace {sandbox_ns} deletion initiated"),
            Err(kube::Error::Api(ae)) if ae.code == 404 => {
                tracing::info!("Namespace {sandbox_ns} already gone");
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to delete namespace {sandbox_ns}");
                return Ok(Action::requeue(Duration::from_secs(10)));
            }
        }

        // Clean up the spawner ClusterRoleBinding
        let crb_api: Api<ClusterRoleBinding> = Api::all(client.clone());
        let crb_name = format!("azureclaw-spawner-{name}");
        match crb_api.delete(&crb_name, &DeleteParams::default()).await {
            Ok(_) => tracing::info!("ClusterRoleBinding {crb_name} deleted"),
            Err(kube::Error::Api(ae)) if ae.code == 404 => {}
            Err(e) => tracing::warn!(error = %e, "Failed to delete ClusterRoleBinding {crb_name}"),
        }

        // Clean up the Azure federated identity credential
        if let Some(ref fedcred) = ctx.fedcred {
            if let Err(e) = fedcred.delete_federated_credential(&name).await {
                tracing::warn!(sandbox = %name, "Federated credential cleanup failed (non-fatal): {e}");
            }
        }

        // Remove the finalizer so K8s can complete CRD deletion
        let sandbox_api: Api<ClawSandbox> =
            Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
        let patch = json!({
            "metadata": {
                "finalizers": sandbox.metadata.finalizers.as_ref()
                    .map(|f| f.iter().filter(|x| x.as_str() != FINALIZER).collect::<Vec<_>>())
                    .unwrap_or_default()
            }
        });
        let _ = sandbox_api
            .patch(&name, &PatchParams::default(), &Patch::Merge(patch))
            .await;

        tracing::info!("ClawSandbox {name} cleanup complete");
        return Ok(Action::await_change());
    }

    // Ensure our finalizer is present (add it if missing)
    let has_finalizer = sandbox.metadata.finalizers.as_ref()
        .is_some_and(|f| f.iter().any(|x| x == FINALIZER));
    if !has_finalizer {
        let sandbox_api: Api<ClawSandbox> =
            Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
        let mut finalizers = sandbox.metadata.finalizers.clone().unwrap_or_default();
        finalizers.push(FINALIZER.to_string());
        let patch = json!({ "metadata": { "finalizers": finalizers } });
        sandbox_api
            .patch(&name, &PatchParams::default(), &Patch::Merge(patch))
            .await?;
        tracing::info!("Added finalizer to ClawSandbox {name}");
    }

    let spec = sandbox.spec.clone();
    let sandbox_config = spec.sandbox.unwrap_or_default();
    let inference_config = spec.inference.unwrap_or_default();
    let openclaw_config = spec.openclaw.unwrap_or_default();
    let agent_config = spec.agent.unwrap_or_default();

    // ── Validate CRD inputs ──────────────────────────────────────────────
    let isolation = &sandbox_config.isolation;
    if !["standard", "enhanced", "confidential"].contains(&isolation.as_str()) {
        tracing::error!("Invalid isolation level: {isolation} (must be standard/enhanced/confidential)");
        return Ok(Action::requeue(Duration::from_secs(60)));
    }
    if inference_config.model.is_empty() {
        tracing::error!("ClawSandbox {name} has empty model — skipping reconciliation");
        return Ok(Action::requeue(Duration::from_secs(60)));
    }
    if ctx.foundry_endpoint.is_empty() && ctx.openai_endpoint.is_empty() {
        tracing::error!("No inference endpoint configured (FOUNDRY_ENDPOINT or AZURE_OPENAI_ENDPOINT)");
        return Ok(Action::requeue(Duration::from_secs(60)));
    }

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
                "azureclaw.azure.com/role": "sandbox",
                "pod-security.kubernetes.io/enforce": "privileged",
                "pod-security.kubernetes.io/audit": "baseline",
                "pod-security.kubernetes.io/warn": "baseline"
            }
        }
    }))?;
    ns_api
        .patch(
            &sandbox_ns,
            &PatchParams::apply("azureclaw-controller").force(),
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
            &PatchParams::apply("azureclaw-controller").force(),
            &Patch::Apply(sa),
        )
        .await?;

    // ── Step 2b: Create Azure federated identity credential ──────────────
    // Maps system:serviceaccount:{namespace}:sandbox → managed identity so
    // Workload Identity token exchange works for this sub-agent.
    if let Some(ref fedcred) = ctx.fedcred {
        if let Err(e) = fedcred.ensure_federated_credential(&name, &sandbox_ns).await {
            tracing::warn!(sandbox = %name, "Federated credential creation failed (non-fatal): {e}");
        }
    }

    // ── Step 2a: Grant sandbox SA permission to spawn sub-agents ─────────
    // Bind the sandbox SA to the azureclaw-sandbox-spawner ClusterRole so
    // agents can create/list/delete ClawSandbox CRDs for sub-agent spawning.
    let crb_api: Api<ClusterRoleBinding> = Api::all(client.clone());
    let crb_name = format!("azureclaw-spawner-{}", name);
    let crb: ClusterRoleBinding = serde_json::from_value(json!({
        "apiVersion": "rbac.authorization.k8s.io/v1",
        "kind": "ClusterRoleBinding",
        "metadata": {
            "name": crb_name,
            "labels": {
                "azureclaw.azure.com/sandbox": name,
                "app.kubernetes.io/managed-by": "azureclaw-controller"
            }
        },
        "roleRef": {
            "apiGroup": "rbac.authorization.k8s.io",
            "kind": "ClusterRole",
            "name": "azureclaw-sandbox-spawner"
        },
        "subjects": [{
            "kind": "ServiceAccount",
            "name": "sandbox",
            "namespace": sandbox_ns
        }]
    }))?;
    crb_api
        .patch(
            &crb_name,
            &PatchParams::apply("azureclaw-controller").force(),
            &Patch::Apply(crb),
        )
        .await?;

    // ── Step 2b: Create/reuse gateway token Secret ───────────────────────
    // Shared between openclaw and inference-router containers so the TUI
    // can authenticate to the gateway running inside the pod.
    let secret_api: Api<Secret> = Api::namespaced(client.clone(), &sandbox_ns);
    let gateway_token = {
        // Reuse existing token if the Secret already exists (idempotent reconcile)
        let existing = secret_api.get_opt("gateway-token").await?;
        if let Some(ref s) = existing {
            s.data
                .as_ref()
                .and_then(|d| d.get("token"))
                .and_then(|v| String::from_utf8(v.0.clone()).ok())
                .unwrap_or_default()
        } else {
            String::new()
        }
    };
    let gateway_token = if gateway_token.is_empty() {
        // Generate a new 32-char alphanumeric token using OS randomness
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let mut token = String::with_capacity(32);
        for _ in 0..4 {
            let s = RandomState::new();
            let mut h = s.build_hasher();
            h.write_u64(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64);
            token.push_str(&format!("{:016x}", h.finish()));
        }
        token.truncate(32);
        token
    } else {
        gateway_token
    };
    let gw_secret: Secret = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": "gateway-token",
            "namespace": sandbox_ns,
            "labels": {
                "azureclaw.azure.com/sandbox": name
            }
        },
        "stringData": {
            "token": gateway_token
        }
    }))?;
    secret_api
        .patch(
            "gateway-token",
            &PatchParams::apply("azureclaw-controller").force(),
            &Patch::Apply(gw_secret),
        )
        .await?;

    // ── Step 2b: Generate per-sandbox admin token for router ───────────
    //
    // Protects sensitive router endpoints (/admin/*, /egress/*, /sandbox/*, /agt/audit, etc.)
    // Only mounted in the inference-router container — the agent (openclaw) never sees it.
    let admin_token = {
        let existing = secret_api.get_opt("router-admin-token").await?;
        if let Some(ref s) = existing {
            s.data
                .as_ref()
                .and_then(|d| d.get("token"))
                .and_then(|v| String::from_utf8(v.0.clone()).ok())
                .unwrap_or_default()
        } else {
            String::new()
        }
    };
    let admin_token = if admin_token.is_empty() {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let mut token = String::with_capacity(64);
        for _ in 0..8 {
            let s = RandomState::new();
            let mut h = s.build_hasher();
            h.write_u64(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64);
            token.push_str(&format!("{:016x}", h.finish()));
        }
        token.truncate(64);
        token
    } else {
        admin_token
    };
    let admin_secret: Secret = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": "router-admin-token",
            "namespace": sandbox_ns,
            "labels": {
                "azureclaw.azure.com/sandbox": name
            }
        },
        "stringData": {
            "token": admin_token
        }
    }))?;
    secret_api
        .patch(
            "router-admin-token",
            &PatchParams::apply("azureclaw-controller").force(),
            &Patch::Apply(admin_secret),
        )
        .await?;

    // ── Step 3: Apply default-deny NetworkPolicy + allowlist ─────────────
    //
    // Pod-level NetworkPolicy controls what the entire pod can reach.
    // Per-container egress restriction (agent vs. inference-router) is enforced
    // by the iptables init container below (UID-based rules), since K8s
    // NetworkPolicy has no per-container granularity.
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
        // Allow IMDS (Azure Instance Metadata Service) for managed identity token acquisition.
        // NOTE: iptables init container restricts this to UID 1001 (inference-router) only.
        // The openclaw agent container (UID 1000) is blocked from IMDS by iptables.
        json!({
            "to": [{"ipBlock": {"cidr": "169.254.169.254/32"}}],
            "ports": [{"protocol": "TCP", "port": 80}]
        }),
        // Allow HTTPS egress for inference-router only (Workload Identity,
        // Azure OpenAI, Foundry, Content Safety). The openclaw agent container
        // is blocked from all external HTTPS by the iptables init container —
        // it can only reach localhost:8443 (inference-router sidecar) and DNS.
        json!({
            "to": [{"ipBlock": {"cidr": "0.0.0.0/0", "except": ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]}}],
            "ports": [{"protocol": "TCP", "port": 443}]
        }),
        // Allow AGT mesh egress: inference-router → other sandbox routers on port 8443.
        // This permits cross-namespace mesh communication between parent and child agents.
        json!({
            "to": [{"namespaceSelector": {"matchLabels": {"azureclaw.azure.com/role": "sandbox"}}}],
            "ports": [{"protocol": "TCP", "port": 8443}]
        }),
        // Allow AGT relay/registry egress: inference-router → self-hosted agentmesh services.
        // Relay (WebSocket, port 8765) and registry (HTTP, port 8080) in the agentmesh namespace.
        json!({
            "to": [{"namespaceSelector": {"matchLabels": {"app.kubernetes.io/managed-by": "azureclaw"}}}],
            "ports": [{"protocol": "TCP", "port": 8765}, {"protocol": "TCP", "port": 8080}]
        }),
    ];

    // Add user-defined allowed endpoints (for the inference-router to reach
    // on behalf of the agent — agent itself can only reach localhost)
    if let Some(ref policy) = spec.network_policy
        && let Some(ref endpoints) = policy.allowed_endpoints
    {
        for ep in endpoints {
            let port = ep.port.unwrap_or(443);
            if port != 443 {
                // Only add non-443 ports (443 is already covered by the blanket HTTPS rule)
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
            &PatchParams::apply("azureclaw-controller").force(),
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

    // Token budget values from CRD (0 = unlimited)
    let token_budget_daily = inference_config
        .token_budget
        .as_ref()
        .and_then(|b| b.daily)
        .unwrap_or(0);
    let token_budget_per_request = inference_config
        .token_budget
        .as_ref()
        .and_then(|b| b.per_request)
        .unwrap_or(0);

    // Build OpenClaw container env vars
    let mut openclaw_env = vec![
        json!({"name": "OPENCLAW_MODEL", "value": inference_config.model}),
        json!({"name": "AZURE_OPENAI_ENDPOINT", "value": &ctx.openai_endpoint}),
        json!({"name": "AZURECLAW_AUTH_MODE", "value": "workload-identity"}),
        json!({"name": "OPENCLAW_GATEWAY_TOKEN", "value": &gateway_token}),
    ];
    // Foundry project endpoint (for standalone APIs: Memory Store, Foundry IQ, etc.)
    if !ctx.foundry_project_endpoint.is_empty() {
        openclaw_env.push(json!({"name": "FOUNDRY_PROJECT_ENDPOINT", "value": &ctx.foundry_project_endpoint}));
    }
    // Foundry deployments list (so plugin shows only deployed models, not full catalog)
    if !ctx.foundry_deployments.is_empty() {
        openclaw_env.push(json!({"name": "FOUNDRY_DEPLOYMENTS", "value": &ctx.foundry_deployments}));
    }
    // Inject Foundry Agent ID if set in status (for tools needing agent runs)
    if let Some(ref agent_id) = sandbox.status.as_ref().and_then(|s| s.foundry_agent_id.clone())
        && !agent_id.is_empty()
    {
        openclaw_env.push(json!({"name": "FOUNDRY_AGENT_ID", "value": agent_id}));
    }
    // Signal configured Foundry agent tools
    if let Some(ref tools) = agent_config.tools
        && !tools.is_empty()
    {
        openclaw_env.push(json!({"name": "FOUNDRY_AGENT_TOOLS", "value": tools.join(",")}));
    }
    // AGT governance env vars (opt-in) — injected into BOTH openclaw and router containers
    let governance_config = spec.governance.unwrap_or_default();
    let mut router_agt_env: Vec<serde_json::Value> = Vec::new();
    if governance_config.enabled {
        openclaw_env.push(json!({"name": "AGT_GOVERNANCE_ENABLED", "value": "true"}));
        openclaw_env.push(json!({"name": "AGT_POLICY_PROFILE", "value": governance_config.tool_policy}));
        openclaw_env.push(json!({"name": "AGT_TRUST_THRESHOLD", "value": governance_config.trust_threshold.to_string()}));
        // Router needs these too for the AGT governance module
        router_agt_env.push(json!({"name": "AGT_GOVERNANCE_ENABLED", "value": "true"}));
        router_agt_env.push(json!({"name": "AGT_POLICY_PROFILE", "value": &governance_config.tool_policy}));
        router_agt_env.push(json!({"name": "AGT_TRUST_THRESHOLD", "value": governance_config.trust_threshold.to_string()}));
        // Mesh namespace for K8s DNS routing between agents
        router_agt_env.push(json!({"name": "AGT_MESH_NAMESPACE", "value": &sandbox_ns}));
        // Self-hosted AGT relay + registry (for E2E encrypted inter-agent comms)
        router_agt_env.push(json!({"name": "AGT_RELAY_URL", "value": "ws://agentmesh-relay.agentmesh.svc.cluster.local:8765"}));
        router_agt_env.push(json!({"name": "AGT_REGISTRY_URL", "value": "http://agentmesh-registry.agentmesh.svc.cluster.local:8080"}));
    }

    // Build the inference-router env array
    let mut router_env = vec![
        json!({"name": "AZURE_OPENAI_ENDPOINT", "value": &ctx.openai_endpoint}),
        json!({"name": "FOUNDRY_ENDPOINT", "value": &ctx.foundry_endpoint}),
        json!({"name": "FOUNDRY_PROJECT_ENDPOINT", "value": &ctx.foundry_project_endpoint}),
        json!({"name": "IMDS_CLIENT_ID", "value": &ctx.imds_client_id}),
        json!({"name": "AZURE_OPENAI_DEPLOYMENT", "value": &inference_config.model}),
        json!({"name": "AZURECLAW_AUTH_MODE", "value": "workload-identity"}),
        json!({"name": "CONTENT_SAFETY_ENABLED", "value": inference_config.content_safety.to_string()}),
        json!({"name": "PROMPT_SHIELDS_ENABLED", "value": inference_config.prompt_shields.to_string()}),
        json!({"name": "CONTENT_SAFETY_ENDPOINT", "value": &ctx.content_safety_endpoint}),
        json!({"name": "TOKEN_BUDGET_DAILY", "value": token_budget_daily.to_string()}),
        json!({"name": "TOKEN_BUDGET_PER_REQUEST", "value": token_budget_per_request.to_string()}),
        json!({"name": "SANDBOX_NAME", "value": &name}),
        json!({"name": "SANDBOX_ISOLATION", "value": &sandbox_config.isolation}),
        json!({"name": "ADMIN_TOKEN", "value": &admin_token}),
        json!({"name": "RUST_LOG", "value": "info,inference_router=debug"}),
    ];
    router_env.extend(router_agt_env);

    // ── Blocklist ConfigMap + env vars ──
    // The blocklist is always-on: router loads a seed file at startup, then
    // auto-refreshes from OISD + URLhaus feeds every 6 hours.
    let blocklist_cm_name = format!("{}-blocklist", &name);
    router_env.push(json!({"name": "BLOCKLIST_ENABLED", "value": "true"}));
    router_env.push(json!({"name": "BLOCKLIST_SEED_PATH", "value": "/etc/azureclaw/blocklist/domains.txt"}));

    // Egress learn mode — enabled by default so operators can discover required domains.
    // Blocklist (threat intelligence) is still enforced. Disable with network_policy.learn_egress=false.
    let learn_egress = spec.network_policy.as_ref().is_none_or(|np| np.learn_egress);
    if learn_egress {
        router_env.push(json!({"name": "EGRESS_LEARN_MODE", "value": "true"}));
    }

    // Build the pod spec — runtimeClassName only set for Kata (confidential)
    let mut pod_spec = json!({
        "serviceAccountName": "sandbox",
        "securityContext": build_pod_security_context(&sandbox_config),
        // ── Init container: iptables-based per-container egress control ──
        // Since K8s NetworkPolicy operates at pod level (not container level),
        // we use iptables UID-based rules to restrict the openclaw agent
        // container (UID 1000) to localhost + DNS only, with a transparent
        // forward proxy for HTTP/HTTPS egress enforcement and learn mode.
        //
        // Filter chain (OUTPUT):
        //   UID 1000 → allow loopback, DNS, established → DROP everything else
        //
        // NAT chain (OUTPUT):
        //   UID 1000 port 80/443 (not loopback) → REDIRECT to :8444
        //   The transparent proxy on port 8444 (inference-router, UID 1001):
        //   - Records every domain for learn mode
        //   - Enforces blocklist/allowlist per domain
        //   - Tunnels allowed traffic to the real destination
        //
        // This blocks:
        //  - IMDS credential theft (169.254.169.254)
        //  - Data exfiltration to any external host
        //  - Lateral movement to other pods
        //
        // The agent can only reach the inference-router sidecar on localhost:8443.
        // HTTP/HTTPS goes through the transparent proxy for policy enforcement.
        "initContainers": [{
            "name": "egress-guard",
            "image": &ctx.inference_router_image,
            "command": ["sh", "-c", concat!(
                // Filter chain: allow localhost, DNS, established — drop everything else
                "iptables -A OUTPUT -m owner --uid-owner 1000 -o lo -j ACCEPT && ",
                "iptables -A OUTPUT -m owner --uid-owner 1000 -p udp --dport 53 -j ACCEPT && ",
                "iptables -A OUTPUT -m owner --uid-owner 1000 -p tcp --dport 53 -j ACCEPT && ",
                // Allow reply packets (SYN-ACK etc.) for inbound connections to the
                // gateway — without this, the WebUX and Telegram channel can't respond.
                "iptables -A OUTPUT -m owner --uid-owner 1000 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT && ",
                "iptables -A OUTPUT -m owner --uid-owner 1000 -j DROP && ",
                // NAT chain: redirect HTTP/HTTPS from UID 1000 to the transparent
                // forward proxy (port 8444) in the inference-router sidecar. This
                // enables learn mode (domain discovery) and per-domain enforcement.
                // Redirected packets go to 127.0.0.1:8444, matching the -o lo ACCEPT
                // rule above. The proxy (UID 1001) then connects to the real destination.
                "iptables -t nat -A OUTPUT -m owner --uid-owner 1000 ! -o lo -p tcp --dport 80 -j REDIRECT --to-port 8444 && ",
                "iptables -t nat -A OUTPUT -m owner --uid-owner 1000 ! -o lo -p tcp --dport 443 -j REDIRECT --to-port 8444 && ",
                "echo 'egress-guard: UID 1000 → transparent proxy on :8444 (learn + enforce)'"
            )],
            "securityContext": {
                "runAsUser": 0,
                "runAsNonRoot": false,
                "seccompProfile": { "type": "Unconfined" },
                "capabilities": {
                    "add": ["NET_ADMIN", "NET_RAW"],
                    "drop": ["ALL"]
                }
            },
            "resources": {
                "requests": {"cpu": "10m", "memory": "32Mi"},
                "limits": {"cpu": "200m", "memory": "256Mi"}
            }
        }],
        "containers": [
                        {
                            "name": "openclaw",
                            "image": image,
                            "imagePullPolicy": pull_policy,
                            "ports": [{"containerPort": 18789, "name": "gateway"}],
                            "env": openclaw_env,
                            "envFrom": [
                                {"secretRef": {"name": format!("{}-credentials", name), "optional": true}}
                            ],
                            "securityContext": {
                                "runAsUser": 1000,
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
                            "env": router_env,
                            "securityContext": {
                                "runAsUser": 1001,
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

    // If AGT governance is enabled, mount the policy ConfigMap into the router
    if governance_config.enabled {
        let policy_profile = &governance_config.tool_policy;
        let cm_name = format!("agt-policy-{}", policy_profile);

        // Add policy volume
        if let Some(volumes) = pod_spec.get_mut("volumes").and_then(|v| v.as_array_mut()) {
            volumes.push(json!({
                "name": "agt-policy",
                "configMap": {
                    "name": cm_name
                }
            }));
        }

        // Add volumeMount + AGT_POLICY_DIR env to the router container
        if let Some(containers) = pod_spec.get_mut("containers").and_then(|c| c.as_array_mut()) {
            for container in containers.iter_mut() {
                if container.get("name").and_then(|n| n.as_str()) == Some("inference-router") {
                    // Add volumeMount
                    let mounts = container
                        .as_object_mut().unwrap()
                        .entry("volumeMounts")
                        .or_insert(json!([]));
                    if let Some(mounts_arr) = mounts.as_array_mut() {
                        mounts_arr.push(json!({
                            "name": "agt-policy",
                            "mountPath": "/etc/agt/policies",
                            "readOnly": true
                        }));
                    }
                    // Add AGT_POLICY_DIR env var pointing to the mounted path
                    if let Some(env) = container.get_mut("env").and_then(|e| e.as_array_mut()) {
                        env.push(json!({"name": "AGT_POLICY_DIR", "value": "/etc/agt/policies"}));
                    }
                }
            }
        }
    }

    // Mount blocklist seed ConfigMap into the router container
    if let Some(volumes) = pod_spec.get_mut("volumes").and_then(|v| v.as_array_mut()) {
        volumes.push(json!({
            "name": "blocklist-seed",
            "configMap": {
                "name": &blocklist_cm_name,
                "optional": true
            }
        }));
    }
    if let Some(containers) = pod_spec.get_mut("containers").and_then(|c| c.as_array_mut()) {
        for container in containers.iter_mut() {
            if container.get("name").and_then(|n| n.as_str()) == Some("inference-router") {
                let mounts = container
                    .as_object_mut().unwrap()
                    .entry("volumeMounts")
                    .or_insert(json!([]));
                if let Some(mounts_arr) = mounts.as_array_mut() {
                    mounts_arr.push(json!({
                        "name": "blocklist-seed",
                        "mountPath": "/etc/azureclaw/blocklist",
                        "readOnly": true
                    }));
                }
            }
        }
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
            &PatchParams::apply("azureclaw-controller").force(),
            &Patch::Apply(deployment),
        )
        .await?;

    // ── Step 4b: Azure Services RBAC annotations ─────────────────────────
    // If spec.azure_services is configured, annotate the ServiceAccount and
    // namespace so that `azureclaw up` (or a future RBAC controller) can
    // create the necessary Azure role assignments for the sandbox identity.
    if let Some(ref azure_services) = spec.azure_services
        && !azure_services.is_empty()
    {
        let sa_api: Api<k8s_openapi::api::core::v1::ServiceAccount> =
            Api::namespaced(client.clone(), &sandbox_ns);
        let mut annotations = std::collections::BTreeMap::new();
        for (i, svc) in azure_services.iter().enumerate() {
            annotations.insert(
                format!("azureclaw.azure.com/service-{i}"),
                svc.service.clone(),
            );
            if let Some(ref acct) = svc.account {
                annotations.insert(
                    format!("azureclaw.azure.com/service-{i}-account"),
                    acct.clone(),
                );
            }
            if let Some(ref perms) = svc.permissions {
                annotations.insert(
                    format!("azureclaw.azure.com/service-{i}-permissions"),
                    perms.join(","),
                );
            }
        }
        let sa_patch = json!({
            "apiVersion": "v1",
            "kind": "ServiceAccount",
            "metadata": {
                "name": &name,
                "namespace": &sandbox_ns,
                "annotations": annotations,
            }
        });
        sa_api
            .patch(
                &name,
                &PatchParams::apply("azureclaw-controller").force(),
                &Patch::Apply(serde_json::from_value::<k8s_openapi::api::core::v1::ServiceAccount>(sa_patch)?),
            )
            .await?;
        tracing::info!(
            sandbox = %name,
            services = azure_services.len(),
            "Azure services RBAC annotations applied to ServiceAccount"
        );
    }

    // ── Step 4c: AGT governance infrastructure ──────────────────────────
    // When governance is enabled, create:
    //  - K8s Service exposing port 8443 (enables mesh DNS: {name}.{ns}.svc.cluster.local)
    //  - ConfigMap with the policy YAML (mounted into router container at /etc/agt/policies/)
    //  - NetworkPolicy ingress rule allowing mesh traffic on port 8443
    if governance_config.enabled {
        // Create Service for inter-agent mesh DNS routing
        let svc_api: Api<Service> = Api::namespaced(client.clone(), &sandbox_ns);
        let svc: Service = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {
                "name": &name,
                "namespace": &sandbox_ns,
                "labels": {
                    "azureclaw.azure.com/sandbox": &name,
                    "azureclaw.azure.com/component": "sandbox"
                }
            },
            "spec": {
                "selector": {
                    "azureclaw.azure.com/sandbox": &name
                },
                "ports": [{
                    "name": "inference",
                    "port": 8443,
                    "targetPort": 8443,
                    "protocol": "TCP"
                }]
            }
        }))?;
        svc_api
            .patch(
                &name,
                &PatchParams::apply("azureclaw-controller").force(),
                &Patch::Apply(svc),
            )
            .await?;

        // Create ConfigMap with default policy YAML
        let policy_profile = &governance_config.tool_policy;
        let policy_yaml = include_str!("../../cli/policies/azureclaw-default.yaml");
        let cm_api: Api<ConfigMap> = Api::namespaced(client.clone(), &sandbox_ns);
        let cm_name = format!("agt-policy-{}", policy_profile);
        let cm: ConfigMap = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": &cm_name,
                "namespace": &sandbox_ns,
                "labels": {
                    "azureclaw.azure.com/sandbox": &name,
                    "azureclaw.azure.com/component": "agt-policy"
                }
            },
            "data": {
                format!("azureclaw-{}.yaml", policy_profile): policy_yaml
            }
        }))?;
        cm_api
            .patch(
                &cm_name,
                &PatchParams::apply("azureclaw-controller").force(),
                &Patch::Apply(cm),
            )
            .await?;

        // Patch NetworkPolicy to allow ingress on port 8443 for mesh messages
        // and ports 18789/18791 for the gateway WebUX + WebSocket
        let mesh_ingress_patch = json!({
            "apiVersion": "networking.k8s.io/v1",
            "kind": "NetworkPolicy",
            "metadata": {
                "name": "sandbox-policy",
                "namespace": &sandbox_ns
            },
            "spec": {
                "podSelector": {"matchLabels": {"azureclaw.azure.com/component": "sandbox"}},
                "policyTypes": ["Ingress"],
                "ingress": [{
                    "from": [{
                        "namespaceSelector": {
                            "matchLabels": {"azureclaw.azure.com/role": "sandbox"}
                        }
                    }],
                    "ports": [
                        {"port": 8443, "protocol": "TCP"},
                        {"port": 18789, "protocol": "TCP"},
                        {"port": 18791, "protocol": "TCP"}
                    ]
                }]
            }
        });
        let _ = np_api
            .patch(
                "sandbox-policy",
                &PatchParams::apply("azureclaw-controller").force(),
                &Patch::Apply(serde_json::from_value::<NetworkPolicy>(mesh_ingress_patch)?),
            )
            .await;

        tracing::info!(
            sandbox = %name,
            service = format!("{name}.{sandbox_ns}.svc.cluster.local:8443"),
            policy_cm = %cm_name,
            "AGT governance infrastructure created (Service + ConfigMap + mesh ingress)"
        );
    }

    // ── Step 4d: Blocklist seed ConfigMap + CronJob ──────────────────────
    // Create ConfigMap with seed blocklist (immediate protection from day 0).
    // Create CronJob that fetches fresh lists from OISD + URLhaus every 6h
    // and patches the ConfigMap — so even if the router can't reach feeds
    // directly, the mounted file stays fresh.
    {
        let full_seed = include_str!("../../cli/blocklists/seed-domains.txt");
        // Truncate to stay under 1MB ConfigMap limit (K8s rejects >1048576 bytes)
        let seed_domains = if full_seed.len() > 900_000 {
            // Take the first N lines that fit in 900KB
            let mut end = 900_000;
            while end > 0 && !full_seed.is_char_boundary(end) { end -= 1; }
            // Find last newline before the boundary
            if let Some(pos) = full_seed[..end].rfind('\n') { &full_seed[..pos] } else { &full_seed[..end] }
        } else {
            full_seed
        };
        let cm_api: Api<ConfigMap> = Api::namespaced(client.clone(), &sandbox_ns);
        let cm: ConfigMap = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": &blocklist_cm_name,
                "namespace": &sandbox_ns,
                "labels": {
                    "azureclaw.azure.com/sandbox": &name,
                    "azureclaw.azure.com/component": "blocklist"
                }
            },
            "data": {
                "domains.txt": seed_domains
            }
        }))?;
        cm_api
            .patch(
                &blocklist_cm_name,
                &PatchParams::apply("azureclaw-controller").force(),
                &Patch::Apply(cm),
            )
            .await?;

        // CronJob: fetch OISD + URLhaus → patch ConfigMap every 6h
        // Uses kubectl to patch the ConfigMap in-place. The router's background
        // task also fetches feeds directly, so this is defense-in-depth.
        let cronjob_name = format!("{}-blocklist-refresh", &name);
        let cronjob: serde_json::Value = json!({
            "apiVersion": "batch/v1",
            "kind": "CronJob",
            "metadata": {
                "name": &cronjob_name,
                "namespace": &sandbox_ns,
                "labels": {
                    "azureclaw.azure.com/sandbox": &name,
                    "azureclaw.azure.com/component": "blocklist-refresh"
                }
            },
            "spec": {
                "schedule": "0 */6 * * *",
                "successfulJobsHistoryLimit": 1,
                "failedJobsHistoryLimit": 1,
                "jobTemplate": {
                    "spec": {
                        "backoffLimit": 2,
                        "template": {
                            "spec": {
                                "serviceAccountName": "sandbox",
                                "restartPolicy": "OnFailure",
                                "containers": [{
                                    "name": "refresh",
                                    "image": "mcr.microsoft.com/cbl-mariner/base/core:2.0",
                                    "command": ["sh", "-c", concat!(
                                        "set -e; ",
                                        "echo '# Auto-refreshed blocklist' > /tmp/domains.txt; ",
                                        "echo '# Updated: '$(date -u +%Y-%m-%dT%H:%M:%SZ) >> /tmp/domains.txt; ",
                                        // Fetch OISD small list
                                        "curl -sfL --max-time 60 --max-filesize 52428800 ",
                                        "'https://small.oisd.nl/domainswild' >> /tmp/domains.txt 2>/dev/null || ",
                                        "echo '# OISD fetch failed' >> /tmp/domains.txt; ",
                                        // Fetch URLhaus hostfile
                                        "curl -sfL --max-time 60 --max-filesize 52428800 ",
                                        "'https://urlhaus.abuse.ch/downloads/hostfile/' >> /tmp/urlhaus.txt 2>/dev/null && ",
                                        "grep '^127\\.0\\.0\\.1' /tmp/urlhaus.txt | awk '{print $2}' >> /tmp/domains.txt || ",
                                        "echo '# URLhaus fetch failed' >> /tmp/domains.txt; ",
                                        // Patch the ConfigMap via kubectl
                                        "kubectl create configmap ", // Continued in args
                                    )],
                                    "args": [
                                        format!("{} --from-file=domains.txt=/tmp/domains.txt -n {} --dry-run=client -o yaml | kubectl apply -f -",
                                            &blocklist_cm_name, &sandbox_ns)
                                    ],
                                    "resources": {
                                        "requests": {"cpu": "50m", "memory": "64Mi"},
                                        "limits": {"cpu": "200m", "memory": "256Mi"}
                                    },
                                    "securityContext": {
                                        "runAsNonRoot": true,
                                        "runAsUser": 65534,
                                        "readOnlyRootFilesystem": false,
                                        "allowPrivilegeEscalation": false
                                    }
                                }]
                            }
                        }
                    }
                }
            }
        });

        // Use dynamic API for CronJob (batch/v1)
        let cj_gvk = kube::api::GroupVersionKind::gvk("batch", "v1", "CronJob");
        let (cj_ar, _caps) = kube::discovery::pinned_kind(client, &cj_gvk).await?;
        let cj_api: Api<kube::api::DynamicObject> = Api::namespaced_with(
            client.clone(), &sandbox_ns, &cj_ar,
        );
        let cj_obj: kube::api::DynamicObject = serde_json::from_value(cronjob)?;
        let _ = cj_api
            .patch(
                &cronjob_name,
                &PatchParams::apply("azureclaw-controller").force(),
                &Patch::Apply(cj_obj),
            )
            .await;

        tracing::info!(
            sandbox = %name,
            configmap = %blocklist_cm_name,
            cronjob = %cronjob_name,
            "Blocklist infrastructure created (seed ConfigMap + 6h refresh CronJob)"
        );
    }

    // ── Step 5: Update status ────────────────────────────────────────────
    let sandbox_api: Api<ClawSandbox> =
        Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
    let mut status_obj = json!({
        "status": {
            "phase": "Running",
            "namespace": sandbox_ns,
            "sandboxPod": format!("{name}-*"),
            "inferenceEndpoint": format!("https://azureclaw-inference-router.azureclaw-system.svc.cluster.local:8443"),
            "pendingApprovals": 0
        }
    });
    // Preserve existing foundryAgentId in status (set externally or by future controller logic)
    if let Some(ref existing_status) = sandbox.status
        && let Some(ref agent_id) = existing_status.foundry_agent_id
    {
        status_obj["status"]["foundryAgentId"] = json!(agent_id);
    }
    let _ = sandbox_api
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(status_obj))
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
        .unwrap_or_else(|_| {
            tracing::warn!("INFERENCE_ROUTER_IMAGE not set — using default :latest image");
            "azureclawacr.azurecr.io/azureclaw-inference-router:latest".into()
        });
    let sandbox_image = std::env::var("SANDBOX_IMAGE")
        .unwrap_or_else(|_| {
            tracing::warn!("SANDBOX_IMAGE not set — using default :latest image");
            "azureclawacr.azurecr.io/openclaw-sandbox:latest".into()
        });
    let openai_endpoint = std::env::var("AZURE_OPENAI_ENDPOINT")
        .unwrap_or_default();
    let foundry_endpoint = std::env::var("FOUNDRY_ENDPOINT")
        .unwrap_or_default();
    let foundry_project_endpoint = std::env::var("FOUNDRY_PROJECT_ENDPOINT")
        .unwrap_or_default();
    let foundry_deployments = std::env::var("FOUNDRY_DEPLOYMENTS")
        .unwrap_or_default();
    let imds_client_id = std::env::var("IMDS_CLIENT_ID")
        .unwrap_or_default();
    // Content Safety endpoint — defaults to Foundry endpoint if not set separately,
    // since Azure AI Services multi-service resources host Content Safety at the same base URL.
    let content_safety_endpoint = std::env::var("CONTENT_SAFETY_ENDPOINT")
        .unwrap_or_else(|_| foundry_endpoint.clone());

    if openai_endpoint.is_empty() && foundry_endpoint.is_empty() {
        tracing::warn!("Neither AZURE_OPENAI_ENDPOINT nor FOUNDRY_ENDPOINT set — inference routing will fail");
    }
    if !foundry_endpoint.is_empty() {
        tracing::info!("Using Foundry Models endpoint: {foundry_endpoint}");
    }
    if !foundry_project_endpoint.is_empty() {
        tracing::info!("Using Foundry Project endpoint: {foundry_project_endpoint}");
    }
    if !imds_client_id.is_empty() {
        tracing::info!("IMDS auth enabled with kubelet MI: {imds_client_id}");
    }

    // Initialize federated credential manager (if env vars are configured)
    let fedcred = FedCredConfig::from_env().map(|cfg| {
        tracing::info!(
            identity = %cfg.identity_name,
            rg = %cfg.identity_resource_group,
            "Federated credential manager enabled — will auto-create fedcreds for sub-agents",
        );
        FedCredManager::new(cfg)
    });
    if fedcred.is_none() {
        tracing::warn!(
            "Federated credential manager disabled — set AZURE_SUBSCRIPTION_ID, IDENTITY_NAME, \
             IDENTITY_RESOURCE_GROUP, OIDC_ISSUER_URL to enable automatic fedcred creation"
        );
    }

    let ctx = Arc::new(Context {
        client,
        wi_client_id,
        inference_router_image,
        sandbox_image,
        openai_endpoint,
        foundry_endpoint,
        foundry_project_endpoint,
        foundry_deployments,
        imds_client_id,
        content_safety_endpoint,
        fedcred,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::SandboxConfig;

    #[test]
    fn standard_isolation_uses_runtime_default_seccomp() {
        let cfg = SandboxConfig {
            isolation: "standard".into(),
            ..Default::default()
        };
        let ctx = build_pod_security_context(&cfg);
        assert_eq!(ctx["seccompProfile"]["type"], "RuntimeDefault");
    }

    #[test]
    fn enhanced_isolation_uses_localhost_seccomp() {
        let cfg = SandboxConfig {
            isolation: "enhanced".into(),
            seccomp_profile: "azureclaw-strict".into(),
            ..Default::default()
        };
        let ctx = build_pod_security_context(&cfg);
        assert_eq!(ctx["seccompProfile"]["type"], "Localhost");
        assert_eq!(
            ctx["seccompProfile"]["localhostProfile"],
            "profiles/azureclaw-strict.json"
        );
    }

    #[test]
    fn confidential_isolation_uses_runtime_default_seccomp() {
        let cfg = SandboxConfig {
            isolation: "confidential".into(),
            seccomp_profile: "azureclaw-strict".into(),
            ..Default::default()
        };
        let ctx = build_pod_security_context(&cfg);
        // Kata VM provides isolation, so RuntimeDefault is sufficient
        assert_eq!(ctx["seccompProfile"]["type"], "RuntimeDefault");
    }

    #[test]
    fn security_context_enforces_non_root() {
        let cfg = SandboxConfig::default();
        let ctx = build_pod_security_context(&cfg);
        assert_eq!(ctx["runAsNonRoot"], true);
        assert_eq!(ctx["runAsUser"], 1000);
        assert_eq!(ctx["runAsGroup"], 1000);
        assert_eq!(ctx["fsGroup"], 1000);
    }

    #[test]
    fn selinux_context_only_set_when_non_empty() {
        let cfg = SandboxConfig::default(); // empty selinux_context
        let ctx = build_pod_security_context(&cfg);
        assert!(ctx.get("seLinuxOptions").is_none());

        let cfg_with_selinux = SandboxConfig {
            selinux_context: "custom_t".into(),
            ..Default::default()
        };
        let ctx2 = build_pod_security_context(&cfg_with_selinux);
        assert_eq!(ctx2["seLinuxOptions"]["type"], "custom_t");
    }

    #[test]
    fn isolation_scheduling_standard() {
        let (runtime, pool) = isolation_scheduling("standard");
        assert!(runtime.is_none());
        assert_eq!(pool, "sandbox");
    }

    #[test]
    fn isolation_scheduling_enhanced() {
        let (runtime, pool) = isolation_scheduling("enhanced");
        assert!(runtime.is_none());
        assert_eq!(pool, "sandbox");
    }

    #[test]
    fn isolation_scheduling_confidential() {
        let (runtime, pool) = isolation_scheduling("confidential");
        assert_eq!(runtime, Some("kata-vm-isolation"));
        assert_eq!(pool, "sandbox-kata");
    }

    #[test]
    fn crd_defaults_are_secure() {
        let cfg = SandboxConfig::default();
        assert_eq!(cfg.isolation, "enhanced");
        assert!(cfg.read_only_root_filesystem);
        assert!(cfg.run_as_non_root);
        assert!(!cfg.allow_privilege_escalation);
        assert_eq!(cfg.seccomp_profile, "azureclaw-strict");
        assert!(cfg.selinux_context.is_empty());
    }
}
