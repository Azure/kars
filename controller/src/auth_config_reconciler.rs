// Copyright (c) Microsoft Corporation.
// ci:loc-ok — Entra Agent ID auth config reconciler, split planned for Phase 1

// Licensed under the MIT License.

//! `KarsAuthConfig` reconciler — materialises the sidecar ConfigMap.
//!
//! Watches the cluster-singleton `KarsAuthConfig/default` CR and, on
//! every change, renders a flat ConfigMap of environment variables the
//! Microsoft Entra SDK sidecar consumes. Sandbox pods then
//! `envFrom: configMapRef: { name: kars-auth-sidecar-env }` on their
//! sidecar container.
//!
//! Why a ConfigMap intermediate? Pods cannot `envFrom` a CRD directly —
//! ConfigMaps/Secrets are the only first-class envFrom sources. The
//! CRD is the user-facing source of truth; the ConfigMap is the
//! controller-managed projection. The reconciler computes a stable
//! spec hash and records it as an annotation on the ConfigMap so the
//! sandbox reconciler can detect drift.
//!
//! ## What this reconciler does NOT do
//!
//! - It does NOT call Microsoft Graph to verify the blueprint exists.
//!   That is a separate cross-cutting health check delivered by
//!   `kars doctor` and surfaced via the
//!   `KarsAuthConfig.status.conditions.BlueprintReady` condition,
//!   which the sandbox reconciler can read but does not mutate.
//! - It does NOT manage the controller MI or the AKS node-pool VMSS
//!   identity assignment. Those are CLI-time operations performed by
//!   `kars mesh setup-trust`; the reconciler trusts the CR's contents.
//! - It does NOT create per-sandbox agent identities. Those are
//!   provisioned lazily by the sandbox reconciler via
//!   `agent_identity::create_agent_identity`.

use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, ObjectMeta};
use kube::{
    Client, ResourceExt,
    api::{Api, Patch, PatchParams},
    runtime::controller::{Action, Controller},
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::auth_config::{DEFAULT_AUTH_CONFIG_NAME, KarsAuthConfig, KarsAuthConfigSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::jiff::Timestamp;

/// Field manager for SSA patches.
const FIELD_MANAGER: &str = "kars-auth-config-reconciler";

/// Name of the ConfigMap materialised by this reconciler.
pub const SIDECAR_ENV_CONFIGMAP: &str = "kars-auth-sidecar-env";

/// Namespace the ConfigMap is created in. Sandbox pods read it via
/// their own namespace if we projected per-namespace, or — preferred —
/// the controller mirrors it into every sandbox namespace at
/// sandbox-reconcile time. We use the canonical system namespace as
/// the source-of-truth copy.
pub const AUTH_SYSTEM_NAMESPACE: &str = "kars-system";

/// Annotation key on the ConfigMap recording the spec hash used to
/// generate it. Lets downstream reconcilers detect drift without
/// re-reading the CRD.
pub const SPEC_HASH_ANNOTATION: &str = "kars.azure.com/auth-config-spec-hash";

/// Annotation key recording the blueprint client ID. Surfaced for
/// human diagnosis (`kubectl describe cm kars-auth-sidecar-env`).
pub const BLUEPRINT_ANNOTATION: &str = "kars.azure.com/blueprint-client-id";

/// Run the reconciler. Spawned from `main.rs` alongside the existing
/// reconcilers. Non-fatal when the CRD is absent — the cluster starts
/// in anonymous-tier mode and the reconciler waits for an operator to
/// install the CRD before doing anything.
pub async fn run(client: Client) -> Result<()> {
    let api: Api<KarsAuthConfig> = Api::all(client.clone());

    // Discover whether the CRD is even installed — if not, sit dormant.
    // This matches the no-CRD-no-crash pattern used by
    // `mcp_server_reconciler::run`.
    if api.list(&Default::default()).await.is_err() {
        tracing::warn!(
            "KarsAuthConfig CRD not installed; auth-config reconciler idle (cluster runs in anonymous tier)"
        );
        // Block forever — we'll be restarted with the CRD installed.
        std::future::pending::<()>().await;
        return Ok(());
    }

    tracing::info!(
        "auth-config reconciler starting (watching KarsAuthConfig/{DEFAULT_AUTH_CONFIG_NAME})"
    );

    let ctx = Arc::new(ReconcilerCtx { client });

    Controller::new(api, Default::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => tracing::debug!(?obj, "auth-config reconciled"),
                Err(e) => tracing::warn!(error = %e, "auth-config reconcile error"),
            }
        })
        .await;
    Ok(())
}

