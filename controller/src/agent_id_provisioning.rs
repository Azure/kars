// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Per-sandbox Entra Agent Identity provisioning orchestration.
//!
//! Sits between the sandbox reconciler (`reconciler/mod.rs`) and the
//! Graph client (`agent_identity.rs`). Owns the "resolve mesh-auth
//! mode → provision/recover identity → materialise sidecar
//! ConfigMap in the sandbox namespace → produce a ready-to-inject
//! summary for the pod-spec assembler" pipeline.
//!
//! ## Why this lives in a dedicated module
//!
//! The sandbox reconciler is 2900+ LoC and the agent-id path has
//! enough load-bearing logic (idempotent recovery, three-way mode
//! resolution, per-namespace CM mirroring, status patching) that
//! inlining it there would obscure both the reconciler's structure
//! AND the security-critical ordering between Graph provisioning
//! and pod-spec assembly. Keeping the orchestration here means the
//! reconciler integration is a single `match` arm.
//!
//! ## Idempotency contract
//!
//! [`ensure_agent_identity_for_sandbox`] is safe to call on every
//! reconcile. The flow is:
//!
//! 1. If `KarsSandbox.status.agentIdentity` is populated, GET the SP
//!    from Graph. On 200 reuse; on 404 reprovision (drop the stale
//!    status). On 5xx requeue with backoff.
//! 2. If status is empty, list the cluster's agent identities filtered
//!    by `kars-sandbox-uid:<uid>` tag. If one matches (a previous
//!    reconcile created it but crashed before status patch), reuse.
//! 3. Otherwise create a new one, patch status immediately, then
//!    return the new identity to the caller.
//!
//! Crash window: between the Graph POST succeeding and the status
//! patch landing, a duplicate SP could be created on retry. Step 2
//! (the tag lookup) catches that case on the next reconcile. The
//! `agent_identity_reaper` (separate module, follow-up PR) sweeps any
//! truly-orphaned SPs whose owning sandbox is gone.

use crate::agent_identity::{AgentIdentity, AgentIdentityClient, AgentIdentityConfig};
use crate::auth_config::{DEFAULT_AUTH_CONFIG_NAME, KarsAuthConfig, KarsAuthConfigSpec};
use crate::auth_config_reconciler::render_sidecar_env;
use crate::crd::{AgentIdentityStatus, KarsSandbox, MeshAuthMode};
use crate::sidecar_injection::SIDECAR_ENV_CONFIGMAP_NAME;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{OwnerReference, ObjectMeta};
use kube::{
    Client, ResourceExt,
    api::{Api, Patch, PatchParams},
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Field manager name used for all SSA patches issued by this module.
/// Distinct from the sandbox reconciler's field manager so kube can
/// arbitrate ownership cleanly when multiple managers touch the same
/// status subresource.
pub const FIELD_MANAGER: &str = "kars-agent-id-provisioner";

/// Outcome of `ensure_agent_identity_for_sandbox`.
#[derive(Debug, Clone)]
pub enum ProvisioningOutcome {
    /// Mesh-auth mode resolved to a non-AgentId path (Anonymous or
    /// AgentId-unsupported because KarsAuthConfig is absent). The
    /// reconciler should proceed with the legacy fedcred + anonymous-
    /// tier path; no sidecar injection.
    Skipped { reason: SkipReason },
    /// Agent identity is ready for injection. Caller appends the
    /// sidecar container, sets the router env vars to `agent_app_id`,
    /// and flips `agent_id_mode=true` on the egress-guard.
    Ready {
        agent_identity: AgentIdentityStatus,
        /// The cached `KarsAuthConfig.spec` so the reconciler doesn't
        /// re-fetch it. Borrowed by the pod-spec assembler.
        auth_spec: Arc<KarsAuthConfigSpec>,
    },
    /// Provisioning failed in a way the reconciler should surface as
    /// `status.conditions.AgentIdentityReady=False`. The reconciler is
    /// expected to requeue with backoff; partial progress is preserved
    /// (any in-progress patch will be retried next reconcile).
    Failed { reason: String, retry_after_secs: u64 },
}

/// Why provisioning was skipped (informational; surfaces in status).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// `meshAuth.mode` was explicitly set to `Anonymous`.
    ExplicitAnonymous,
    /// `meshAuth.mode` was `Auto` (or unset) and `KarsAuthConfig/default`
    /// does not exist on the cluster — anonymous-tier fallback per the
    /// CRD contract.
    AutoFallbackNoConfig,
    /// `KarsAuthConfig/default` exists but its status is not `Ready` yet.
    /// Reconciler should requeue and try again once the auth-config
    /// reconciler has caught up.
    AuthConfigNotReady,
}

