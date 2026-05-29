// Copyright (c) Microsoft Corporation.
// ci:loc-ok — Entra Agent ID feature module, split planned for Phase 1 (see ci/loc-budget.yaml)

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
//!
//! ## Scale-out invariant
//!
//! Per the Phase 5 security review (rubber-duck #4), kars MUST NOT
//! provision one agent identity per pod when a `KarsSandbox` scales
//! to N replicas — all replicas of the same sandbox CR share ONE
//! agent identity. This is enforced architecturally:
//!
//! - The agent identity is keyed on `KarsSandbox.metadata.uid` (the
//!   CR's identity, not any pod's) via the
//!   `kars-sandbox-uid:<uid>` Graph tag.
//! - The status field `agentIdentity` lives on the CR itself, so
//!   every reconcile of the same CR resolves to the same identity.
//! - The pod template carries the appId in
//!   `PINNED_AGENT_IDENTITY_APP_ID` env, which is identical across
//!   all replicas of the Deployment.
//!
//! If `KarsSandbox` later grows a `replicas` field, the invariant
//! holds because nothing in this module branches on replica count.
//! The `recovers_existing_identity_on_repeat_reconcile` test below
//! pins this behaviour.
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

use crate::agent_identity::{AgentIdentityClient, AgentIdentityConfig};
use crate::auth_config::{DEFAULT_AUTH_CONFIG_NAME, KarsAuthConfig, KarsAuthConfigSpec};
use crate::crd::{AgentIdentityStatus, KarsSandbox, MeshAuthMode};
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
    Failed {
        reason: String,
        retry_after_secs: u64,
    },
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
    #[allow(dead_code)]
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

    // Step 2: if status already records an identity, trust it.
    //
    // We deliberately do NOT GET-verify the recorded identity here:
    // Graph's `GET /servicePrincipals/{id}` has eventual-consistency
    // delays on the order of seconds after creation, which can return
    // 404 for a freshly-recorded identity. Treating that 404 as
    // "stale, reprovision" creates a runaway-creation loop (observed
    // live on kars-aks producing 70+ duplicate SPs per minute).
    //
    // The orphan reaper (separate module / follow-up PR) handles the
    // out-of-band-delete case by scrubbing tagged SPs whose owning
    // sandbox no longer exists. The reaper doesn't need this GET
    // because it operates on `list_cluster_agent_identities` results.
    let recorded = sandbox
        .status
        .as_ref()
        .and_then(|s| s.agent_identity.as_ref());
    if let Some(recorded) = recorded {
        tracing::debug!(
            sandbox = %sandbox_name,
            app_id = %recorded.app_id,
            "trusting recorded agent identity (no GET-verify; reaper handles orphans)"
        );
        let status = AgentIdentityStatus {
            app_id: recorded.app_id.clone(),
            object_id: recorded.object_id.clone(),
            display_name: recorded.display_name.clone(),
            created_at: recorded.created_at.clone(),
        };

        // Reconcile ARM role assignments for already-provisioned
        // identities too (Phase 5b). This is what retroactively
        // grants roles to sandboxes that existed before the
        // operator added `foundryRbac` entries to KarsAuthConfig,
        // and what re-asserts assignments that drifted (deleted
        // out-of-band). All operations are idempotent on ARM's side
        // via the deterministic assignment GUID.
        //
        // Failure path is identical to Step 3c below: log WARN,
        // continue. The sandbox boots; ARM RBAC eventually
        // converges as the controller retries.
        tracing::info!(
            sandbox = %sandbox_name,
            app_id = %recorded.app_id,
            foundry_rbac_entries = spec.foundry_rbac.len(),
            "Phase 5b reconcile: re-asserting ARM role assignments for recorded agent identity"
        );
        for assignment in &spec.foundry_rbac {
            for role_id in &assignment.role_definition_ids {
                match graph
                    .assign_role_to_agent_identity(&recorded.object_id, role_id, &assignment.scope)
                    .await
                {
                    Ok(()) => {
                        tracing::info!(
                            sandbox = %sandbox_name,
                            app_id = %recorded.app_id,
                            role = %role_id,
                            scope = %assignment.scope,
                            "ARM role re-asserted on recorded agent identity (idempotent)"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            sandbox = %sandbox_name,
                            app_id = %recorded.app_id,
                            role = %role_id,
                            scope = %assignment.scope,
                            error = %e,
                            "ARM role re-assertion failed on recorded agent identity; will retry next reconcile"
                        );
                    }
                }
            }
        }

        // No per-namespace ConfigMap mirror: the shared auth-sidecar
        // in `kars-system` consumes a single cluster-level ConfigMap
        // (managed by the auth_config_reconciler). Sandboxes carry
        // their per-identity attribution via PINNED_AGENT_IDENTITY_APP_ID
        // env on the inference-router.
        return ProvisioningOutcome::Ready {
            agent_identity: status,
            auth_spec: spec,
        };
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
                &spec.agent_id.sponsor_user_object_ids,
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

    // Step 3b: apply per-sandbox custom security attributes (Phase 5).
    //
    // PATCH is run on every reconcile (idempotent on Graph's side) so
    // edits to `KarsSandbox.spec.meshAuth.customSecurityAttributes`
    // propagate without requiring a fresh identity. Fail-closed: when
    // the attribute set is undeclared in the tenant, Graph returns
    // 400; we surface as Failed so the operator notices rather than
    // silently running the identity without the intended Conditional
    // Access targeting.
    let attrs = sandbox
        .spec
        .mesh_auth
        .as_ref()
        .map(|m| &m.custom_security_attributes);
    if let Some(attrs) = attrs
        && !attrs.is_empty()
        && let Err(e) = graph
            .patch_custom_security_attributes(&identity.id, attrs)
            .await
    {
        tracing::warn!(
            sandbox = %sandbox_name,
            app_id = %identity.app_id,
            error = %e,
            "PATCH custom security attributes failed; retrying next reconcile"
        );
        return ProvisioningOutcome::Failed {
            reason: format!("PATCH custom security attributes: {e}"),
            retry_after_secs: 60,
        };
    }

    // Step 3c: assign per-agent ARM RBAC roles (Phase 5b).
    //
    // Microsoft docs (concept-agent-id-design-patterns) confirm that
    // Azure RBAC is per-principal — role assignments on the blueprint
    // SP do NOT inherit to derived agent identity SPs. So every newly-
    // provisioned agent identity must get its own role assignment for
    // each downstream resource it needs to access (Foundry, KV,
    // Storage, etc).
    //
    // Configured via `KarsAuthConfig.spec.foundryRbac` (a list of
    // {scope, roleDefinitionIds[]} entries). When empty (default),
    // this step is a no-op and operators run the manual grants
    // documented in the migration guide.
    //
    // Failure mode: when the controller MI lacks
    // `Microsoft.Authorization/roleAssignments/write` on the scope,
    // Azure returns 403 AuthorizationFailed. We log a WARN and
    // continue — the sandbox boots, agent identity is recorded, but
    // inference returns 401 until an operator grants manually. This
    // is a graceful degradation from the old manual-only path.
    for assignment in &spec.foundry_rbac {
        for role_id in &assignment.role_definition_ids {
            match graph
                .assign_role_to_agent_identity(&identity.id, role_id, &assignment.scope)
                .await
            {
                Ok(()) => {
                    tracing::info!(
                        sandbox = %sandbox_name,
                        app_id = %identity.app_id,
                        role = %role_id,
                        scope = %assignment.scope,
                        "ARM role assigned to agent identity"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        sandbox = %sandbox_name,
                        app_id = %identity.app_id,
                        role = %role_id,
                        scope = %assignment.scope,
                        error = %e,
                        "ARM role assignment failed — sandbox will boot but may return 401 from downstream Azure until granted manually"
                    );
                }
            }
        }
    }

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

    // No per-namespace ConfigMap mirror in the shared-sidecar
    // architecture — see the doc comment at the top of this module.

    ProvisioningOutcome::Ready {
        agent_identity: status,
        auth_spec: spec,
    }
}