struct ReconcilerCtx {
    client: Client,
}

async fn reconcile(
    obj: Arc<KarsAuthConfig>,
    ctx: Arc<ReconcilerCtx>,
) -> Result<Action, ReconcilerError> {
    let name = obj.name_any();

    // Singleton check: refuse any CR not named `default`. We surface
    // this as a status condition so the operator can self-diagnose.
    if name != DEFAULT_AUTH_CONFIG_NAME {
        tracing::warn!(
            cr_name = %name,
            "ignoring KarsAuthConfig with non-singleton name (must be '{DEFAULT_AUTH_CONFIG_NAME}')",
        );
        return Ok(Action::await_change());
    }

    // Validate the spec is configurationally consistent for the
    // chosen credential mode. ManagedIdentityImds requires the MI
    // clientId to be populated; WorkloadIdentity has no field
    // requirements. A misconfigured spec surfaces as a Degraded
    // condition so an operator can self-diagnose without trawling
    // logs, and we DO NOT materialise the sidecar ConfigMap (which
    // would otherwise propagate the misconfiguration to running
    // sandboxes).
    if !obj.spec.controller.is_valid_for_mode() {
        let reason = match obj.spec.controller.credential_mode {
            crate::auth_config::CredentialMode::ManagedIdentityImds => {
                "credentialMode=ManagedIdentityImds requires \
                 spec.controller.managedIdentityClientId to be set"
            }
            crate::auth_config::CredentialMode::WorkloadIdentity => {
                "internal: WorkloadIdentity should not fail is_valid_for_mode"
            }
        };
        tracing::warn!(
            reason,
            "KarsAuthConfig spec is invalid for chosen credentialMode"
        );
        let _ = patch_degraded_status(
            &ctx.client,
            &name,
            obj.metadata.generation.unwrap_or(0),
            "InvalidCredentialMode",
            reason,
        )
        .await;
        return Ok(Action::requeue(std::time::Duration::from_secs(60)));
    }

    // Render the env-var map from the spec.
    let env_map = render_sidecar_env(&obj.spec);
    let spec_hash = hash_spec(&obj.spec);

    // Apply the ConfigMap via server-side apply.
    apply_configmap(
        &ctx.client,
        &env_map,
        &spec_hash,
        &obj.spec.agent_id.blueprint_client_id,
    )
    .await
    .map_err(ReconcilerError::Apply)?;

    tracing::debug!(spec_hash = %spec_hash, "auth-config sidecar ConfigMap reconciled");

    // Patch status.Ready=True so the sandbox reconciler can gate
    // agent-id provisioning on a real readiness signal (rubber-duck
    // critique #7). The condition reason `Reconciled` matches the
    // helper in `build_condition_blueprint_ready`. Best-effort: a
    // failed status patch is logged but doesn't fail the reconcile —
    // the ConfigMap is already correct and the next reconcile retries
    // the patch.
    //
    // No-op guard: SSA patches with `.force()` overwrite our
    // managed fields including `lastTransitionTime`, which bumps the
    // resourceVersion on every reconcile and triggers another watch
    // event. That creates an infinite reconcile loop. Only patch
    // when the observed status doesn't already reflect what we'd
    // write (per the patch-only-when-different pattern documented
    // in kube-rs's controller docs §"status writers").
    let observed_generation = obj.metadata.generation.unwrap_or(0);
    let current_phase = obj.status.as_ref().and_then(|s| s.phase.as_deref());
    let current_observed_gen = obj.status.as_ref().and_then(|s| s.observed_generation);
    let already_ready = current_phase == Some(crate::status::phase::PHASE_READY)
        && current_observed_gen == Some(observed_generation);
    if !already_ready
        && let Err(e) = patch_ready_status(
            &ctx.client,
            &name,
            observed_generation,
            &format!(
                "ConfigMap kars-system/{SIDECAR_ENV_CONFIGMAP} materialised (hash {spec_hash})"
            ),
        )
        .await
    {
        tracing::warn!(error = %e, "patch KarsAuthConfig status failed; will retry next reconcile");
    }

    // Re-reconcile on a slow cadence as a defensive measure against
    // ConfigMap drift, mirroring the pattern in mcp_server_reconciler.
    Ok(Action::requeue(Duration::from_secs(300)))
}