impl SkipReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            SkipReason::ExplicitAnonymous => "ExplicitAnonymous",
            SkipReason::AutoFallbackNoConfig => "AutoFallbackNoConfig",
            SkipReason::AuthConfigNotReady => "AuthConfigNotReady",
        }
    }
}

/// Cluster-wide cache of `AgentIdentityClient`s keyed by blueprint
/// client ID. Token caches inside each client are shared across all
/// concurrent sandbox reconciles, so back-to-back sandbox creates
/// don't each roundtrip Entra for a blueprint token.
///
/// The cache also serves as the source of truth for the cluster UID
/// (passed in at first cache fill from the controller's leader-election
/// lease metadata). Keying by blueprint client ID lets us tolerate the
/// (rare) case where a KarsAuthConfig is edited to point at a new
/// blueprint mid-flight — the cache just grows by one entry.
pub struct ProvisionerCache {
    clients: RwLock<BTreeMap<String, Arc<AgentIdentityClient>>>,
}

impl ProvisionerCache {
    pub fn new() -> Self {
        Self {
            clients: RwLock::new(BTreeMap::new()),
        }
    }

    async fn get_or_init(
        &self,
        spec: &KarsAuthConfigSpec,
        cluster_uid: &str,
    ) -> Arc<AgentIdentityClient> {
        let key = spec.agent_id.blueprint_client_id.clone();
        {
            let r = self.clients.read().await;
            if let Some(c) = r.get(&key) {
                return c.clone();
            }
        }
        let mut w = self.clients.write().await;
        // Double-check after acquiring write lock — another task may
        // have raced and inserted while we waited.
        if let Some(c) = w.get(&key) {
            return c.clone();
        }
        let cfg = AgentIdentityConfig::from_auth_config(spec, cluster_uid.to_string());
        let client = Arc::new(AgentIdentityClient::new(cfg));
        w.insert(key, client.clone());
        client
    }
}