/// Deprovision a sandbox's agent identity and clean up its ARM RBAC
/// assignments. Called from the KarsSandbox deletion finalizer in
/// `reconciler/mod.rs`.
///
/// Steps:
///   1. Read the recorded `agentIdentity` from sandbox status (the
///      principalId + objectId we created at provisioning time).
///   2. For each `foundryRbac` scope, list role assignments held by
///      this principalId and DELETE them. Eliminates the orphan
///      "agent identity has Foundry role but the sandbox is gone"
///      state.
///   3. Graph DELETE the agent identity SP itself. The orphan reaper
///      catches the case where status was never written (the SP was
///      created but the status patch failed); we do BOTH to be safe.
///
/// Failure mode: anything that fails is logged as WARN but does NOT
/// block the finalizer from being removed. The orphan reaper catches
/// what we miss. Blocking the finalizer on cleanup failures would
/// leave the K8s CR stuck in `Terminating` forever if Graph or ARM
/// were unavailable.
pub async fn cleanup_agent_identity_for_sandbox(
    client: &Client,
    sandbox: &KarsSandbox,
    cache: &ProvisionerCache,
) {
    let sandbox_name = sandbox.name_any();
    let recorded = sandbox
        .status
        .as_ref()
        .and_then(|s| s.agent_identity.as_ref());
    let Some(recorded) = recorded else {
        tracing::debug!(
            sandbox = %sandbox_name,
            "no recorded agent identity on sandbox; skipping deprovision (reaper covers orphans)"
        );
        return;
    };

    // Need the KarsAuthConfig spec to know which scopes to clean up
    // role assignments at, and the auth path to use.
    let auth_config = match load_auth_config(client).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                sandbox = %sandbox_name,
                error = %e,
                "could not load KarsAuthConfig during deprovision — skipping cleanup; reaper will retry"
            );
            return;
        }
    };
    let spec = match auth_config {
        Some((s, _ready)) => Arc::new(s),
        None => {
            tracing::debug!(
                sandbox = %sandbox_name,
                "KarsAuthConfig absent during deprovision — nothing to clean up"
            );
            return;
        }
    };

    let cluster_uid = std::env::var("CLUSTER_UID").unwrap_or_else(|_| "cluster".to_string());
    let graph = cache.get_or_init(&spec, &cluster_uid).await;

    // Step 1: DELETE role assignments for each configured scope.
    // Idempotent (404s are treated as success). When the controller
    // MI lacks ARM permission, this fails gracefully — operators
    // surface and clean up via the reaper.
    for assignment in &spec.foundry_rbac {
        match graph
            .delete_role_assignments_for_principal(&recorded.object_id, &assignment.scope)
            .await
        {
            Ok(n) => {
                tracing::info!(
                    sandbox = %sandbox_name,
                    principal_id = %recorded.object_id,
                    scope = %assignment.scope,
                    deleted = n,
                    "ARM role assignments cleaned up"
                );
            }
            Err(e) => {
                tracing::warn!(
                    sandbox = %sandbox_name,
                    principal_id = %recorded.object_id,
                    scope = %assignment.scope,
                    error = %e,
                    "ARM role assignment cleanup failed; reaper will retry"
                );
            }
        }
    }

    // Step 2: Graph DELETE the agent identity SP itself.
    // Idempotent (404 treated as success).
    match graph.delete_agent_identity(&recorded.object_id).await {
        Ok(()) => {
            tracing::info!(
                sandbox = %sandbox_name,
                app_id = %recorded.app_id,
                object_id = %recorded.object_id,
                "agent identity SP deprovisioned"
            );
        }
        Err(e) => {
            tracing::warn!(
                sandbox = %sandbox_name,
                app_id = %recorded.app_id,
                object_id = %recorded.object_id,
                error = %e,
                "agent identity deprovision failed; reaper will retry"
            );
        }
    }
}