#[derive(thiserror::Error, Debug)]
enum ReconcilerError {
    #[error("failed to apply sidecar ConfigMap: {0}")]
    Apply(String),
}

fn error_policy(
    _obj: Arc<KarsAuthConfig>,
    _err: &ReconcilerError,
    _ctx: Arc<ReconcilerCtx>,
) -> Action {
    Action::requeue(Duration::from_secs(30))
}

/// Render the sidecar env-var map from a `KarsAuthConfig` spec.
///
/// The Microsoft Entra SDK sidecar consumes nested settings using
/// double-underscore segments — `AzureAd__ClientId`,
/// `AzureAd__ClientCredentials__0__SourceType`, etc. This mapping is
/// the controller-side mirror of the YAML structure documented in
/// `docs/architecture/entra-agent-id/01-runtime-token-flow.md`.
pub fn render_sidecar_env(spec: &KarsAuthConfigSpec) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = BTreeMap::new();

    // Core Entra wiring.
    env.insert("AzureAd__TenantId".into(), spec.tenant.tenant_id.clone());
    env.insert(
        "AzureAd__Instance".into(),
        spec.tenant.authority_host.clone(),
    );
    env.insert(
        "AzureAd__ClientId".into(),
        spec.agent_id.blueprint_client_id.clone(),
    );

    // Credential entry shape depends on the controller's chosen mode.
    //
    // - ManagedIdentityImds (Pattern A, default — corp-tenant
    //   safe): the sidecar uses `SignedAssertionFromManagedIdentity`
    //   to bridge an IMDS-issued MI token into the blueprint's
    //   MI-as-FIC. Anti-loop-safe per the POC; required when the
    //   tenant's FIC issuer-allowlist policy blocks the AKS OIDC.
    //
    // - WorkloadIdentity (Pattern B — OSS / non-restricted tenant):
    //   the sidecar uses `SignedAssertionFilePath` to present its
    //   projected SA token directly as the federated assertion
    //   against the blueprint's SA-as-FIC. No MI involvement, no
    //   VMSS identity assignment needed for the sidecar.
    //
    // See `crate::auth_config::CredentialMode` and
    // `docs/architecture/entra-agent-id/01-runtime-token-flow.md`.
    match spec.controller.credential_mode {
        crate::auth_config::CredentialMode::ManagedIdentityImds => {
            env.insert(
                "AzureAd__ClientCredentials__0__SourceType".into(),
                "SignedAssertionFromManagedIdentity".into(),
            );
            // Empty when the operator declared MI mode but forgot to
            // populate the field — let the sidecar fail loudly at
            // boot rather than silently using "default MI" (which on
            // a multi-MI node would attribute calls unpredictably).
            env.insert(
                "AzureAd__ClientCredentials__0__ManagedIdentityClientId".into(),
                spec.controller
                    .managed_identity_client_id
                    .clone()
                    .unwrap_or_default(),
            );
        }
        crate::auth_config::CredentialMode::WorkloadIdentity => {
            env.insert(
                "AzureAd__ClientCredentials__0__SourceType".into(),
                "SignedAssertionFilePath".into(),
            );
            // AKS azure-wi-webhook projects the federated SA token at
            // this path when the pod is labeled
            // `azure.workload.identity/use=true` and the SA is
            // annotated with `azure.workload.identity/client-id`.
            // The path is also overrideable via `AZURE_FEDERATED_TOKEN_FILE`
            // env (set by the webhook), but the absolute path is the
            // documented contract — the sidecar reads it directly.
            env.insert(
                "AzureAd__ClientCredentials__0__SignedAssertionFileDiskPath".into(),
                "/var/run/secrets/azure/tokens/azure-identity-token".into(),
            );
        }
    }

    // Downstream API config — emit one cluster of env vars per entry.
    for (api_name, api_cfg) in &spec.downstream_apis {
        env.insert(
            format!("DownstreamApis__{api_name}__BaseUrl"),
            api_cfg.base_url.clone(),
        );
        env.insert(
            format!("DownstreamApis__{api_name}__RequestAppToken"),
            if api_cfg.request_app_token {
                "true"
            } else {
                "false"
            }
            .into(),
        );
        for (idx, scope) in api_cfg.scopes.iter().enumerate() {
            env.insert(
                format!("DownstreamApis__{api_name}__Scopes__{idx}"),
                scope.clone(),
            );
        }
    }

    // Phase 6: auto-emit an `AgentMesh` downstream entry when the
    // operator opts into Entra-signed mesh peer auth. The router's
    // `/v1/mesh-token` route calls `sidecar.get_token("api://agentmesh/.default")`,
    // which the sidecar resolves via `DownstreamApis__AgentMesh__*`
    // — without this entry the sidecar 404s the request and the
    // entrypoint falls back to anonymous tier.
    //
    // We auto-emit so flipping the CRD field is a one-toggle change.
    // If the operator has already declared an explicit `AgentMesh`
    // downstream API entry (e.g. for a custom relay audience), we
    // respect it and skip the auto-emit. The case match is
    // case-insensitive against the standard Pascal-case key the
    // sidecar expects, matching downstream env-var rendering.
    if matches!(
        spec.mesh_auth_backend,
        super::auth_config::MeshAuthBackend::EntraAgentIdentity
    ) && !spec
        .downstream_apis
        .keys()
        .any(|k| k.eq_ignore_ascii_case("AgentMesh"))
    {
        // BaseUrl is irrelevant — the route never calls the relay
        // via the sidecar's downstream HTTP forwarder. It's required
        // by the sidecar's config validator, so set a sentinel value
        // that's deliberately not reachable from the sandbox NP.
        env.insert(
            "DownstreamApis__AgentMesh__BaseUrl".into(),
            "https://agentmesh.invalid/".into(),
        );
        env.insert(
            "DownstreamApis__AgentMesh__RequestAppToken".into(),
            "true".into(),
        );
        let audience = spec
            .mesh_auth_audience
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("api://agentmesh/.default");
        env.insert(
            "DownstreamApis__AgentMesh__Scopes__0".into(),
            audience.to_string(),
        );
    }

    env
}

