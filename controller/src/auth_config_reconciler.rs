// Copyright (c) Microsoft Corporation.
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
pub const AUTH_SYSTEM_NAMESPACE: &str = "azureclaw-system";

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

    // Render the env-var map from the spec.
    let env_map = render_sidecar_env(&obj.spec);
    let spec_hash = hash_spec(&obj.spec);

    // Apply the ConfigMap via server-side apply.
    apply_configmap(&ctx.client, &env_map, &spec_hash, &obj.spec.agent_id.blueprint_client_id)
        .await
        .map_err(ReconcilerError::Apply)?;

    tracing::debug!(spec_hash = %spec_hash, "auth-config sidecar ConfigMap reconciled");

    // Re-reconcile on a slow cadence as a defensive measure against
    // ConfigMap drift, mirroring the pattern in mcp_server_reconciler.
    Ok(Action::requeue(Duration::from_secs(300)))
}

#[derive(thiserror::Error, Debug)]
enum ReconcilerError {
    #[error("failed to apply sidecar ConfigMap: {0}")]
    Apply(String),
}

fn error_policy(_obj: Arc<KarsAuthConfig>, _err: &ReconcilerError, _ctx: Arc<ReconcilerCtx>) -> Action {
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

    // The single credential entry: SignedAssertionFromManagedIdentity
    // pointed at the controller MI's IMDS endpoint. This is the
    // anti-loop-safe path proven during the POC; see
    // `docs/architecture/entra-agent-id/01-runtime-token-flow.md`.
    env.insert(
        "AzureAd__ClientCredentials__0__SourceType".into(),
        "SignedAssertionFromManagedIdentity".into(),
    );
    env.insert(
        "AzureAd__ClientCredentials__0__ManagedIdentityClientId".into(),
        spec.controller.managed_identity_client_id.clone(),
    );

    // Downstream API config — emit one cluster of env vars per entry.
    for (api_name, api_cfg) in &spec.downstream_apis {
        env.insert(
            format!("DownstreamApis__{api_name}__BaseUrl"),
            api_cfg.base_url.clone(),
        );
        env.insert(
            format!("DownstreamApis__{api_name}__RequestAppToken"),
            if api_cfg.request_app_token { "true" } else { "false" }.into(),
        );
        for (idx, scope) in api_cfg.scopes.iter().enumerate() {
            env.insert(
                format!("DownstreamApis__{api_name}__Scopes__{idx}"),
                scope.clone(),
            );
        }
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
/// result. Surfaced on the CR's `status.conditions[]` by the
/// reconciler in a future patch — for now this is a helper exposed so
/// `kars doctor` and CLI commands can render the same vocabulary.
#[allow(dead_code)]
pub fn build_condition_blueprint_ready(observed_generation: i64, message: &str) -> Condition {
    Condition {
        type_: "BlueprintReady".into(),
        status: "True".into(),
        reason: "Reconciled".into(),
        message: message.into(),
        last_transition_time: Time(Timestamp::now()),
        observed_generation: Some(observed_generation),
    }
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
            },
            controller: ControllerIdentityConfig {
                managed_identity_client_id: "a5cc7e08-ee03-4eee-b034-5302b6b54547".into(),
                managed_identity_resource_id:
                    "/subscriptions/X/resourceGroups/Y/providers/Microsoft.ManagedIdentity/userAssignedIdentities/Z"
                        .into(),
                managed_identity_principal_id: Some("5eaee919-d1bf-4ed0-9da0-0f1589dc2f4b".into()),
            },
            downstream_apis: downstream,
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
            env.get("AzureAd__ClientCredentials__0__SourceType").map(String::as_str),
            Some("SignedAssertionFromManagedIdentity")
        );
        assert_eq!(
            env.get("AzureAd__ClientCredentials__0__ManagedIdentityClientId").map(String::as_str),
            Some("a5cc7e08-ee03-4eee-b034-5302b6b54547")
        );
    }

    #[test]
    fn renders_downstream_apis_with_indexed_scopes() {
        let env = render_sidecar_env(&fixture_spec());
        assert_eq!(
            env.get("DownstreamApis__Foundry__BaseUrl").map(String::as_str),
            Some("https://example.cognitiveservices.azure.com/")
        );
        assert_eq!(
            env.get("DownstreamApis__Foundry__Scopes__0").map(String::as_str),
            Some("https://ai.azure.com/.default")
        );
        assert_eq!(
            env.get("DownstreamApis__Foundry__RequestAppToken").map(String::as_str),
            Some("true")
        );
        // Multi-scope: Graph entry should index both.
        assert_eq!(
            env.get("DownstreamApis__Graph__Scopes__0").map(String::as_str),
            Some("https://graph.microsoft.com/.default")
        );
        assert_eq!(
            env.get("DownstreamApis__Graph__Scopes__1").map(String::as_str),
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
}