impl Default for ProvisionerCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve the effective mesh-auth mode given the sandbox CR's
/// declared mode and the cluster's auth config state.
///
/// Pure function — no I/O — so it can be unit-tested without a kube
/// fixture. Returns the resolved mode (one of AgentId / Anonymous)
/// plus a reason string suitable for surfacing in status conditions.
pub fn resolve_mesh_auth_mode(
    declared: MeshAuthMode,
    auth_config_present_and_ready: bool,
) -> ResolvedMeshAuthMode {
    match (declared, auth_config_present_and_ready) {
        (MeshAuthMode::Anonymous, _) => ResolvedMeshAuthMode::Anonymous {
            reason: SkipReason::ExplicitAnonymous,
        },
        (MeshAuthMode::AgentId, true) => ResolvedMeshAuthMode::AgentId,
        (MeshAuthMode::AgentId, false) => ResolvedMeshAuthMode::Anonymous {
            // Explicit AgentId but no ready config — surface as
            // not-ready (transient) rather than no-config (terminal)
            // because the user explicitly asked for agent-id.
            reason: SkipReason::AuthConfigNotReady,
        },
        (MeshAuthMode::Auto, true) => ResolvedMeshAuthMode::AgentId,
        (MeshAuthMode::Auto, false) => ResolvedMeshAuthMode::Anonymous {
            reason: SkipReason::AutoFallbackNoConfig,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedMeshAuthMode {
    AgentId,
    Anonymous { reason: SkipReason },
}

/// Look up `KarsAuthConfig/default` and assess its readiness.
///
/// Returns `Ok((spec, ready))` when the CR exists. `ready` is true
/// iff the CR's `status.phase == "Ready"` AND the spec hash matches
/// the materialised ConfigMap (the latter check is deferred to the
/// auth-config reconciler, so for now we trust the phase).
///
/// Returns `Ok(None)` when the CR does not exist (anonymous-tier
/// fallback for `Auto` mode).
///
/// Returns `Err(_)` on transient kube API failures — the caller
/// should requeue.
pub async fn load_auth_config(
    client: &Client,
) -> Result<Option<(KarsAuthConfigSpec, bool)>, String> {
    let api: Api<KarsAuthConfig> = Api::all(client.clone());
    match api.get(DEFAULT_AUTH_CONFIG_NAME).await {
        Ok(cr) => {
            let ready = cr
                .status
                .as_ref()
                .and_then(|s| s.phase.as_deref())
                .map(|p| p == crate::status::phase::PHASE_READY)
                .unwrap_or(false);
            Ok(Some((cr.spec, ready)))
        }
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(None),
        Err(e) => Err(format!("get KarsAuthConfig/default failed: {e}")),
    }
}

/// Top-level orchestration entry point. Called from the sandbox
/// reconciler before pod-spec assembly.
///
/// Returns a [`ProvisioningOutcome`] the reconciler matches on to
/// decide whether to inject the sidecar / how to set the egress-guard
/// mode / what status condition to write.
pub async fn ensure_agent_identity_for_sandbox(
    client: &Client,
    sandbox: &KarsSandbox,
    cluster_uid: &str,
    cache: &ProvisionerCache,
) -> ProvisioningOutcome {
    let declared = sandbox
        .spec
        .mesh_auth
        .as_ref()
        .map(|m| m.mode)
        .unwrap_or(MeshAuthMode::Auto);

    // Step 1: load auth-config and resolve mode.
    let auth_config = match load_auth_config(client).await {
        Ok(c) => c,
        Err(e) => {
            return ProvisioningOutcome::Failed {
                reason: format!("load KarsAuthConfig: {e}"),
                retry_after_secs: 30,
            };
        }
    };
    let (spec, ready) = match auth_config {
        Some((s, r)) => (Some(s), r),
        None => (None, false),
    };
    let resolved = resolve_mesh_auth_mode(declared, ready && spec.is_some());

    let spec = match (resolved, spec) {
        (ResolvedMeshAuthMode::Anonymous { reason }, _) => {
            return ProvisioningOutcome::Skipped { reason };
        }
        (ResolvedMeshAuthMode::AgentId, Some(s)) => s,
        (ResolvedMeshAuthMode::AgentId, None) => {
            // Shouldn't be reachable (resolve guards this) but defend.
            return ProvisioningOutcome::Skipped {
                reason: SkipReason::AuthConfigNotReady,
            };
        }
    };
    let spec = Arc::new(spec);
    let graph = cache.get_or_init(&spec, cluster_uid).await;

    let sandbox_name = sandbox.name_any();
    let sandbox_uid = sandbox
        .metadata
        .uid
        .clone()
        .unwrap_or_else(|| sandbox_name.clone());
    let cluster_name = std::env::var("CLUSTER_NAME").unwrap_or_else(|_| "kars".to_string());

    // Step 2: if status already records an identity, verify it still
    // exists in Graph. If yes, reuse. If 404, fall through to reprovision.
    let recorded = sandbox
        .status
        .as_ref()
        .and_then(|s| s.agent_identity.as_ref());
    if let Some(recorded) = recorded {
        match graph.get_agent_identity(&recorded.object_id).await {
            Ok(Some(existing)) => {
                tracing::debug!(
                    sandbox = %sandbox_name,
                    app_id = %existing.app_id,
                    "reusing recorded agent identity"
                );
                let status = AgentIdentityStatus {
                    app_id: existing.app_id.clone(),
                    object_id: existing.id.clone(),
                    display_name: existing.display_name.clone(),
                    created_at: existing.created_date_time.clone(),
                };
                if let Err(e) =
                    materialise_sidecar_configmap(client, sandbox, &spec, &status).await
                {
                    return ProvisioningOutcome::Failed {
                        reason: format!("materialise sidecar ConfigMap: {e}"),
                        retry_after_secs: 30,
                    };
                }
                return ProvisioningOutcome::Ready {
                    agent_identity: status,
                    auth_spec: spec,
                };
            }
            Ok(None) => {
                tracing::warn!(
                    sandbox = %sandbox_name,
                    stale_object_id = %recorded.object_id,
                    "recorded agent identity no longer exists in Graph; re-provisioning"
                );
                // Fall through to step 3.
            }
            Err(e) => {
                return ProvisioningOutcome::Failed {
                    reason: format!("Graph GET recorded SP: {e}"),
                    retry_after_secs: 30,
                };
            }
        }
    }

    // Step 3: tag lookup before create — catches the crash-between-
    // -create-and-status-patch case described in the module doc.
    let existing_by_tag = match graph
        .list_cluster_agent_identities(&spec.agent_id.blueprint_client_id)
        .await
    {
        Ok(list) => list.into_iter().find(|ai| {
            ai.tags
                .iter()
                .any(|t| t == &format!("kars-sandbox-uid:{sandbox_uid}"))
        }),
        Err(e) => {
            // Non-fatal: if the list call fails we still try to create.
            // Duplicate would be caught by the reaper, but at least we
            // unblock the sandbox.
            tracing::warn!(
                sandbox = %sandbox_name,
                error = %e,
                "list_cluster_agent_identities failed; proceeding to create"
            );
            None
        }
    };

    let identity = if let Some(reuse) = existing_by_tag {
        tracing::info!(
            sandbox = %sandbox_name,
            app_id = %reuse.app_id,
            "found prior agent identity by tag (crash-recovery path); reusing"
        );
        reuse
    } else {
        match graph
            .create_agent_identity(
                &cluster_name,
                &sandbox_name,
                &sandbox_uid,
                &spec.agent_id.blueprint_client_id,
                &[], // sponsor IDs — sourced from KarsAuthConfig in follow-up
            )
            .await
        {
            Ok(ai) => ai,
            Err(e) => {
                return ProvisioningOutcome::Failed {
                    reason: format!("Graph create agent identity: {e}"),
                    retry_after_secs: 60,
                };
            }
        }
    };

    let status = AgentIdentityStatus {
        app_id: identity.app_id.clone(),
        object_id: identity.id.clone(),
        display_name: identity.display_name.clone(),
        created_at: identity.created_date_time.clone(),
    };

    // Step 4: patch sandbox.status.agentIdentity. Best-effort — if it
    // fails we still return Ready so the sandbox boots; the next
    // reconcile will retry the patch.
    if let Err(e) = patch_sandbox_status(client, sandbox, &status).await {
        tracing::warn!(
            sandbox = %sandbox_name,
            error = %e,
            "failed to patch sandbox status with agent identity; will retry next reconcile"
        );
    }

    // Step 5: materialise per-namespace sidecar env CM.
    if let Err(e) = materialise_sidecar_configmap(client, sandbox, &spec, &status).await {
        return ProvisioningOutcome::Failed {
            reason: format!("materialise sidecar ConfigMap: {e}"),
            retry_after_secs: 30,
        };
    }

    ProvisioningOutcome::Ready {
        agent_identity: status,
        auth_spec: spec,
    }
}

/// Copy the sidecar env into the sandbox's namespace.
///
/// Required because `envFrom.configMapRef` cannot cross namespaces.
/// The CR-level reconciler owns the `kars-system`-namespace copy as
/// the source of truth; we replicate it per-sandbox so the sidecar
/// container's `envFrom` works.
///
/// The replicated ConfigMap is owned by the KarsSandbox (via
/// `ownerReferences`) so K8s garbage-collects it when the sandbox
/// is deleted. No extra reaper logic required.
async fn materialise_sidecar_configmap(
    client: &Client,
    sandbox: &KarsSandbox,
    spec: &KarsAuthConfigSpec,
    _identity: &AgentIdentityStatus,
) -> Result<(), String> {
    let name = sandbox.name_any();
    let sandbox_ns = format!("kars-{name}");
    let env = render_sidecar_env(spec);

    let owner = sandbox_owner_ref(sandbox).ok_or_else(|| {
        "KarsSandbox missing uid; cannot set ownerReference on sidecar ConfigMap".to_string()
    })?;

    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    labels.insert("app.kubernetes.io/managed-by".into(), "kars".into());
    labels.insert(
        "app.kubernetes.io/component".into(),
        "auth-sidecar-env".into(),
    );
    labels.insert("kars.azure.com/sandbox".into(), name.clone());

    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(
        "kars.azure.com/blueprint-client-id".into(),
        spec.agent_id.blueprint_client_id.clone(),
    );

    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(SIDECAR_ENV_CONFIGMAP_NAME.to_string()),
            namespace: Some(sandbox_ns.clone()),
            labels: Some(labels),
            annotations: Some(annotations),
            owner_references: Some(vec![owner]),
            ..Default::default()
        },
        data: Some(env),
        ..Default::default()
    };

    let api: Api<ConfigMap> = Api::namespaced(client.clone(), &sandbox_ns);
    api.patch(
        SIDECAR_ENV_CONFIGMAP_NAME,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&cm),
    )
    .await
    .map_err(|e| format!("apply ConfigMap {sandbox_ns}/{SIDECAR_ENV_CONFIGMAP_NAME}: {e}"))?;

    Ok(())
}