/// Compute a stable hash of the spec for drift detection.
///
/// Uses Rust's built-in SipHash via std::hash, applied to a
/// deterministic serialisation. NOT cryptographically secure — we only
/// need collision resistance for ConfigMap-drift detection on
/// human-managed CRs.
pub fn hash_spec(spec: &KarsAuthConfigSpec) -> String {
    use std::hash::{Hash, Hasher};
    // Render to a deterministic JSON (BTreeMap iteration is sorted
    // by key) and hash. Avoids depending on cryptographic-strength
    // crates for a non-security-critical fingerprint.
    let canonical = serde_json::to_string(spec).unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

async fn apply_configmap(
    client: &Client,
    env: &BTreeMap<String, String>,
    spec_hash: &str,
    blueprint_client_id: &str,
) -> Result<(), String> {
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), AUTH_SYSTEM_NAMESPACE);

    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(SPEC_HASH_ANNOTATION.into(), spec_hash.into());
    annotations.insert(BLUEPRINT_ANNOTATION.into(), blueprint_client_id.into());

    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(SIDECAR_ENV_CONFIGMAP.into()),
            namespace: Some(AUTH_SYSTEM_NAMESPACE.into()),
            annotations: Some(annotations),
            labels: Some({
                let mut l = BTreeMap::new();
                l.insert("app.kubernetes.io/managed-by".into(), "kars".into());
                l.insert(
                    "app.kubernetes.io/component".into(),
                    "auth-sidecar-env".into(),
                );
                l
            }),
            ..Default::default()
        },
        data: Some(env.clone()),
        ..Default::default()
    };

    let pp = PatchParams::apply(FIELD_MANAGER).force();
    api.patch(SIDECAR_ENV_CONFIGMAP, &pp, &Patch::Apply(&cm))
        .await
        .map_err(|e| format!("apply ConfigMap failed: {e}"))?;

    Ok(())
}

