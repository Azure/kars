// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
    Client, ResourceExt,
    api::{Api, DeleteParams, ListParams, Patch, PatchParams},
    runtime::controller::{Action, Controller},
};
use serde_json::json;
use std::sync::Arc;
use tokio::time::Duration;

use crate::crd::{ClawSandbox, SandboxConfig};
use crate::fedcred::{FedCredConfig, FedCredManager};

pub(crate) mod byo_contract;
pub(crate) mod governance_mounts;

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
    /// Phase 3 S8: when true, BYO sandboxes whose `byo.contractVersion`
    /// does not match a supported version (or whose `image` is shape-
    /// invalid) are rejected with `Degraded=True / Reason=BYOContractInvalid`
    /// instead of reconciling. Default `false` keeps Phase 2 warn-only
    /// behaviour. Wired via `BYO_STRICT_MODE=1` env var (Helm value
    /// `controller.byoStrict`).
    byo_strict: bool,
}

/// Main reconciliation function — called whenever a ClawSandbox changes.
async fn reconcile(sandbox: Arc<ClawSandbox>, ctx: Arc<Context>) -> Result<Action, ReconcileError> {
    let name = sandbox.name_any();

    // Validate sandbox name — must be K8s-safe (alphanumeric + hyphens)
    if name.is_empty()
        || name.len() > 63
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || !name.as_bytes()[0].is_ascii_alphanumeric()
    {
        tracing::error!(sandbox = %name, "Invalid sandbox name — must be lowercase alphanumeric with hyphens");
        // Can't stamp Degraded here: without a K8s-legal name we can't
        // target the CR with patch_status (would 404). Invalid-name CRs
        // never make it into the cluster via the OpenAPI schema anyway;
        // this path only fires on corrupt informer state.
        return Ok(Action::requeue(Duration::from_secs(300)));
    }

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
        if let Some(ref fedcred) = ctx.fedcred
            && let Err(e) = fedcred.delete_federated_credential(&name).await
        {
            tracing::warn!(sandbox = %name, "Federated credential cleanup failed (non-fatal): {e}");
        }

        // Offload sandbox slot cleanup: decrement slotsUsed on the parent ClawPairing
        if let Some(requester) = sandbox
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("azureclaw.azure.com/offload-requester"))
        {
            crate::pairing::release_offload_slot(client.clone(), requester, &name).await;
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
    let has_finalizer = sandbox
        .metadata
        .finalizers
        .as_ref()
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
    let sandbox_self_ns = sandbox.namespace().unwrap_or_default();
    // S10.A2: runtime dispatch flows through `reconciler::runtime` —
    // a `RuntimeDeploymentPlan` flattens the `spec.runtime` discriminated
    // union to the concrete image / command / args / runtime-specific env
    // that the deployment builder consumes. Adding a new runtime kind
    // means adding a producer in `runtime.rs`, not editing this fn.
    //
    // Behavior is strictly equivalent to S10.A1 for OpenClaw; non-OpenClaw
    // kinds short-circuit through the same `AdapterMissing` path
    // (refusing to silently run the OpenClaw image — see plan §S10.A1
    // rubber-duck #2 + audit doc 2026-04-28-phase2-multi-runtime-crd.md).
    let runtime_spec = spec.runtime.clone();
    let runtime_kind_str = crate::reconciler::runtime::kind_str(&runtime_spec.kind);
    let runtime_plan =
        match crate::reconciler::runtime::build_runtime_plan(&runtime_spec, &ctx.sandbox_image) {
            Ok(plan) => {
                // Defensive: producer must agree with the dispatcher on the
                // wire-format kind string — they read the same enum but via
                // different paths. If they ever drift, that's a bug we want
                // to surface in tests, not in production status patches.
                debug_assert_eq!(plan.kind_str, runtime_kind_str);
                plan
            }
            Err(crate::reconciler::runtime::RuntimePlanError::AdapterMissing(kind)) => {
                let msg = format!(
                    "spec.runtime.kind=`{kind}` has no adapter wired in this controller \
                 build (S10.A1+A2 spine); skipping Deployment to avoid silently running \
                 the OpenClaw image. Track adapter rollout: BYO=S10.A2.b, \
                 OpenAIAgents=S10.A3 (wired), MicrosoftAgentFramework=S10.A4; \
                 SemanticKernel/LangGraph/Anthropic are Tier-2 placeholders pending roadmap"
                );
                tracing::warn!(sandbox = %name, runtime = %kind, "{msg}");
                crate::status::stamp_runtime_unsupported(client, &sandbox, &name, kind, &msg).await;
                return Ok(Action::requeue(Duration::from_secs(300)));
            }
            Err(crate::reconciler::runtime::RuntimePlanError::ShapeInvalid(msg)) => {
                tracing::error!(sandbox = %name, "runtime spec shape invalid: {msg}");
                crate::status::stamp_degraded(
                    client,
                    &sandbox,
                    &name,
                    crate::status::conditions::reason::SPEC_INVALID,
                    &msg,
                )
                .await;
                return Ok(Action::requeue(Duration::from_secs(300)));
            }
        };
    let agent_config = spec.agent.unwrap_or_default();

    // ── Phase 3 S8: BYO contract validation ──────────────────────────────
    // For BYO sandboxes, validate `spec.runtime.byo` against the
    // contract documented in `docs/byo-runtime-contract.md`. Behaviour
    // depends on the controller-level `BYO_STRICT_MODE` flag:
    //
    //   * loose (default): warn-only, a `RuntimeReady` condition is
    //     stamped with reason `BYOContractAdvisory` and reconciliation
    //     proceeds.
    //   * strict: any violation degrades the CR with reason
    //     `BYOContractInvalid` and the Deployment is NOT created.
    if let Some(byo_cfg) = runtime_spec.byo.as_ref() {
        let issues = byo_contract::validate(byo_cfg, ctx.byo_strict);
        if !issues.is_empty() {
            let summary = issues
                .iter()
                .map(|i| format!("{}: {}", i.field, i.message))
                .collect::<Vec<_>>()
                .join("; ");
            match byo_contract::worst_severity(&issues) {
                Some(byo_contract::Severity::Strict) => {
                    tracing::error!(
                        sandbox = %name,
                        "BYO strict-mode rejection: {summary}"
                    );
                    crate::status::stamp_degraded(
                        client,
                        &sandbox,
                        &name,
                        "BYOContractInvalid",
                        &summary,
                    )
                    .await;
                    return Ok(Action::requeue(Duration::from_secs(300)));
                }
                Some(byo_contract::Severity::Warn) | None => {
                    tracing::warn!(
                        sandbox = %name,
                        "BYO contract advisory (warn-only): {summary}"
                    );
                }
            }
        }
    }

    // ── Validate CRD inputs ──────────────────────────────────────────────
    let isolation = &sandbox_config.isolation;
    use crate::status::conditions::reason::{DEPENDENCY_MISSING, SPEC_INVALID};
    // Stamp Degraded + requeue-60s. Shared by all validation-failure exits.
    macro_rules! degrade {
        ($reason:expr, $msg:expr) => {{
            crate::status::stamp_degraded(client, &sandbox, &name, $reason, &$msg).await;
            return Ok(Action::requeue(Duration::from_secs(60)));
        }};
    }
    if !["standard", "enhanced", "confidential"].contains(&isolation.as_str()) {
        tracing::error!("Invalid isolation level: {isolation}");
        degrade!(
            SPEC_INVALID,
            format!("invalid sandbox.isolation: {isolation}")
        );
    }

    // ── S13: resolve the sandbox's InferencePolicy ref ───────────────────
    // ClawSandbox.spec.inferenceRef is the single source of truth for
    // inference guardrails (model preference, content-safety floor,
    // prompt-shield requirement, token budgets). The reconciler resolves
    // the ref against the sandbox's *own* namespace; cross-namespace
    // refs are deliberately not supported (privilege-escalation vector
    // — see docs/crd-precedence.md). Missing ref target → Degraded with
    // reason `InferencePolicyNotFound`; we do not fall through with
    // defaults.
    let inference_ref_name = spec.inference_ref.name.clone();
    if inference_ref_name.is_empty() {
        tracing::error!(sandbox = %name, "spec.inferenceRef.name is empty");
        degrade!(SPEC_INVALID, "spec.inferenceRef.name is required");
    }
    let ip_api: Api<crate::inference_policy::InferencePolicy> =
        Api::namespaced(client.clone(), &sandbox_self_ns);
    let inference_policy = match ip_api.get(&inference_ref_name).await {
        Ok(ip) => ip,
        Err(kube::Error::Api(ae)) if ae.code == 404 => {
            tracing::error!(
                sandbox = %name,
                ref = %inference_ref_name,
                ns = %sandbox_self_ns,
                "InferencePolicy not found in sandbox namespace",
            );
            degrade!(
                crate::status::conditions::reason::INFERENCE_POLICY_NOT_FOUND,
                format!(
                    "InferencePolicy `{inference_ref_name}` not found in namespace \
                     `{sandbox_self_ns}` (cross-namespace refs not supported)"
                )
            );
        }
        Err(e) => {
            tracing::error!(error = %e, sandbox = %name, "InferencePolicy lookup failed");
            return Ok(Action::requeue(Duration::from_secs(15)));
        }
    };
    let inference_policy_spec = inference_policy.spec.clone();
    // Model is required: it's plumbed into AZURE_OPENAI_DEPLOYMENT and (for
    // OpenClaw) OPENCLAW_MODEL. Without it the agent container has no
    // Foundry deployment to call.
    let inference_model = inference_policy_spec
        .model_preference
        .as_ref()
        .map(|mp| mp.primary.deployment.clone())
        .unwrap_or_default();
    if inference_model.is_empty() {
        tracing::error!(
            sandbox = %name,
            ref = %inference_ref_name,
            "InferencePolicy.modelPreference.primary.deployment is empty",
        );
        degrade!(
            SPEC_INVALID,
            format!(
                "InferencePolicy `{inference_ref_name}` is missing \
                 spec.modelPreference.primary.deployment"
            )
        );
    }
    // Prompt Shields default-on; opt-out via require_prompt_shields=false.
    let prompt_shields_enabled = inference_policy_spec
        .content_safety
        .as_ref()
        .and_then(|c| c.require_prompt_shields)
        .unwrap_or(true);
    // Content Safety always enabled at the router boundary; the policy
    // CR's severity floors tighten the router's defaults but do not
    // disable the feature.
    let content_safety_enabled = true;
    let token_budget_daily = inference_policy_spec
        .token_budget
        .as_ref()
        .and_then(|b| b.daily_tokens)
        .unwrap_or(0) as i64;
    let token_budget_per_request = inference_policy_spec
        .token_budget
        .as_ref()
        .and_then(|b| b.per_request_tokens)
        .unwrap_or(0) as i64;

    if ctx.foundry_endpoint.is_empty() && ctx.openai_endpoint.is_empty() {
        tracing::error!("No inference endpoint configured");
        degrade!(
            DEPENDENCY_MISSING,
            "no inference endpoint configured (FOUNDRY_ENDPOINT or AZURE_OPENAI_ENDPOINT)"
        );
    }

    // ── Overlay-mode pre-flight (Phase 2 S8) ─────────────────────────────
    // When `spec.upstreamCompatibility.sigsAgentSandbox == "overlay"`,
    // an upstream `Sandbox` CR (sigs.k8s.io/agent-sandbox) owns the Pod.
    // We still create the overlay (namespace, SA, NetworkPolicy,
    // governance ConfigMaps) but skip Deployment/Service/CronJob.
    //
    // Field-level invariant (no ClawSandbox CEL today): overlay mode
    // requires `upstreamSandboxRef.name`. Without it we can't surface
    // *which* upstream CR the operator meant, so we Degrade rather than
    // silently no-op.
    let upstream_compat = spec.upstream_compatibility.clone().unwrap_or_default();
    let overlay_mode = upstream_compat.is_overlay_mode();
    let overlay_target: Option<String> = if overlay_mode {
        match upstream_compat.overlay_target_name() {
            Some(n) if !n.is_empty() => Some(n.to_owned()),
            _ => {
                tracing::error!(
                    sandbox = %name,
                    "OverlayMode selected but upstreamSandboxRef.name is missing/empty"
                );
                degrade!(
                    SPEC_INVALID,
                    "upstreamCompatibility.sigsAgentSandbox=\"overlay\" requires \
                     upstreamCompatibility.upstreamSandboxRef.name"
                );
            }
        }
    } else {
        // Reject unknown values eagerly so misspellings ("Overlay",
        // "overaly") don't silently fall through to Native mode.
        if let Some(v) = upstream_compat.sigs_agent_sandbox.as_deref()
            && !matches!(v, "" | "off" | "observe" | "translate" | "overlay")
        {
            tracing::error!(sandbox = %name, value = v,
                "Unknown upstreamCompatibility.sigsAgentSandbox value");
            degrade!(
                SPEC_INVALID,
                format!(
                    "upstreamCompatibility.sigsAgentSandbox: unknown value `{v}` \
                     (expected off|observe|translate|overlay)"
                )
            );
        }
        None
    };
    if overlay_mode {
        tracing::info!(
            sandbox = %name,
            upstream = %overlay_target.as_deref().unwrap_or("?"),
            "OverlayMode active — skipping Deployment/Service/blocklist-CronJob \
             (upstream Sandbox CR owns the Pod)"
        );
    }

    // Hoisted out of the deployment block so Step 4c (governance
    // ConfigMap) and the blocklist CronJob (Step 4d) can reach them.
    // In overlay mode the Pod is upstream-owned, but the governance
    // overlay still relies on these.
    let governance_config = spec.governance.clone().unwrap_or_default();
    let blocklist_cm_name = format!("{}-blocklist", &name);

    // ── S13: resolve the sandbox's ToolPolicy ref (if governance enabled) ─
    // Governance == off ⇒ no ref required. Governance == on ⇒ the ref
    // is authoritative; missing target → Degraded with reason
    // `ToolPolicyNotFound`. The resolved CR's `metadata.name` doubles as
    // the AGT policy profile name carried into the sandbox.
    let tool_policy_profile: String = if governance_config.enabled {
        let tp_ref_name = governance_config.tool_policy_ref.name.clone();
        if tp_ref_name.is_empty() {
            tracing::error!(sandbox = %name, "spec.governance.toolPolicyRef.name is empty");
            degrade!(
                SPEC_INVALID,
                "spec.governance.toolPolicyRef.name is required when governance.enabled=true"
            );
        }
        let tp_api: Api<crate::tool_policy::ToolPolicy> =
            Api::namespaced(client.clone(), &sandbox_self_ns);
        match tp_api.get(&tp_ref_name).await {
            Ok(_) => tp_ref_name,
            Err(kube::Error::Api(ae)) if ae.code == 404 => {
                tracing::error!(
                    sandbox = %name,
                    ref = %tp_ref_name,
                    ns = %sandbox_self_ns,
                    "ToolPolicy not found in sandbox namespace",
                );
                degrade!(
                    crate::status::conditions::reason::TOOL_POLICY_NOT_FOUND,
                    format!(
                        "ToolPolicy `{tp_ref_name}` not found in namespace \
                         `{sandbox_self_ns}` (cross-namespace refs not supported)"
                    )
                );
            }
            Err(e) => {
                tracing::error!(error = %e, sandbox = %name, "ToolPolicy lookup failed");
                return Ok(Action::requeue(Duration::from_secs(15)));
            }
        }
    } else {
        // governance off ⇒ no profile is shipped. The reconciler still
        // needs *some* string for the ConfigMap name when it's referenced;
        // it never is in this branch (all `tool_policy_profile` reads are
        // gated by `governance_config.enabled`).
        String::new()
    };

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
                "azureclaw.azure.com/isolated": "strict",
                "pod-security.kubernetes.io/enforce": "privileged",
                "pod-security.kubernetes.io/audit": "baseline",
                "pod-security.kubernetes.io/warn": "baseline"
            }
        }
    }))?;
    ns_api
        .patch(
            &sandbox_ns,
            &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
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
            &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
            &Patch::Apply(sa),
        )
        .await?;

    // ── Step 2b: Create Azure federated identity credential ──────────────
    // Maps system:serviceaccount:{namespace}:sandbox → managed identity so
    // Workload Identity token exchange works for this sub-agent.
    if let Some(ref fedcred) = ctx.fedcred
        && let Err(e) = fedcred
            .ensure_federated_credential(&name, &sandbox_ns)
            .await
    {
        tracing::warn!(sandbox = %name, "Federated credential creation failed (non-fatal): {e}");
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
            &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
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
            h.write_u64(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64,
            );
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
            &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
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
            h.write_u64(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64,
            );
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
            &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
            &Patch::Apply(admin_secret),
        )
        .await?;

    // ── Step 3: Apply default-deny NetworkPolicy + allowlist ─────────────
    //
    // Pod-level NetworkPolicy controls what the entire pod can reach.
    // Per-container egress restriction (agent vs. inference-router) is enforced
    // by the iptables init container below (UID-based rules), since K8s
    // NetworkPolicy has no per-container granularity.
    //
    // S12.e: the user-defined endpoints are resolved here via
    // [`crate::policy_fetcher::resolve_allowlist`]. When
    // `spec.networkPolicy.allowlistRef` is set, the controller derives
    // endpoints from the verified canonical artifact (or the LKG cache
    // on verify failure). When the ref is unset, the legacy inline
    // path is used. The resolution carries the
    // `AllowlistVerified` / `AllowlistAuthoritative` / `AllowlistDrift`
    // conditions which are merged into the running-status patch below.
    let allowlist_resolution = crate::policy_fetcher::resolve_allowlist(&sandbox).await;
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
        // it can only reach localhost:8443 (inference-router) and DNS.
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
    // on behalf of the agent — agent itself can only reach localhost).
    // S12.e fail-closed: when `endpoints == None` (verify failed and no
    // LKG), no user-defined egress rules are added — the sandbox is
    // restricted to the always-allowed defaults above.
    if let Some(endpoints) = allowlist_resolution.endpoints.as_ref() {
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
            &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
            &Patch::Apply(netpol),
        )
        .await?;

    // S12.e: if the allowlist resolved to fail-closed-no-LKG (verify
    // failed on first reconcile / after controller restart, no
    // last-known-good cached), stamp Degraded with FailedClosed and
    // requeue. The NetworkPolicy above carries only the always-allowed
    // baseline rules; user-defined egress is denied. We deliberately
    // do NOT proceed to deploy the sandbox pod — running an agent
    // whose egress policy cannot be authoritatively determined is a
    // failure mode that should be visible to operators, not papered
    // over.
    if allowlist_resolution.fail_closed_no_lkg {
        tracing::warn!(
            sandbox = %name,
            "AllowlistAuthoritative=False/FailedClosed: verify failed and no last-known-good; \
             refusing to deploy sandbox pod with broad egress"
        );
        let sandbox_api: Api<ClawSandbox> =
            Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
        let mut status_obj = crate::status::build_degraded_status_patch(
            &sandbox,
            crate::status::conditions::reason::FAILED_CLOSED,
            "egress allowlist verify failed and no last-known-good cached; \
             refusing to broaden egress (fail-closed)",
        );
        // Preserve the resolution's three conditions so operators see
        // the verify-fail reason alongside the Degraded marker.
        if let Some(arr) = status_obj["status"]["conditions"].as_array_mut() {
            for c in &allowlist_resolution.conditions {
                let v = serde_json::to_value(c).unwrap_or(serde_json::Value::Null);
                if let Some(t) = v.get("type").and_then(|t| t.as_str()) {
                    arr.retain(|e| e.get("type").and_then(|x| x.as_str()) != Some(t));
                }
                arr.push(v);
            }
        }
        let _ = sandbox_api
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(status_obj))
            .await;
        return Ok(Action::requeue(crate::backoff::requeue_secs_with_jitter(
            60,
        )));
    }

    // ── Step 4: Deploy sandbox pod ───────────────────────────────────────
    // Skipped wholesale in OverlayMode (Phase 2 S8): the operator's
    // upstream `Sandbox` CR (sigs.k8s.io/agent-sandbox) owns the Pod
    // lifecycle. Step 4b (SA annotations for Azure RBAC) and Step 4c
    // (governance ConfigMap + mesh ingress NetworkPolicy) intentionally
    // still run — they form the *overlay* that AzureClaw layers on top
    // of the upstream Pod.
    'deployment_block: {
        if overlay_mode {
            break 'deployment_block;
        }
        // Phase G P1 #4: spec.suspended scales the Deployment to 0
        // replicas without removing it, preserving cluster state for
        // a graceful resume. We still walk the rest of this block so
        // image / env / volume drift is reflected on the suspended
        // Deployment (so resume picks up the latest spec).
        let suspended_by_spec = spec.suspended.unwrap_or(false);
        let desired_replicas: i64 = if suspended_by_spec { 0 } else { 1 };

        // S10.A2: image now comes from the runtime plan (already
        // resolved against the controller default fallback). The
        // deployment builder must not re-derive the image from
        // `openclaw_config` — see plan §S10.A1 rubber-duck #2.
        let image = runtime_plan.image.clone();

        let (runtime_class, pool_label) = isolation_scheduling(&sandbox_config.isolation);

        let pull_policy = if image.ends_with(":latest") {
            "Always"
        } else {
            "IfNotPresent"
        };

        let deploy_api: Api<Deployment> = Api::namespaced(client.clone(), &sandbox_ns);

        // Token budget values resolved from the InferencePolicy ref above
        // (hoisted to the top of `reconcile` after S13). 0 = unlimited.

        // S10.A2.b / S10.A3: OpenClaw vs non-OpenClaw branch the *agent
        // container shape*. The router sidecar, init container,
        // NetworkPolicy, SA, seccomp, volumes, and security context are
        // runtime-agnostic. Only the agent container itself differs.
        // BYO (S10.A2.b) and OpenAIAgents (S10.A3) both follow the
        // generic-runtime shape (different container name, no
        // OpenClaw-specific env, no admin-token mount).
        let is_openclaw = matches!(runtime_spec.kind, crate::crd::RuntimeKind::OpenClaw);

        // Build OpenClaw container env vars.
        //
        // OPENCLAW_GATEWAY_TOKEN is plumbed via secretKeyRef rather than a static
        // value so that:
        //   1. Rotating the Secret + restarting the pod reliably picks up the new
        //      token without requiring a controller reconcile to re-render the
        //      Deployment env.
        //   2. The pod env is, by construction, equal to the Secret value at pod
        //      start. `azureclaw connect` reads the Secret to find the gateway
        //      token; if env and Secret ever drift, the operator gets 401s on a
        //      rolled pod even though the Secret looks correct. valueFrom closes
        //      that drift window.
        //
        // For BYO, OPENCLAW_* envs are skipped: the gateway-token Secret is
        // OpenClaw-specific (azureclaw up provisions it for OpenClaw only),
        // and a BYO pod referencing it without `optional: true` would
        // ImagePullBackOff-style fail to start.
        let mut openclaw_env: Vec<serde_json::Value> = Vec::new();
        if is_openclaw {
            openclaw_env.push(json!({"name": "OPENCLAW_MODEL", "value": inference_model.clone()}));
        }
        openclaw_env.push(json!({"name": "AZURE_OPENAI_ENDPOINT", "value": &ctx.openai_endpoint}));
        openclaw_env.push(json!({"name": "AZURECLAW_AUTH_MODE", "value": "workload-identity"}));
        if is_openclaw {
            openclaw_env.push(json!({
                "name": "OPENCLAW_GATEWAY_TOKEN",
                "valueFrom": {
                    "secretKeyRef": {
                        "name": "gateway-token",
                        "key": "token"
                    }
                }
            }));
        }
        // Foundry project endpoint (for standalone APIs: Memory Store, Foundry IQ, etc.)
        if !ctx.foundry_project_endpoint.is_empty() {
            openclaw_env.push(
                json!({"name": "FOUNDRY_PROJECT_ENDPOINT", "value": &ctx.foundry_project_endpoint}),
            );
        }
        // Foundry deployments list (so plugin shows only deployed models, not full catalog).
        // BYO agents bring their own Foundry client (or none); skipped to avoid leaking
        // the deployment list into a runtime that doesn't need it.
        if is_openclaw && !ctx.foundry_deployments.is_empty() {
            openclaw_env
                .push(json!({"name": "FOUNDRY_DEPLOYMENTS", "value": &ctx.foundry_deployments}));
        }
        // Inject Foundry Agent ID if set in status (for tools needing agent runs).
        // BYO doesn't go through the OpenClaw plugin path; skipped.
        if is_openclaw
            && let Some(ref agent_id) = sandbox
                .status
                .as_ref()
                .and_then(|s| s.foundry_agent_id.clone())
            && !agent_id.is_empty()
        {
            openclaw_env.push(json!({"name": "FOUNDRY_AGENT_ID", "value": agent_id}));
        }
        // Signal configured Foundry agent tools (OpenClaw plugin reads this).
        if is_openclaw
            && let Some(ref tools) = agent_config.tools
            && !tools.is_empty()
        {
            openclaw_env.push(json!({"name": "FOUNDRY_AGENT_TOOLS", "value": tools.join(",")}));
        }
        // AGT governance env vars (opt-in) — injected into BOTH openclaw and router containers
        // (`governance_config` is hoisted above the deployment block so Step 4c can use it.)
        let mut router_agt_env: Vec<serde_json::Value> = Vec::new();

        // Operator-level Entra-auth kill switch.
        //
        // When the cluster operator sets `AZURECLAW_DISABLE_ENTRA_AUTH=1` on
        // the controller (e.g. dev clusters, or any subscription where the
        // `api://agentmesh` Entra app registration is not yet provisioned),
        // tell every sandbox to skip the Entra token-exchange step at startup.
        //
        // Without this, sub-agents burn ~123s on doomed AAD retries before
        // falling back to anonymous tier — long enough that parent→sub-agent
        // spawn-and-message workflows fail because the parent's tool-call
        // timeout fires before the sub-agent finishes booting. See
        // docs/security-audits/2026-04-26-entra-auth-toggle.md for the full
        // analysis.
        //
        // Default is "skip" until the operator explicitly opts in by unsetting
        // the env var or setting it to "0". Phase 2 will replace this with
        // controller-side tenant feature detection once Entra Agent ID
        // provisioning is automated.
        let skip_entra =
            std::env::var("AZURECLAW_DISABLE_ENTRA_AUTH").unwrap_or_else(|_| "1".to_string());
        if skip_entra == "1" || skip_entra.eq_ignore_ascii_case("true") {
            openclaw_env.push(json!({"name": "AGT_SKIP_ENTRA", "value": "1"}));
        }

        if governance_config.enabled {
            openclaw_env.push(json!({"name": "AGT_GOVERNANCE_ENABLED", "value": "true"}));
            openclaw_env
                .push(json!({"name": "AGT_POLICY_PROFILE", "value": tool_policy_profile.clone()}));
            openclaw_env.push(json!({"name": "AGT_TRUST_THRESHOLD", "value": governance_config.trust_threshold.to_string()}));
            // Validate and propagate trusted peers (format: "name:AMID,name:AMID,...")
            let valid_peers = governance_config.trusted_peers.as_deref().filter(|p| {
                let ok = !p.contains('\n') && !p.contains('\r') && !p.contains('\0');
                if !ok {
                    tracing::warn!("Ignoring trusted_peers: contains control characters");
                }
                ok
            });
            if let Some(peers) = valid_peers {
                openclaw_env.push(json!({"name": "AGT_TRUSTED_PEERS", "value": peers}));
            }
            // Validate registry mode: must be "local" or "global"
            let reg_mode = match governance_config.registry_mode.as_deref() {
                Some("local") => "local",
                Some("global") | None => "global",
                Some(other) => {
                    tracing::warn!(mode = other, "Invalid registry_mode, defaulting to global");
                    "global"
                }
            };
            openclaw_env.push(json!({"name": "AGT_REGISTRY_MODE", "value": reg_mode}));
            // Plugin also needs relay/registry URLs for direct AgentMesh SDK connections
            openclaw_env.push(json!({"name": "AGT_RELAY_URL", "value": "ws://agentmesh-relay.agentmesh.svc.cluster.local:8765"}));
            openclaw_env.push(json!({"name": "AGT_REGISTRY_URL", "value": "http://agentmesh-registry.agentmesh.svc.cluster.local:8080"}));
            // Router needs governance vars too (handoff auth, policy enforcement)
            router_agt_env.push(json!({"name": "AGT_GOVERNANCE_ENABLED", "value": "true"}));
            router_agt_env
                .push(json!({"name": "AGT_POLICY_PROFILE", "value": &tool_policy_profile}));
            router_agt_env.push(json!({"name": "AGT_TRUST_THRESHOLD", "value": governance_config.trust_threshold.to_string()}));
            // Behavior-monitor burst threshold: offload workers run long
            // research loops that make many tool/inference calls in short
            // bursts. Bump the burst limit well above the interactive default
            // (100/60s) so legitimate research bursts aren't flagged as
            // "abuse" by the self-burst detector. Non-offload profiles keep
            // the router's built-in default.
            if tool_policy_profile == "offload" {
                router_agt_env
                    .push(json!({"name": "AGT_BEHAVIOR_BURST_THRESHOLD", "value": "1000"}));
            }
            if let Some(peers) = valid_peers {
                router_agt_env.push(json!({"name": "AGT_TRUSTED_PEERS", "value": peers}));
            }
            router_agt_env.push(json!({"name": "AGT_REGISTRY_MODE", "value": reg_mode}));
            // Mesh namespace for K8s DNS routing between agents
            router_agt_env.push(json!({"name": "AGT_MESH_NAMESPACE", "value": &sandbox_ns}));
            // Self-hosted AGT relay + registry (for E2E encrypted inter-agent comms)
            router_agt_env.push(json!({"name": "AGT_RELAY_URL", "value": "ws://agentmesh-relay.agentmesh.svc.cluster.local:8765"}));
            router_agt_env.push(json!({"name": "AGT_REGISTRY_URL", "value": "http://agentmesh-registry.agentmesh.svc.cluster.local:8080"}));
        }

        // ── extraEnv: runtime-specific env vars from the runtime plan ──
        // For OpenClaw this comes from `spec.runtime.openclaw.extraEnv`
        // and is used by the controller to propagate offload parameters
        // (OFFLOAD_REQUEST_ID, OFFLOAD_PARENT_AMID, OFFLOAD_TASK,
        // OFFLOAD_TIMEOUT_MINUTES) into offload sandboxes. For other
        // runtime kinds (S10.A2.b BYO, A3 OpenAIAgents, A4 MAF) the same
        // map is sourced from the matching variant struct via
        // `runtime::build_runtime_plan`. Keys are validated against
        // reserved prefixes regardless of source.
        if !runtime_plan.runtime_extra_env.is_empty() {
            // Reserved prefixes that must come from the reconciler itself, not user input.
            const RESERVED_PREFIXES: &[&str] =
                &["AGT_", "FOUNDRY_AGENT_", "AZURE_", "IMDS_", "AZURECLAW_"];
            // Names already set above — skip silently if caller provided a duplicate.
            let mut existing: std::collections::HashSet<String> = openclaw_env
                .iter()
                .filter_map(|v| v.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect();
            for (k, v) in &runtime_plan.runtime_extra_env {
                if k.is_empty()
                    || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    || k.chars().next().is_some_and(|c| c.is_ascii_digit())
                {
                    tracing::warn!(key = %k, "extraEnv: invalid env var name, skipping");
                    continue;
                }
                if RESERVED_PREFIXES.iter().any(|p| k.starts_with(p)) {
                    tracing::warn!(key = %k, "extraEnv: key uses reserved prefix, skipping");
                    continue;
                }
                if v.contains('\0') {
                    tracing::warn!(key = %k, "extraEnv: value contains NUL byte, skipping");
                    continue;
                }
                if existing.contains(k) {
                    tracing::debug!(key = %k, "extraEnv: overridden by reconciler, skipping");
                    continue;
                }
                openclaw_env.push(json!({"name": k, "value": v}));
                existing.insert(k.clone());
            }
        }

        // S10.A2.b: append `plan.raw_env` entries (BYO `valueFrom` etc.).
        // The producer guarantees these have a `name` field; we apply the
        // same reserved-prefix / NUL / dup filter to the `name` only —
        // the `valueFrom` payload itself is rendered verbatim.
        if !runtime_plan.raw_env.is_empty() {
            const RESERVED_PREFIXES: &[&str] =
                &["AGT_", "FOUNDRY_AGENT_", "AZURE_", "IMDS_", "AZURECLAW_"];
            let mut existing: std::collections::HashSet<String> = openclaw_env
                .iter()
                .filter_map(|v| v.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect();
            for entry in &runtime_plan.raw_env {
                let Some(name) = entry.get("name").and_then(|n| n.as_str()) else {
                    tracing::warn!("rawEnv: entry missing `name`, skipping");
                    continue;
                };
                if name.is_empty()
                    || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    || name.chars().next().is_some_and(|c| c.is_ascii_digit())
                {
                    tracing::warn!(key = %name, "rawEnv: invalid env var name, skipping");
                    continue;
                }
                if RESERVED_PREFIXES.iter().any(|p| name.starts_with(p)) {
                    tracing::warn!(key = %name, "rawEnv: key uses reserved prefix, skipping");
                    continue;
                }
                if existing.contains(name) {
                    tracing::debug!(key = %name, "rawEnv: overridden by reconciler, skipping");
                    continue;
                }
                openclaw_env.push(entry.clone());
                existing.insert(name.to_string());
            }
        }

        // Build the inference-router env array
        let mut router_env = vec![
            json!({"name": "AZURE_OPENAI_ENDPOINT", "value": &ctx.openai_endpoint}),
            json!({"name": "FOUNDRY_ENDPOINT", "value": &ctx.foundry_endpoint}),
            json!({"name": "FOUNDRY_PROJECT_ENDPOINT", "value": &ctx.foundry_project_endpoint}),
            json!({"name": "IMDS_CLIENT_ID", "value": &ctx.imds_client_id}),
            json!({"name": "AZURE_OPENAI_DEPLOYMENT", "value": &inference_model}),
            json!({"name": "AZURECLAW_AUTH_MODE", "value": "workload-identity"}),
            json!({"name": "CONTENT_SAFETY_ENABLED", "value": content_safety_enabled.to_string()}),
            json!({"name": "PROMPT_SHIELDS_ENABLED", "value": prompt_shields_enabled.to_string()}),
            json!({"name": "CONTENT_SAFETY_ENDPOINT", "value": &ctx.content_safety_endpoint}),
            json!({"name": "TOKEN_BUDGET_DAILY", "value": token_budget_daily.to_string()}),
            json!({"name": "TOKEN_BUDGET_PER_REQUEST", "value": token_budget_per_request.to_string()}),
            json!({"name": "SANDBOX_NAME", "value": &name}),
            json!({"name": "SANDBOX_ISOLATION", "value": &sandbox_config.isolation}),
            json!({"name": "RUST_LOG", "value": "info,inference_router=debug"}),
        ];
        router_env.extend(router_agt_env);

        // ── Blocklist ConfigMap + env vars ──
        // The blocklist is always-on: router loads a seed file at startup, then
        // auto-refreshes from OISD + URLhaus feeds every 6 hours.
        // (`blocklist_cm_name` is hoisted above the deployment block so the
        // CronJob (Step 4d) can reach it in overlay mode.)
        router_env.push(json!({"name": "BLOCKLIST_ENABLED", "value": "true"}));
        router_env.push(
            json!({"name": "BLOCKLIST_SEED_PATH", "value": "/etc/azureclaw/blocklist/domains.txt"}),
        );

        // Egress learn mode — enabled by default so operators can discover required domains.
        // Blocklist (threat intelligence) is still enforced. Disable with network_policy.learn_egress=false.
        let learn_egress = spec
            .network_policy
            .as_ref()
            .is_none_or(|np| np.learn_egress);
        if learn_egress {
            router_env.push(json!({"name": "EGRESS_LEARN_MODE", "value": "true"}));
        }

        // ── Agent container ──────────────────────────────────────────
        // S10.A2.b: branch the agent container shape on the runtime
        // kind. OpenClaw container = "openclaw" with gateway port 18789
        // + admin-token mount (plugin pushes trust to router after
        // KNOCK). BYO container = "agent" with no port, no admin-token
        // mount, command/args from the runtime plan. Everything else
        // (env, security context, volumes, probes, resources) is
        // identical across runtimes — they're platform contract,
        // controller-enforced.
        let agent_container_name = if is_openclaw { "openclaw" } else { "agent" };
        let agent_resources = spec
            .resources
            .as_ref()
            .map(|r| {
                json!({
                    "requests": r.requests,
                    "limits": r.limits,
                })
            })
            .unwrap_or(json!({
                "requests": {"cpu": "500m", "memory": "1Gi"},
                "limits": {"cpu": "2", "memory": "4Gi"},
            }));
        let mut agent_volume_mounts = vec![
            json!({"name": "sandbox-data", "mountPath": "/sandbox"}),
            json!({"name": "tmp", "mountPath": "/tmp"}),
        ];
        if is_openclaw {
            // OpenClaw plugin needs admin token to authenticate trust
            // mutations after KNOCK handshakes (pushTrustToRouter).
            // BYO does not run the plugin — mount is omitted to keep the
            // attack surface narrow (defense-in-depth § the principle of
            // least privilege).
            agent_volume_mounts.push(json!({
                "name": "admin-token",
                "mountPath": "/etc/azureclaw/secrets",
                "readOnly": true,
            }));
        }
        let mut agent_container = json!({
            "name": agent_container_name,
            "image": image,
            "imagePullPolicy": pull_policy,
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
            "volumeMounts": agent_volume_mounts,
            "resources": agent_resources,
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
        });
        if is_openclaw {
            // OpenClaw gateway port (used by `azureclaw connect` port-forward).
            agent_container["ports"] = json!([{"containerPort": 18789, "name": "gateway"}]);
        }
        if let Some(cmd) = &runtime_plan.command {
            agent_container["command"] = json!(cmd);
        }
        if let Some(args) = &runtime_plan.args {
            agent_container["args"] = json!(args);
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
            // The agent can only reach the inference-router on localhost:8443.
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
                    // forward proxy (port 8444) in the inference-router. This
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
                            agent_container,
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
                                },
                                "volumeMounts": [
                                    {"name": "admin-token", "mountPath": "/etc/azureclaw/secrets", "readOnly": true}
                                ]
                            }
                        ],
                        "volumes": [
                            {"name": "sandbox-data", "emptyDir": {}},
                            {"name": "tmp", "emptyDir": {"medium": "Memory", "sizeLimit": "1Gi"}},
                            {"name": "admin-token", "secret": {"secretName": "router-admin-token", "items": [{"key": "token", "path": "admin-token"}]}}
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
            pod_spec
                .as_object_mut()
                .unwrap()
                .insert("runtimeClassName".into(), json!(rc));
        }

        // S7 wiring — mirror governance CRD ConfigMaps/Secrets from the
        // user namespace into the sandbox namespace, then inject mounts
        // into the inference-router container.
        //
        // ToolPolicy (always, when governance enabled):
        //   - Source: `toolpolicy-{tp_ref_name}-profile` ConfigMap
        //     (key `profile.json`) in `sandbox_self_ns`, owned by the
        //     ToolPolicy reconciler.
        //   - Destination: same name in `sandbox_ns`.
        //   - Mount: `/etc/agt/policies` + env `AGT_POLICY_DIR`.
        //   - Failure: source missing → mount omitted (router falls back
        //     to empty policy engine, fail-closed at the AGT layer).
        if governance_config.enabled && !tool_policy_profile.is_empty() {
            let cm_name = format!("toolpolicy-{}-profile", &tool_policy_profile);
            match governance_mounts::mirror_configmap(
                client,
                &cm_name,
                &sandbox_self_ns,
                &sandbox_ns,
                &name,
                "ToolPolicy",
            )
            .await
            {
                Ok(governance_mounts::MirrorOutcome::Mirrored) => {
                    governance_mounts::inject_configmap_mount(
                        &mut pod_spec,
                        "inference-router",
                        &cm_name,
                        "agt-policy",
                        governance_mounts::paths::TOOL_POLICY_DIR,
                        Some(("AGT_POLICY_DIR", governance_mounts::paths::TOOL_POLICY_DIR)),
                    );
                }
                Ok(governance_mounts::MirrorOutcome::Skipped(reason)) => {
                    tracing::warn!(
                        sandbox = %name,
                        cm = %cm_name,
                        reason = %reason,
                        "ToolPolicy compiled-profile ConfigMap not mirrored; \
                         router will start with empty policy engine",
                    );
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        sandbox = %name,
                        cm = %cm_name,
                        "ToolPolicy ConfigMap mirror failed",
                    );
                    return Ok(Action::requeue(Duration::from_secs(15)));
                }
            }

            // Add writable emptyDir volume for trust store + audit log persistence
            if let Some(volumes) = pod_spec.get_mut("volumes").and_then(|v| v.as_array_mut())
                && !volumes
                    .iter()
                    .any(|v| v.get("name").and_then(|n| n.as_str()) == Some("agt-data"))
            {
                volumes.push(json!({
                    "name": "agt-data",
                    "emptyDir": {"sizeLimit": "10Mi"}
                }));
            }
        }

        // McpServer (optional): if the sandbox references one, mirror its
        // JWKS ConfigMap + signing-key Secret and mount them.
        if let Some(mcp_ref) = governance_config.mcp_server_ref.as_ref() {
            let mcp_name = mcp_ref.name.trim();
            if !mcp_name.is_empty() {
                let jwks_cm = format!("mcp-{mcp_name}-jwks");
                let signing_secret = format!("mcp-{mcp_name}-signing");
                match governance_mounts::mirror_configmap(
                    client,
                    &jwks_cm,
                    &sandbox_self_ns,
                    &sandbox_ns,
                    &name,
                    "McpServer",
                )
                .await
                {
                    Ok(governance_mounts::MirrorOutcome::Mirrored) => {
                        governance_mounts::inject_configmap_mount(
                            &mut pod_spec,
                            "inference-router",
                            &jwks_cm,
                            "mcp-jwks",
                            governance_mounts::paths::MCP_JWKS_DIR,
                            Some(("MCP_JWKS_PATH", "/etc/azureclaw/mcp/jwks.json")),
                        );
                    }
                    Ok(governance_mounts::MirrorOutcome::Skipped(reason)) => {
                        tracing::warn!(
                            sandbox = %name,
                            cm = %jwks_cm,
                            reason = %reason,
                            "McpServer JWKS ConfigMap not mirrored; \
                             router will not advertise customer MCP",
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            sandbox = %name,
                            cm = %jwks_cm,
                            "McpServer JWKS mirror failed",
                        );
                        return Ok(Action::requeue(Duration::from_secs(15)));
                    }
                }
                match governance_mounts::mirror_secret(
                    client,
                    &signing_secret,
                    &sandbox_self_ns,
                    &sandbox_ns,
                    &name,
                    "McpServer",
                )
                .await
                {
                    Ok(governance_mounts::MirrorOutcome::Mirrored) => {
                        governance_mounts::inject_secret_mount(
                            &mut pod_spec,
                            "inference-router",
                            &signing_secret,
                            "mcp-signing",
                            governance_mounts::paths::MCP_SIGNING_DIR,
                            Some((
                                "MCP_SIGNING_KEY_DIR",
                                governance_mounts::paths::MCP_SIGNING_DIR,
                            )),
                        );
                    }
                    Ok(governance_mounts::MirrorOutcome::Skipped(reason)) => {
                        tracing::warn!(
                            sandbox = %name,
                            secret = %signing_secret,
                            reason = %reason,
                            "McpServer signing-key Secret not mirrored",
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            sandbox = %name,
                            secret = %signing_secret,
                            "McpServer signing Secret mirror failed",
                        );
                        return Ok(Action::requeue(Duration::from_secs(15)));
                    }
                }
            }
        }

        // A2AAgent (optional): when A2A is enabled, mirror the signed
        // AgentCard ConfigMap so the router can serve `/.well-known/agent.json`.
        // Defaults to an A2AAgent named after the sandbox itself.
        let a2a_cfg_opt = spec.a2a.as_ref();
        if let Some(a2a_cfg) = a2a_cfg_opt
            && a2a_cfg.enabled
        {
            let agent_name = a2a_cfg
                .agent_ref
                .as_ref()
                .map(|r| r.name.as_str())
                .unwrap_or(name.as_str());
            if !agent_name.is_empty() {
                let card_cm = format!("a2aagent-{agent_name}-card");
                match governance_mounts::mirror_configmap(
                    client,
                    &card_cm,
                    &sandbox_self_ns,
                    &sandbox_ns,
                    &name,
                    "A2AAgent",
                )
                .await
                {
                    Ok(governance_mounts::MirrorOutcome::Mirrored) => {
                        governance_mounts::inject_configmap_mount(
                            &mut pod_spec,
                            "inference-router",
                            &card_cm,
                            "a2a-card",
                            governance_mounts::paths::A2A_CARD_DIR,
                            Some(("A2A_CARD_DIR", governance_mounts::paths::A2A_CARD_DIR)),
                        );
                    }
                    Ok(governance_mounts::MirrorOutcome::Skipped(reason)) => {
                        tracing::warn!(
                            sandbox = %name,
                            cm = %card_cm,
                            reason = %reason,
                            "A2AAgent card ConfigMap not mirrored; \
                             /.well-known/agent.json will 404",
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            sandbox = %name,
                            cm = %card_cm,
                            "A2AAgent card mirror failed",
                        );
                        return Ok(Action::requeue(Duration::from_secs(15)));
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
        if let Some(containers) = pod_spec
            .get_mut("containers")
            .and_then(|c| c.as_array_mut())
        {
            for container in containers.iter_mut() {
                if container.get("name").and_then(|n| n.as_str()) == Some("inference-router") {
                    let mounts = container
                        .as_object_mut()
                        .unwrap()
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
                "replicas": desired_replicas,
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
                &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
                &Patch::Apply(deployment),
            )
            .await?;
    } // end 'deployment_block

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
                &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
                &Patch::Apply(serde_json::from_value::<
                    k8s_openapi::api::core::v1::ServiceAccount,
                >(sa_patch)?),
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
                &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
                &Patch::Apply(svc),
            )
            .await?;

        // S7 wiring: the per-CR ToolPolicy compiled-profile ConfigMap is
        // owned by the ToolPolicy reconciler in the user namespace; the
        // sandbox reconciler mirrors it into the sandbox namespace
        // (above, in the pod-spec assembly block). The previous
        // implementation baked `cli/policies/azureclaw-default.yaml` /
        // `cli/policies/azureclaw-offload.yaml` into the controller
        // binary and wrote it to a static `agt-policy-{profile}`
        // ConfigMap; that path is removed because it bypassed the
        // ToolPolicy CRD entirely (changes required a controller rebuild).
        let cm_name = format!("toolpolicy-{}-profile", &tool_policy_profile);

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
                &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
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
    //
    // **OverlayMode (S8):** skipped. The blocklist is mounted into the
    // AzureClaw-managed router container; in overlay mode the upstream
    // Sandbox CR owns the Pod and there is no AzureClaw router to consume
    // the ConfigMap. Recreating it would be dead overhead (CronJob would
    // run every 6h with nothing reading the output).
    if !overlay_mode {
        let full_seed = include_str!("../../../cli/blocklists/seed-domains.txt");
        // Truncate to stay under 1MB ConfigMap limit (K8s rejects >1048576 bytes)
        let seed_domains = if full_seed.len() > 900_000 {
            // Take the first N lines that fit in 900KB
            let mut end = 900_000;
            while end > 0 && !full_seed.is_char_boundary(end) {
                end -= 1;
            }
            // Find last newline before the boundary
            if let Some(pos) = full_seed[..end].rfind('\n') {
                &full_seed[..pos]
            } else {
                &full_seed[..end]
            }
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
                &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
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
        let cj_api: Api<kube::api::DynamicObject> =
            Api::namespaced_with(client.clone(), &sandbox_ns, &cj_ar);
        let cj_obj: kube::api::DynamicObject = serde_json::from_value(cronjob)?;
        let _ = cj_api
            .patch(
                &cronjob_name,
                &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
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
    // Idempotency guard: skip the patch when the desired status already
    // matches reality. Without this, every reconcile bumps
    // `metadata.resourceVersion` (kube-apiserver bumps RV on every PATCH
    // against `.status` regardless of byte-equality), which retriggers
    // our own watch and produces a hot reconcile loop. See
    // `crate::status::running_status_matches` for the rationale.
    //
    // OverlayMode emits a distinct `phase: "Overlay"` + Suspended=True
    // condition so dashboards can surface "this CR is intentionally not
    // driving a Pod" — see [`crate::status::build_overlay_status_patch`].
    if let Some(upstream_ref) = overlay_target.as_deref() {
        if !crate::status::overlay_status_matches(
            &sandbox,
            &sandbox_ns,
            upstream_ref,
            runtime_kind_str,
        ) {
            let sandbox_api: Api<ClawSandbox> =
                Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
            let status_obj = crate::status::build_overlay_status_patch(
                &sandbox,
                &sandbox_ns,
                upstream_ref,
                runtime_kind_str,
            );
            let _ = sandbox_api
                .patch_status(&name, &PatchParams::default(), &Patch::Merge(status_obj))
                .await;
        }
    } else {
        // S12.e: surface the AllowlistVerified / AllowlistAuthoritative
        // / AllowlistDrift conditions computed earlier as part of the
        // allowlist resolution. With no `allowlistRef` set, this is a
        // single AllowlistAuthoritative=False/Inline condition (or an
        // empty list when `networkPolicy` itself is unset). The
        // fetcher itself short-circuits before any network IO.
        let mut extras: Vec<_> = allowlist_resolution.conditions.clone();

        // Phase G P1 #4: stamp Suspended condition when spec.suspended
        // is true, or clear a prior SuspendedBySpec when it is false.
        // We deliberately do NOT stamp Suspended=False on CRs that
        // were never suspended — that would add a new condition
        // retroactively to every existing sandbox.
        let suspended_by_spec = spec.suspended.unwrap_or(false);
        let prior_conditions_for_susp = sandbox
            .status
            .as_ref()
            .map(|s| s.conditions.as_slice())
            .unwrap_or(&[]);
        let prior_suspended = crate::status::conditions::find(
            prior_conditions_for_susp,
            crate::status::conditions::TYPE_SUSPENDED,
        );
        let prior_was_spec_suspended = prior_suspended
            .is_some_and(|c| c.reason == crate::status::conditions::reason::SUSPENDED_BY_SPEC);
        if suspended_by_spec {
            extras.push(crate::status::conditions::preserve_transition_time(
                prior_suspended,
                crate::status::conditions::TYPE_SUSPENDED,
                crate::status::conditions::status::TRUE,
                crate::status::conditions::reason::SUSPENDED_BY_SPEC,
                "spec.suspended=true; Deployment scaled to replicas=0",
                sandbox.metadata.generation,
            ));
        } else if prior_was_spec_suspended {
            extras.push(crate::status::conditions::preserve_transition_time(
                prior_suspended,
                crate::status::conditions::TYPE_SUSPENDED,
                crate::status::conditions::status::FALSE,
                crate::status::conditions::reason::ACTIVE,
                "spec.suspended cleared; Deployment scaled back to replicas=1",
                sandbox.metadata.generation,
            ));
        }

        if !crate::status::running_status_matches_with_extras(
            &sandbox,
            &sandbox_ns,
            runtime_kind_str,
            &extras,
        ) {
            let sandbox_api: Api<ClawSandbox> =
                Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
            let status_obj = crate::status::build_running_status_patch_with_extras(
                &sandbox,
                &sandbox_ns,
                runtime_kind_str,
                &extras,
            );
            let _ = sandbox_api
                .patch_status(&name, &PatchParams::default(), &Patch::Merge(status_obj))
                .await;
        }
    }

    tracing::info!("ClawSandbox {name} reconciled successfully");
    Ok(Action::requeue(Duration::from_secs(300)))
}

/// How long to wait before requeuing a failed reconcile, by error kind.
/// S7.D: ±20% jitter applied so retries spread across the interval and
/// don't thundering-herd the API server when many CRs hit the same
/// transient error in lockstep.
fn error_requeue_duration(error: &ReconcileError) -> Duration {
    let base = match error {
        // Transient kube API errors (throttling, connection reset, 5xx):
        // retry soon so we don't starve legitimate work.
        ReconcileError::Kube(_) => 30,
        // Serde errors are deterministic — the same body will fail again.
        // Back off longer so we don't spam logs while a human fixes the
        // bad CR.
        ReconcileError::SerdeJson(_) => 300,
    };
    crate::backoff::requeue_secs_with_jitter(base)
}

/// Error policy — what to do when reconciliation fails.
fn error_policy(sandbox: Arc<ClawSandbox>, error: &ReconcileError, _ctx: Arc<Context>) -> Action {
    let class = match error {
        ReconcileError::Kube(_) => "kube_api",
        ReconcileError::SerdeJson(_) => "serde",
    };
    crate::metrics::record_reconcile_error("ClawSandbox", class);
    tracing::error!(
        "Reconciliation error for {}: {:?}",
        sandbox.name_any(),
        error
    );
    Action::requeue(error_requeue_duration(error))
}

/// Run the controller — blocks forever, watching ClawSandbox CRDs.
pub async fn run(client: Client) -> Result<()> {
    let sandboxes: Api<ClawSandbox> = Api::all(client.clone());

    // Verify CRD is installed
    sandboxes
        .list(&ListParams::default().limit(1))
        .await
        .map_err(|e| {
            anyhow::anyhow!("ClawSandbox CRD not found — install the Helm chart first: {e}")
        })?;
    tracing::info!("ClawSandbox CRD found — starting controller");

    let wi_client_id = std::env::var("AZURE_WI_CLIENT_ID").unwrap_or_default();
    let inference_router_image = std::env::var("INFERENCE_ROUTER_IMAGE").unwrap_or_else(|_| {
        tracing::warn!("INFERENCE_ROUTER_IMAGE not set — using default :latest image");
        "azureclawacr.azurecr.io/azureclaw-inference-router:latest".into()
    });
    let sandbox_image = std::env::var("SANDBOX_IMAGE").unwrap_or_else(|_| {
        tracing::warn!("SANDBOX_IMAGE not set — using default :latest image");
        "azureclawacr.azurecr.io/openclaw-sandbox:latest".into()
    });
    let openai_endpoint = std::env::var("AZURE_OPENAI_ENDPOINT").unwrap_or_default();
    let foundry_endpoint = std::env::var("FOUNDRY_ENDPOINT").unwrap_or_default();
    let foundry_project_endpoint = std::env::var("FOUNDRY_PROJECT_ENDPOINT").unwrap_or_default();
    let foundry_deployments = std::env::var("FOUNDRY_DEPLOYMENTS").unwrap_or_default();
    let imds_client_id = std::env::var("IMDS_CLIENT_ID").unwrap_or_default();
    // Content Safety endpoint — defaults to Foundry endpoint if not set separately,
    // since Azure AI Services multi-service resources host Content Safety at the same base URL.
    let content_safety_endpoint =
        std::env::var("CONTENT_SAFETY_ENDPOINT").unwrap_or_else(|_| foundry_endpoint.clone());

    if openai_endpoint.is_empty() && foundry_endpoint.is_empty() {
        tracing::warn!(
            "Neither AZURE_OPENAI_ENDPOINT nor FOUNDRY_ENDPOINT set — inference routing will fail"
        );
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

    let byo_strict = matches!(
        std::env::var("BYO_STRICT_MODE").as_deref(),
        Ok("1" | "true" | "True" | "TRUE" | "yes")
    );
    if byo_strict {
        tracing::info!("BYO strict-mode enabled — invalid BYO contracts will be rejected");
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
        byo_strict,
    });

    Controller::new(sandboxes, kube::runtime::watcher::Config::default())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("ClawSandbox", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
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
mod tests;

pub mod runtime;