fn sandbox_owner_ref(sandbox: &KarsSandbox) -> Option<OwnerReference> {
    let uid = sandbox.metadata.uid.clone()?;
    let name = sandbox.name_any();
    Some(OwnerReference {
        api_version: "kars.azure.com/v1alpha1".into(),
        kind: "KarsSandbox".into(),
        name,
        uid,
        controller: Some(true),
        block_owner_deletion: Some(true),
    })
}

async fn patch_sandbox_status(
    client: &Client,
    sandbox: &KarsSandbox,
    identity: &AgentIdentityStatus,
) -> Result<(), String> {
    let name = sandbox.name_any();
    let ns = sandbox.namespace().unwrap_or_default();
    let api: Api<KarsSandbox> = Api::namespaced(client.clone(), &ns);
    let patch = json!({
        "status": {
            "agentIdentity": {
                "appId": identity.app_id,
                "objectId": identity.object_id,
                "displayName": identity.display_name,
                "createdAt": identity.created_at,
            }
        }
    });
    api.patch_status(&name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(&patch))
        .await
        .map_err(|e| format!("patch_status {ns}/{name}: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::MeshAuthMode;

    #[test]
    fn mode_anonymous_always_resolves_anonymous() {
        let r = resolve_mesh_auth_mode(MeshAuthMode::Anonymous, true);
        assert_eq!(
            r,
            ResolvedMeshAuthMode::Anonymous {
                reason: SkipReason::ExplicitAnonymous
            }
        );
        let r = resolve_mesh_auth_mode(MeshAuthMode::Anonymous, false);
        assert_eq!(
            r,
            ResolvedMeshAuthMode::Anonymous {
                reason: SkipReason::ExplicitAnonymous
            }
        );
    }

    #[test]
    fn mode_agent_id_with_ready_config_resolves_agent_id() {
        let r = resolve_mesh_auth_mode(MeshAuthMode::AgentId, true);
        assert_eq!(r, ResolvedMeshAuthMode::AgentId);
    }

    #[test]
    fn mode_agent_id_without_ready_config_is_not_ready_not_no_config() {
        // Distinct from AutoFallbackNoConfig so operators can tell the
        // difference between "user explicitly asked for agent-id but
        // tenant isn't set up" and "auto-fallback to anonymous because
        // no config exists". Different remediation per reason.
        let r = resolve_mesh_auth_mode(MeshAuthMode::AgentId, false);
        assert_eq!(
            r,
            ResolvedMeshAuthMode::Anonymous {
                reason: SkipReason::AuthConfigNotReady
            }
        );
    }

    #[test]
    fn mode_auto_resolves_per_config_readiness() {
        let r = resolve_mesh_auth_mode(MeshAuthMode::Auto, true);
        assert_eq!(r, ResolvedMeshAuthMode::AgentId);
        let r = resolve_mesh_auth_mode(MeshAuthMode::Auto, false);
        assert_eq!(
            r,
            ResolvedMeshAuthMode::Anonymous {
                reason: SkipReason::AutoFallbackNoConfig
            }
        );
    }

    #[test]
    fn skip_reason_string_representation_is_stable() {
        // Status conditions surface these as condition reasons; pin
        // the strings so existing dashboards/alerts don't silently
        // break.
        assert_eq!(SkipReason::ExplicitAnonymous.as_str(), "ExplicitAnonymous");
        assert_eq!(SkipReason::AutoFallbackNoConfig.as_str(), "AutoFallbackNoConfig");
        assert_eq!(SkipReason::AuthConfigNotReady.as_str(), "AuthConfigNotReady");
    }

    #[tokio::test]
    async fn provisioner_cache_returns_same_client_for_same_blueprint() {
        let cache = ProvisionerCache::new();
        let spec = KarsAuthConfigSpec {
            tenant: crate::auth_config::TenantConfig {
                tenant_id: "t".into(),
                authority_host: "https://login.microsoftonline.com/".into(),
                service_management_reference: None,
            },
            agent_id: crate::auth_config::AgentIdConfig {
                blueprint_client_id: "blueprint-1".into(),
                blueprint_object_id: "obj-1".into(),
            },
            controller: crate::auth_config::ControllerIdentityConfig {
                managed_identity_client_id: "mi-c".into(),
                managed_identity_resource_id: "mi-r".into(),
                managed_identity_principal_id: Some("mi-p".into()),
            },
            downstream_apis: Default::default(),
        };
        let c1 = cache.get_or_init(&spec, "cluster-1").await;
        let c2 = cache.get_or_init(&spec, "cluster-1").await;
        assert!(Arc::ptr_eq(&c1, &c2));
    }
}