/// Build the well-known condition entries from a recent reconcile
/// result. Surfaced on the CR's `status.conditions[]` by
/// `patch_ready_status` below, and re-exported so `kars doctor`
/// and CLI commands can render the same vocabulary.
pub fn build_condition_blueprint_ready(observed_generation: i64, message: &str) -> Condition {
    Condition {
        type_: "SidecarConfigMaterialized".into(),
        status: "True".into(),
        reason: "Reconciled".into(),
        message: message.into(),
        last_transition_time: Time(Timestamp::now()),
        observed_generation: Some(observed_generation),
    }
}

/// Patch `KarsAuthConfig/<name>.status` with `phase=Ready` plus the
/// matching `SidecarConfigMaterialized=True` condition.
///
/// Server-side-apply with a dedicated field manager so the sandbox
/// reconciler's status patches don't clobber these condition entries.
async fn patch_ready_status(
    client: &Client,
    name: &str,
    observed_generation: i64,
    message: &str,
) -> Result<(), String> {
    let api: Api<KarsAuthConfig> = Api::all(client.clone());
    let condition = build_condition_blueprint_ready(observed_generation, message);
    let patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsAuthConfig",
        "status": {
            "phase": crate::status::phase::PHASE_READY,
            "observedGeneration": observed_generation,
            "conditions": [{
                "type": condition.type_,
                "status": condition.status,
                "reason": condition.reason,
                "message": condition.message,
                "lastTransitionTime": condition.last_transition_time.0.to_string(),
                "observedGeneration": observed_generation,
            }],
        }
    });
    api.patch_status(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&patch),
    )
    .await
    .map_err(|e| format!("patch_status {name}: {e}"))?;
    Ok(())
}