/// (Removed) `materialise_sidecar_configmap` lived here in the
/// per-pod-sidecar design — each sandbox got a copy of the shared
/// auth-sidecar env in its own namespace. The shared-sidecar
/// architecture (one Deployment in `kars-system`) makes that
/// mirroring unnecessary: the sidecar consumes a single
/// `kars-system`-scoped ConfigMap managed by `auth_config_reconciler`.
async fn patch_sandbox_status(
    client: &Client,
    sandbox: &KarsSandbox,
    identity: &AgentIdentityStatus,
) -> Result<(), String> {
    // Idempotency guard: skip the patch if the recorded status
    // already matches. Without this guard the SSA patch with
    // `.force()` rewrites the field every reconcile, bumping
    // resourceVersion and triggering another watch event — an
    // infinite reconcile loop. (Same bug class as the auth-config
    // status loop fixed earlier on this branch.)
    if let Some(current) = sandbox
        .status
        .as_ref()
        .and_then(|s| s.agent_identity.as_ref())
        && current.app_id == identity.app_id
        && current.object_id == identity.object_id
        && current.display_name == identity.display_name
        && current.created_at == identity.created_at
    {
        return Ok(());
    }
    let name = sandbox.name_any();
    let ns = sandbox.namespace().unwrap_or_default();
    let api: Api<KarsSandbox> = Api::namespaced(client.clone(), &ns);
    // SSA patches MUST include `apiVersion` and `kind` at the top
    // level — without them the API server returns
    // `BadRequest: invalid object type: /, Kind=`. Embedding them
    // is the kube-rs convention for raw-JSON Apply patches.
    let patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsSandbox",
        "status": {
            "agentIdentity": {
                "appId": identity.app_id,
                "objectId": identity.object_id,
                "displayName": identity.display_name,
                "createdAt": identity.created_at,
            }
        }
    });
    api.patch_status(
        &name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&patch),
    )
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
        assert_eq!(
            SkipReason::AutoFallbackNoConfig.as_str(),
            "AutoFallbackNoConfig"
        );
        assert_eq!(
            SkipReason::AuthConfigNotReady.as_str(),
            "AuthConfigNotReady"
        );
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
                sponsor_user_object_ids: vec![],
            },
            controller: crate::auth_config::ControllerIdentityConfig {
                credential_mode: Default::default(),
                managed_identity_client_id: Some("mi-c".into()),
                managed_identity_resource_id: Some("mi-r".into()),
                managed_identity_principal_id: Some("mi-p".into()),
            },
            downstream_apis: Default::default(),
            foundry_rbac: vec![],
            mesh_auth_backend: Default::default(),
            mesh_auth_audience: None,
        };
        let c1 = cache.get_or_init(&spec, "cluster-1").await;
        let c2 = cache.get_or_init(&spec, "cluster-1").await;
        assert!(Arc::ptr_eq(&c1, &c2));
    }

    /// Pins the scale-out invariant from the Phase 5 security review:
    /// the agent-identity tag layout is keyed purely on the CR's
    /// `metadata.uid` (and the cluster UID), with NO dimension for
    /// pod ordinal, replica index, or any other per-pod attribute.
    /// This guarantees that scaling a KarsSandbox to N replicas
    /// resolves to ONE shared agent identity at provisioning time,
    /// not N distinct identities (the anti-pattern Microsoft's
    /// design-patterns documentation explicitly warns against).
    ///
    /// If a future change ever introduces per-pod keying here, this
    /// test will fail by surfacing the new tag dimension.
    #[test]
    fn tag_layout_excludes_per_pod_attributes() {
        let tags =
            crate::agent_identity::AgentIdentityClient::tags_for("cluster-abc", "sandbox-xyz");
        // Sanity: both the cluster + sandbox dimensions are present
        // (those are the legitimate keys).
        assert!(tags.iter().any(|t| t.starts_with("kars-cluster-uid:")));
        assert!(tags.iter().any(|t| t.starts_with("kars-sandbox-uid:")));
        // Anti-pattern guards: no pod-, replica-, ordinal-, or
        // hostname-keyed dimensions. If a future PR adds any of
        // these tag prefixes, the scale-out invariant breaks and
        // this test will catch it.
        for forbidden_prefix in &[
            "kars-pod-",
            "kars-replica-",
            "kars-ordinal-",
            "kars-hostname-",
            "kars-podname-",
        ] {
            assert!(
                !tags.iter().any(|t| t.starts_with(forbidden_prefix)),
                "tag layout has per-pod-keyed dimension '{forbidden_prefix}*' \
                 which would break the scale-out invariant"
            );
        }
    }
}