/// Patch `KarsAuthConfig/<name>.status` with `phase=Degraded` plus a
/// `SidecarConfigMaterialized=False` condition explaining why the
/// reconciler refused to materialise the sidecar-env ConfigMap.
async fn patch_degraded_status(
    client: &Client,
    name: &str,
    observed_generation: i64,
    reason: &str,
    message: &str,
) -> Result<(), String> {
    let api: Api<KarsAuthConfig> = Api::all(client.clone());
    let condition = Condition {
        type_: "SidecarConfigMaterialized".into(),
        status: "False".into(),
        reason: reason.into(),
        message: message.into(),
        last_transition_time: Time(Timestamp::now()),
        observed_generation: Some(observed_generation),
    };
    let patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsAuthConfig",
        "status": {
            "phase": crate::status::phase::PHASE_DEGRADED,
            "observedGeneration": observed_generation,
            "conditions": [{
                "type": condition.type_,
                "status": condition.status,
                "reason": condition.reason,
                "message": condition.message,
                "lastTransitionTime": condition.last_transition_time.0.to_string(),
                "observedGeneration": observed_generation,
            }],
        }
    });
    api.patch_status(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&patch),
    )
    .await
    .map_err(|e| format!("patch_status {name}: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_config::{
        AgentIdConfig, ControllerIdentityConfig, DownstreamApiConfig, TenantConfig,
    };

    fn fixture_spec() -> KarsAuthConfigSpec {
        let mut downstream = std::collections::BTreeMap::new();
        downstream.insert(
            "Foundry".into(),
            DownstreamApiConfig {
                base_url: "https://example.cognitiveservices.azure.com/".into(),
                scopes: vec!["https://ai.azure.com/.default".into()],
                request_app_token: true,
            },
        );
        downstream.insert(
            "Graph".into(),
            DownstreamApiConfig {
                base_url: "https://graph.microsoft.com/v1.0/".into(),
                scopes: vec![
                    "https://graph.microsoft.com/.default".into(),
                    "User.Read".into(),
                ],
                request_app_token: true,
            },
        );
        KarsAuthConfigSpec {
            tenant: TenantConfig {
                tenant_id: "72f988bf-86f1-41af-91ab-2d7cd011db47".into(),
                authority_host: "https://login.microsoftonline.com/".into(),
                service_management_reference: None,
            },
            agent_id: AgentIdConfig {
                blueprint_client_id: "9010cbe3-ee13-4cb6-aa5f-f892910804a0".into(),
                blueprint_object_id: "5a9587be-cd7f-4c58-999f-b93d22757004".into(),
                sponsor_user_object_ids: vec![],
            },
            controller: ControllerIdentityConfig {
                credential_mode: Default::default(),
                managed_identity_client_id: Some("a5cc7e08-ee03-4eee-b034-5302b6b54547".into()),
                managed_identity_resource_id: Some(
                    "/subscriptions/X/resourceGroups/Y/providers/Microsoft.ManagedIdentity/userAssignedIdentities/Z"
                        .into(),
                ),
                managed_identity_principal_id: Some("5eaee919-d1bf-4ed0-9da0-0f1589dc2f4b".into()),
            },
            downstream_apis: downstream,
            foundry_rbac: vec![],
            mesh_auth_backend: Default::default(),
            mesh_auth_audience: None,
        }
    }

    #[test]
    fn renders_core_entra_env() {
        let env = render_sidecar_env(&fixture_spec());
        assert_eq!(
            env.get("AzureAd__TenantId").map(String::as_str),
            Some("72f988bf-86f1-41af-91ab-2d7cd011db47")
        );
        assert_eq!(
            env.get("AzureAd__ClientId").map(String::as_str),
            Some("9010cbe3-ee13-4cb6-aa5f-f892910804a0")
        );
        assert_eq!(
            env.get("AzureAd__ClientCredentials__0__SourceType")
                .map(String::as_str),
            Some("SignedAssertionFromManagedIdentity")
        );
        assert_eq!(
            env.get("AzureAd__ClientCredentials__0__ManagedIdentityClientId")
                .map(String::as_str),
            Some("a5cc7e08-ee03-4eee-b034-5302b6b54547")
        );
    }

    #[test]
    fn renders_downstream_apis_with_indexed_scopes() {
        let env = render_sidecar_env(&fixture_spec());
        assert_eq!(
            env.get("DownstreamApis__Foundry__BaseUrl")
                .map(String::as_str),
            Some("https://example.cognitiveservices.azure.com/")
        );
        assert_eq!(
            env.get("DownstreamApis__Foundry__Scopes__0")
                .map(String::as_str),
            Some("https://ai.azure.com/.default")
        );
        assert_eq!(
            env.get("DownstreamApis__Foundry__RequestAppToken")
                .map(String::as_str),
            Some("true")
        );
        // Multi-scope: Graph entry should index both.
        assert_eq!(
            env.get("DownstreamApis__Graph__Scopes__0")
                .map(String::as_str),
            Some("https://graph.microsoft.com/.default")
        );
        assert_eq!(
            env.get("DownstreamApis__Graph__Scopes__1")
                .map(String::as_str),
            Some("User.Read")
        );
    }

    #[test]
    fn hash_is_stable_for_identical_spec() {
        let a = hash_spec(&fixture_spec());
        let b = hash_spec(&fixture_spec());
        assert_eq!(a, b);
    }

    #[test]
    fn hash_changes_when_blueprint_changes() {
        let mut a = fixture_spec();
        let mut b = fixture_spec();
        a.agent_id.blueprint_client_id = "00000000-0000-0000-0000-000000000001".into();
        b.agent_id.blueprint_client_id = "00000000-0000-0000-0000-000000000002".into();
        assert_ne!(hash_spec(&a), hash_spec(&b));
    }

    #[test]
    fn renders_workload_identity_mode_with_file_assertion() {
        let mut spec = fixture_spec();
        spec.controller.credential_mode = crate::auth_config::CredentialMode::WorkloadIdentity;
        // In WI mode, MI clientId is irrelevant; clear it to prove
        // the renderer doesn't depend on it.
        spec.controller.managed_identity_client_id = None;
        let env = render_sidecar_env(&spec);
        assert_eq!(
            env.get("AzureAd__ClientCredentials__0__SourceType")
                .map(String::as_str),
            Some("SignedAssertionFilePath")
        );
        assert_eq!(
            env.get("AzureAd__ClientCredentials__0__SignedAssertionFileDiskPath")
                .map(String::as_str),
            Some("/var/run/secrets/azure/tokens/azure-identity-token")
        );
        // The MI-mode env vars MUST NOT leak into the WI rendering.
        assert!(
            !env.contains_key("AzureAd__ClientCredentials__0__ManagedIdentityClientId"),
            "WI mode must not emit ManagedIdentityClientId"
        );
    }

    #[test]
    fn renders_managed_identity_mode_with_empty_field_emits_empty_string() {
        // Defensive: if an operator selects MI mode but forgets to
        // populate the clientId, the sidecar should fail loudly at
        // boot rather than silently using the node's "first MI".
        // The auth_config reconciler refuses to materialise such a
        // CR (validated by is_valid_for_mode), but this test pins
        // the render_sidecar_env shape so the bug can't slip past
        // the validator either.
        let mut spec = fixture_spec();
        spec.controller.managed_identity_client_id = None;
        let env = render_sidecar_env(&spec);
        assert_eq!(
            env.get("AzureAd__ClientCredentials__0__SourceType")
                .map(String::as_str),
            Some("SignedAssertionFromManagedIdentity")
        );
        assert_eq!(
            env.get("AzureAd__ClientCredentials__0__ManagedIdentityClientId")
                .map(String::as_str),
            Some("")
        );
    }

    #[test]
    fn is_valid_for_mode_requires_mi_field_in_mi_mode() {
        use crate::auth_config::{ControllerIdentityConfig, CredentialMode};

        let mut cfg = ControllerIdentityConfig {
            credential_mode: CredentialMode::ManagedIdentityImds,
            managed_identity_client_id: Some("abc".into()),
            managed_identity_resource_id: None,
            managed_identity_principal_id: None,
        };
        assert!(
            cfg.is_valid_for_mode(),
            "MI mode + populated clientId → valid"
        );

        cfg.managed_identity_client_id = None;
        assert!(
            !cfg.is_valid_for_mode(),
            "MI mode + missing clientId → invalid"
        );

        cfg.managed_identity_client_id = Some("   ".into());
        assert!(
            !cfg.is_valid_for_mode(),
            "MI mode + whitespace-only clientId → invalid"
        );

        cfg.managed_identity_client_id = Some("".into());
        assert!(
            !cfg.is_valid_for_mode(),
            "MI mode + empty clientId → invalid"
        );
    }

    #[test]
    fn is_valid_for_mode_does_not_require_mi_field_in_wi_mode() {
        use crate::auth_config::{ControllerIdentityConfig, CredentialMode};

        let cfg = ControllerIdentityConfig {
            credential_mode: CredentialMode::WorkloadIdentity,
            managed_identity_client_id: None,
            managed_identity_resource_id: None,
            managed_identity_principal_id: None,
        };
        assert!(cfg.is_valid_for_mode(), "WI mode has no field requirements");
    }

    #[test]
    fn anonymous_mesh_backend_does_not_emit_agent_mesh_entry() {
        // Default is Anonymous → no AgentMesh downstream auto-emit.
        // This is the backward-compat contract: every existing cluster
        // keeps rendering the exact same env, byte-identical, so a
        // controller upgrade does not bounce sidecar pods on an
        // unrelated drift.
        let env = render_sidecar_env(&fixture_spec());
        assert!(!env.contains_key("DownstreamApis__AgentMesh__BaseUrl"));
        assert!(!env.contains_key("DownstreamApis__AgentMesh__Scopes__0"));
        assert!(!env.contains_key("DownstreamApis__AgentMesh__RequestAppToken"));
    }

    #[test]
    fn entra_mesh_backend_auto_emits_agent_mesh_entry() {
        let mut spec = fixture_spec();
        spec.mesh_auth_backend = crate::auth_config::MeshAuthBackend::EntraAgentIdentity;
        let env = render_sidecar_env(&spec);
        assert_eq!(
            env.get("DownstreamApis__AgentMesh__Scopes__0")
                .map(String::as_str),
            Some("api://agentmesh/.default"),
            "default audience must match the entrypoint legacy scope"
        );
        assert_eq!(
            env.get("DownstreamApis__AgentMesh__RequestAppToken")
                .map(String::as_str),
            Some("true"),
            "app-token flow is the only flow that makes sense for a mesh peer"
        );
        assert!(
            env.contains_key("DownstreamApis__AgentMesh__BaseUrl"),
            "sidecar config validator requires BaseUrl; ours is sentinel"
        );
    }

    #[test]
    fn entra_mesh_backend_respects_custom_audience() {
        let mut spec = fixture_spec();
        spec.mesh_auth_backend = crate::auth_config::MeshAuthBackend::EntraAgentIdentity;
        spec.mesh_auth_audience = Some("api://my-custom-relay/.default".into());
        let env = render_sidecar_env(&spec);
        assert_eq!(
            env.get("DownstreamApis__AgentMesh__Scopes__0")
                .map(String::as_str),
            Some("api://my-custom-relay/.default")
        );
    }

    #[test]
    fn entra_mesh_backend_does_not_overwrite_operator_supplied_entry() {
        // Forward-compat: an operator that has already added an
        // explicit `AgentMesh` downstream API entry (e.g. for a
        // multi-relay scenario with non-default BaseUrl) MUST win
        // over the auto-emit.
        let mut spec = fixture_spec();
        spec.mesh_auth_backend = crate::auth_config::MeshAuthBackend::EntraAgentIdentity;
        spec.downstream_apis.insert(
            "AgentMesh".into(),
            DownstreamApiConfig {
                base_url: "https://operator-supplied-relay.example/".into(),
                scopes: vec!["api://operator-scope/.default".into()],
                request_app_token: true,
            },
        );
        let env = render_sidecar_env(&spec);
        assert_eq!(
            env.get("DownstreamApis__AgentMesh__BaseUrl")
                .map(String::as_str),
            Some("https://operator-supplied-relay.example/"),
            "operator-supplied BaseUrl must win"
        );
        assert_eq!(
            env.get("DownstreamApis__AgentMesh__Scopes__0")
                .map(String::as_str),
            Some("api://operator-scope/.default"),
            "operator-supplied scope must win"
        );
    }
}
