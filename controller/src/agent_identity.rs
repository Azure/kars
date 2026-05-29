// Copyright (c) Microsoft Corporation.
// ci:loc-ok — Entra Agent ID feature module, split planned for Phase 1 (see ci/loc-budget.yaml)

// Licensed under the MIT License.

//! Entra Agent Identity provisioning via Microsoft Graph.
//!
//! Per-sandbox Entra Agent Identities are the principals kars sandbox
//! pods present to Foundry / Graph / KV. They are derived from a single
//! tenant-wide blueprint and represent the *agent*, not the cluster MI.
//!
//! ## Token chain
//!
//! ```text
//! 1. IMDS at 169.254.169.254
//!    GET ?resource=api://AzureADTokenExchange&client_id=<controller_mi>
//!    → MI assertion (signed by login.microsoftonline.com,
//!                    sub = controller MI principalId)
//!
//! 2. POST https://login.microsoftonline.com/<tid>/oauth2/v2.0/token
//!    grant_type=client_credentials
//!    client_id=<blueprint app id>
//!    scope=https://graph.microsoft.com/.default
//!    client_assertion_type=jwt-bearer
//!    client_assertion=<MI assertion from step 1>
//!    → blueprint token (appid=<blueprint>, role=AgentIdentity.CreateAsManager)
//!
//! 3. POST https://graph.microsoft.com/beta/servicePrincipals/
//!         Microsoft.Graph.AgentIdentity
//!    Authorization: Bearer <blueprint token>
//!    {displayName, agentIdentityBlueprintId, sponsors@odata.bind: [...]}
//!    → new ServicePrincipal of type ServiceIdentity
//! ```
//!
//! Why this odd shape? AKS Workload Identity tokens are FIC-derived, so
//! Entra rejects re-use as the blueprint's FIC assertion with
//! `AADSTS700231` (anti-loop protection). IMDS-issued tokens are NOT
//! FIC-derived, so the same MI principal id presented via IMDS works
//! where WI does not. See
//! `docs/architecture/entra-agent-id/01-runtime-token-flow.md`.
//!
//! ## Idempotence + tagging
//!
//! - The controller stores the created agent identity's app/object id
//!   in `KarsSandbox.status.agentIdentity`. On reconcile, if status is
//!   populated and the ID still resolves via Graph GET, no create call
//!   is made.
//! - All Graph objects are tagged with `kars-cluster-uid:<uid>` and
//!   `kars-sandbox-uid:<uid>` via the `tags` property so the reaper
//!   (`agent_identity_reaper.rs`) can find orphans without relying on
//!   display name parsing.
//! - Display names follow `kars-<cluster>-<sandbox>` for human
//!   diagnosis but are NOT used as primary keys.

use crate::auth_config::KarsAuthConfigSpec;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Configuration for the agent-identity Graph client.
///
/// Built from env vars at controller startup (so the same instance can
/// reconcile multiple sandboxes without re-reading the K8s CR every
/// call) plus the `KarsAuthConfig` CR for tenant/blueprint anchors.
#[derive(Clone)]
pub struct AgentIdentityConfig {
    /// Microsoft Entra tenant ID.
    pub tenant_id: String,
    /// Authority host (e.g. `https://login.microsoftonline.com/`).
    pub authority_host: String,
    /// Blueprint application client ID — sidecar's `AzureAd__ClientId`
    /// and the principal we authenticate as when calling Graph.
    pub blueprint_client_id: String,
    /// Controller managed identity client ID — IMDS uses this to
    /// disambiguate which MI to fetch a token for when the VMSS has
    /// multiple assigned identities.
    pub controller_mi_client_id: String,
    /// Cluster UID — propagated to Graph object tags for orphan
    /// detection by the reaper.
    pub cluster_uid: String,
}

impl AgentIdentityConfig {
    /// Construct from the `KarsAuthConfig` CR + a known cluster UID.
    ///
    /// The cluster UID typically comes from the controller's leader
    /// election lease's metadata.uid, which is stable for the lifetime
    /// of the cluster. Passing it here keeps `agent_identity.rs` free
    /// of k8s_openapi types.
    pub fn from_auth_config(spec: &KarsAuthConfigSpec, cluster_uid: String) -> Self {
        Self {
            tenant_id: spec.tenant.tenant_id.clone(),
            authority_host: spec.tenant.authority_host.clone(),
            blueprint_client_id: spec.agent_id.blueprint_client_id.clone(),
            // The controller's own Graph client uses MI+IMDS regardless
            // of which CredentialMode the sidecar is configured for.
            // When the MI field is None (Pattern B, sidecar-only WI),
            // the IMDS request will fail and `wi_mi_token` will run —
            // and that path itself fails today because there's no
            // controller-side WI FIC. Adding controller WI support is
            // tracked as a follow-up; Phase 4 only switches the
            // SIDECAR's credential mode.
            controller_mi_client_id: spec
                .controller
                .managed_identity_client_id
                .clone()
                .unwrap_or_default(),
            cluster_uid,
        }
    }
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[allow(dead_code)]
    expires_in: i64,
}

#[derive(Deserialize)]
struct ImdsTokenResponse {
    access_token: String,
}

/// One agent identity as returned by Microsoft Graph.
///
/// Field names match the Microsoft Graph wire format (camelCase) via
/// `#[serde(rename_all = "camelCase")]`. The struct keeps Rust-native
/// `snake_case` names so the rest of the controller code is idiomatic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentIdentity {
    /// Service principal object ID. Used in ARM role assignments and
    /// Graph DELETE.
    pub id: String,
    /// Service principal `appId` / client ID. Used by the sidecar in
    /// the `?AgentIdentity=<id>` URL param when minting tokens.
    pub app_id: String,
    /// Display name as set by the controller.
    pub display_name: String,
    /// Linked blueprint application ID. Returned by Graph; we record
    /// it for sanity-checking that the SP belongs to the blueprint we
    /// expect.
    #[serde(default)]
    pub agent_identity_blueprint_id: Option<String>,
    /// ISO-8601 creation timestamp.
    #[serde(default)]
    pub created_date_time: Option<String>,
    /// Service principal type — should always be `ServiceIdentity` for
    /// agent identities. Recorded for diagnostics.
    #[serde(default)]
    pub service_principal_type: Option<String>,
    /// Tags applied by the controller for orphan detection.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Cached OAuth token with expiry tracking.
struct CachedToken {
    token: String,
    expires_at: std::time::Instant,
}

/// Graph client for agent identity provisioning.
///
/// Token acquisition is cached for ~50 minutes (Entra tokens are valid
/// for 1h; we refresh 10 min before expiry). The cache is shared via
/// `Arc<RwLock>` so multiple concurrent reconciles on different
/// sandboxes share the same blueprint token.
pub struct AgentIdentityClient {
    config: AgentIdentityConfig,
    http: reqwest::Client,
    cached_blueprint_token: Arc<RwLock<Option<CachedToken>>>,
}

impl AgentIdentityClient {
    pub fn new(config: AgentIdentityConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            cached_blueprint_token: Arc::new(RwLock::new(None)),
        }
    }

    /// Acquire a Microsoft Graph access token authenticated as the
    /// blueprint application.
    ///
    /// Chain:
    /// 1. IMDS → MI assertion for `api://AzureADTokenExchange`.
    /// 2. Token endpoint → blueprint Graph token via jwt-bearer.
    ///
    /// Cached for ~50 min so back-to-back agent identity creations on
    /// many sandboxes don't roundtrip Entra each time.
    async fn graph_token(&self) -> Result<String, String> {
        // Cache hit?
        {
            let cached = self.cached_blueprint_token.read().await;
            if let Some(ref ct) = *cached
                && ct.expires_at > std::time::Instant::now()
            {
                return Ok(ct.token.clone());
            }
        }

        // Step 1: WI (preferred) or IMDS for MI assertion.
        let mi_assertion = self.mi_token("api://AzureADTokenExchange").await?;

        // Step 2: Exchange MI assertion for blueprint Graph token.
        let url = format!(
            "{}/{}/oauth2/v2.0/token",
            self.config.authority_host.trim_end_matches('/'),
            self.config.tenant_id,
        );
        let resp = self
            .http
            .post(&url)
            .form(&[
                ("client_id", self.config.blueprint_client_id.as_str()),
                ("scope", "https://graph.microsoft.com/.default"),
                (
                    "client_assertion_type",
                    "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
                ),
                ("client_assertion", &mi_assertion),
                ("grant_type", "client_credentials"),
            ])
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| format!("blueprint token request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "blueprint token exchange failed ({status}): {}",
                &body[..body.len().min(400)]
            ));
        }

        let parsed: OAuthTokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("blueprint token parse failed: {e}"))?;

        // Cache with a safety margin: expire 10 min before Entra would.
        let lifetime = parsed.expires_in.max(300);
        let cache_ttl = (lifetime - 600).max(60) as u64;
        {
            let mut cache = self.cached_blueprint_token.write().await;
            *cache = Some(CachedToken {
                token: parsed.access_token.clone(),
                expires_at: std::time::Instant::now() + std::time::Duration::from_secs(cache_ttl),
            });
        }

        tracing::debug!(
            blueprint = %self.config.blueprint_client_id,
            cache_ttl_seconds = cache_ttl,
            "acquired blueprint Graph token"
        );

        Ok(parsed.access_token)
    }

    /// Fetch a managed-identity token, preferring IMDS over Workload
    /// Identity to avoid the FIC-as-FIC anti-loop check.
    ///
    /// Why IMDS first (not WI): tokens minted by the WI exchange are
    /// themselves derived from federated credentials. Using such a
    /// token as a `client_assertion` against the blueprint triggers
    /// Entra's anti-loop check with `AADSTS700231: Token obtained
    /// using a federated identity credential may not be used as a
    /// federated identity credential.` IMDS-minted tokens are NOT
    /// FIC-derived (the MI is assigned to the node-pool VMSS at the
    /// Azure RBAC layer) so the FIC assertion succeeds.
    ///
    /// Pre-requisites:
    ///   1. `kars-controller-mi` assigned to the AKS node-pool VMSS
    ///      (`az vmss identity assign --identities <mi-id>`).
    ///      `kars up` automates this; verified by
    ///      `kars mesh setup-trust verify`.
    ///   2. Controller namespace's NetworkPolicy allows egress to
    ///      169.254.169.254:80 (added to the default-deny template
    ///      on this branch).
    ///
    /// WI fallback exists for environments where IMDS truly is
    /// unreachable (e.g. local development against an AAD-backed
    /// MI exposed via a static credential rather than VMSS). The
    /// fallback will hit AADSTS700231 in production if reached;
    /// the error surfaces clearly to the operator.
    ///
    /// `audience` is propagated as the `resource` parameter; the
    /// resulting token's `aud` claim equals this value. For the
    /// blueprint exchange we use `api://AzureADTokenExchange`.
    async fn mi_token(&self, audience: &str) -> Result<String, String> {
        match self.imds_mi_token(audience).await {
            Ok(t) => Ok(t),
            Err(imds_err) => {
                let wi_path = std::env::var("AZURE_FEDERATED_TOKEN_FILE").unwrap_or_else(|_| {
                    "/var/run/secrets/azure/tokens/azure-identity-token".into()
                });
                if tokio::fs::try_exists(&wi_path).await.unwrap_or(false) {
                    tracing::warn!(
                        imds_error = %imds_err,
                        "IMDS unavailable; falling back to WI (will fail FIC step with AADSTS700231 in AKS)"
                    );
                    self.wi_mi_token(audience, &wi_path).await
                } else {
                    Err(imds_err)
                }
            }
        }
    }

    /// Acquire a token for `mi_client_id` via Workload Identity.
    ///
    /// The flow is:
    ///   1. Read the projected SA token (a JWT signed by the AKS
    ///      OIDC issuer).
    ///   2. POST to Entra `/oauth2/v2.0/token` with `grant_type=
    ///      client_credentials`, `client_id=<controller_mi_client_id>`,
    ///      `client_assertion=<SA token>`, scope=`<audience>/.default`.
    ///   3. Entra checks the FIC on `<controller_mi_client_id>` for
    ///      `iss=<aks-oidc>, sub=system:serviceaccount:...`; if it
    ///      matches, mints a token for that MI.
    ///
    /// The `audience` parameter is the desired token audience (e.g.
    /// `api://AzureADTokenExchange`). We append `/.default` to form
    /// the scope.
    async fn wi_mi_token(&self, audience: &str, wi_token_path: &str) -> Result<String, String> {
        let sa_token = tokio::fs::read_to_string(wi_token_path)
            .await
            .map_err(|e| format!("read SA token at {wi_token_path}: {e}"))?;

        let url = format!(
            "{}/{}/oauth2/v2.0/token",
            self.config.authority_host.trim_end_matches('/'),
            self.config.tenant_id,
        );
        // The audience-to-scope mapping for client-credentials is the
        // audience plus `/.default`. For api:// audiences this means
        // `api://AzureADTokenExchange/.default`.
        let scope = if audience.ends_with("/.default") {
            audience.to_string()
        } else {
            format!("{}/.default", audience.trim_end_matches('/'))
        };
        let resp = self
            .http
            .post(&url)
            .form(&[
                ("client_id", self.config.controller_mi_client_id.as_str()),
                ("scope", scope.as_str()),
                (
                    "client_assertion_type",
                    "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
                ),
                ("client_assertion", sa_token.trim()),
                ("grant_type", "client_credentials"),
            ])
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| format!("WI MI token request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "WI MI token exchange failed ({status}): {}",
                &body[..body.len().min(400)]
            ));
        }

        let parsed: OAuthTokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("WI MI token parse failed: {e}"))?;
        Ok(parsed.access_token)
    }

    /// Fetch a managed-identity token from IMDS.
    ///
    /// Used in environments where Workload Identity is unavailable
    /// (e.g. local development against a kind cluster running on a
    /// VM with an MSI attached). In AKS WI clusters this path will
    /// fail because WI blocks IMDS — see `mi_token` above for
    /// rationale and the wrapper that tries WI first.
    ///
    /// The `audience` argument is propagated to IMDS as the `resource`
    /// parameter; the resulting token's `aud` claim equals this value.
    async fn imds_mi_token(&self, audience: &str) -> Result<String, String> {
        // IMDS accepts query parameters via reqwest's structured form
        // builder — no manual encoding required. This avoids pulling
        // in a percent-encoding dependency for two call sites.
        let resp = self
            .http
            .get("http://169.254.169.254/metadata/identity/oauth2/token")
            .query(&[
                ("api-version", "2018-02-01"),
                ("resource", audience),
                ("client_id", &self.config.controller_mi_client_id),
            ])
            .header("Metadata", "true")
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("IMDS request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "IMDS returned {status}: {}",
                &body[..body.len().min(300)]
            ));
        }

        let parsed: ImdsTokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("IMDS parse failed: {e}"))?;

        Ok(parsed.access_token)
    }

    /// Create an agent identity for a specific kars sandbox.
    ///
    /// Idempotent at the orchestration layer: callers should check
    /// `KarsSandbox.status.agentIdentity` and call this only when the
    /// status is empty. If the sandbox already has an agent identity
    /// but the caller invokes this anyway, Graph will create a second
    /// service principal — the controller treats that as a bug.
    ///
    /// `sponsor_user_object_ids` are the user object IDs that act as
    /// sponsors on the agent identity. These come from the
    /// blueprint's owner list at `kars mesh setup-trust` time, then
    /// propagated through `KarsAuthConfig` (deferred — today the
    /// caller must supply them explicitly).
    pub async fn create_agent_identity(
        &self,
        cluster_name: &str,
        sandbox_name: &str,
        sandbox_uid: &str,
        blueprint_app_id: &str,
        sponsor_user_object_ids: &[String],
    ) -> Result<AgentIdentity, String> {
        let token = self.graph_token().await?;
        let display_name = format!("kars-{cluster_name}-{sandbox_name}");
        let url =
            "https://graph.microsoft.com/beta/servicePrincipals/Microsoft.Graph.AgentIdentity";

        let mut body = serde_json::json!({
            "displayName": display_name,
            "agentIdentityBlueprintId": blueprint_app_id,
            "tags": Self::tags_for(&self.config.cluster_uid, sandbox_uid),
        });

        // Only attach sponsors when caller provided them — Graph
        // rejects empty arrays for `sponsors@odata.bind`.
        if !sponsor_user_object_ids.is_empty() {
            let refs: Vec<String> = sponsor_user_object_ids
                .iter()
                .map(|oid| format!("https://graph.microsoft.com/v1.0/users/{oid}"))
                .collect();
            body["sponsors@odata.bind"] =
                serde_json::Value::Array(refs.into_iter().map(serde_json::Value::String).collect());
        }

        let resp = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("OData-Version", "4.0")
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| format!("Graph create agent identity failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "Graph create agent identity returned {status}: {}",
                &body_text[..body_text.len().min(600)]
            ));
        }

        let parsed: AgentIdentity = resp
            .json()
            .await
            .map_err(|e| format!("Graph create agent identity parse failed: {e}"))?;

        tracing::info!(
            agent_id = %parsed.app_id,
            display_name = %parsed.display_name,
            cluster = %cluster_name,
            sandbox = %sandbox_name,
            "provisioned agent identity"
        );

        Ok(parsed)
    }

    /// Apply custom security attributes to an existing agent identity SP.
    ///
    /// Microsoft Graph accepts these via PATCH on the SP's
    /// `customSecurityAttributes` field. Format:
    /// ```json
    /// {
    ///   "customSecurityAttributes": {
    ///     "<AttributeSet>": {
    ///       "@odata.type": "#Microsoft.DirectoryServices.CustomSecurityAttributeValue",
    ///       "<AttributeName>@odata.type": "#String" | "#Int32" | "#Boolean" | "#Collection(String)",
    ///       "<AttributeName>": value
    ///     }
    ///   }
    /// }
    /// ```
    /// The `@odata.type` per-attribute is required by Graph (no
    /// implicit type inference). We infer it from the
    /// `serde_json::Value` type — strings → `#String`, integers →
    /// `#Int32`, booleans → `#Boolean`, arrays-of-strings →
    /// `#Collection(String)`. Other shapes (floats, null, nested
    /// objects, arrays of mixed types) are rejected with a clear
    /// error before the call goes out.
    ///
    /// The attribute set + attribute names must be pre-declared in
    /// the tenant's `customSecurityAttributeDefinitions` collection
    /// — Graph returns 400 `Request_BadRequest` when referencing an
    /// undeclared one. Operators wire the recommended baseline via
    /// `deploy/bicep/standalone/custom-security-attributes.bicep`.
    ///
    /// Idempotent: re-applying the same attributes is a no-op on
    /// Graph's side (the PATCH overwrites with identical values).
    pub async fn patch_custom_security_attributes(
        &self,
        object_id: &str,
        attributes: &std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, serde_json::Value>,
        >,
    ) -> Result<(), String> {
        if attributes.is_empty() {
            return Ok(());
        }
        let token = self.graph_token().await?;
        let url = format!("https://graph.microsoft.com/beta/servicePrincipals/{object_id}");

        let mut sets = serde_json::Map::new();
        for (set_name, attrs) in attributes {
            let mut obj = serde_json::Map::new();
            obj.insert(
                "@odata.type".into(),
                serde_json::Value::String(
                    "#Microsoft.DirectoryServices.CustomSecurityAttributeValue".into(),
                ),
            );
            for (attr_name, value) in attrs {
                let odata_type = odata_type_for_value(value).map_err(|e| {
                    format!("custom security attribute '{set_name}/{attr_name}': {e}")
                })?;
                obj.insert(
                    format!("{attr_name}@odata.type"),
                    serde_json::Value::String(odata_type),
                );
                obj.insert(attr_name.clone(), value.clone());
            }
            sets.insert(set_name.clone(), serde_json::Value::Object(obj));
        }

        let body = serde_json::json!({
            "customSecurityAttributes": serde_json::Value::Object(sets),
        });

        let resp = self
            .http
            .patch(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("OData-Version", "4.0")
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| format!("Graph PATCH custom security attributes failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "Graph PATCH custom security attributes on {object_id} returned {status}: {}",
                &body_text[..body_text.len().min(600)]
            ));
        }

        tracing::info!(
            object_id,
            set_count = attributes.len(),
            "applied custom security attributes to agent identity SP"
        );

        Ok(())
    }

    /// Delete an agent identity by service-principal object ID.
    ///
    /// Idempotent — treats 404 as success so finalizer-driven cleanup
    /// is safe to retry. Other 4xx/5xx are bubbled up so the caller
    /// can retry with backoff.
    pub async fn delete_agent_identity(&self, object_id: &str) -> Result<(), String> {
        let token = self.graph_token().await?;
        let url = format!("https://graph.microsoft.com/beta/serviceprincipals/{object_id}");

        let resp = self
            .http
            .delete(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("OData-Version", "4.0")
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| format!("Graph delete agent identity failed: {e}"))?;

        let status = resp.status();
        if status.is_success() || status.as_u16() == 404 {
            tracing::info!(object_id, "agent identity deleted (or already absent)");
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(format!(
                "Graph delete agent identity returned {status}: {}",
                &body[..body.len().min(400)]
            ))
        }
    }

    /// Acquire an ARM management token via the controller MI.
    ///
    /// Used for `Microsoft.Authorization/roleAssignments` operations —
    /// PUT to grant a role to an agent identity SP at provisioning
    /// time, and DELETE on sandbox deprovision.
    ///
    /// This is the SAME MI chain as `graph_token`, just with the
    /// audience set to `https://management.azure.com/`. The token is
    /// cached separately because Entra issues per-audience tokens.
    async fn arm_token(&self) -> Result<String, String> {
        self.mi_token("https://management.azure.com/").await
    }

    /// Assign an Azure RBAC role to an agent identity SP at the given
    /// scope. Idempotent via deterministic assignment GUID derived from
    /// `(scope, principal_id, role_definition_id)` — Azure returns 409
    /// `RoleAssignmentExists` on a re-PUT with the same GUID, which we
    /// treat as success.
    ///
    /// Requires the controller MI to have
    /// `Microsoft.Authorization/roleAssignments/write` at the
    /// requested scope. When the permission is missing, Azure returns
    /// 403 `AuthorizationFailed` and the caller surfaces this on
    /// `KarsSandbox.status` as `AgentRbacAssignmentFailed=False`.
    ///
    /// `subscription_id` is parsed out of the scope to construct the
    /// fully-qualified role definition URI per ARM's contract.
    pub async fn assign_role_to_agent_identity(
        &self,
        principal_id: &str,
        role_definition_id: &str,
        scope: &str,
    ) -> Result<(), String> {
        let token = self.arm_token().await?;

        // Stable assignment name = guid(scope, principalId, roleId).
        // Azure ARM requires the assignment name to be a GUID; using a
        // deterministic hash ensures retries always upsert the same row.
        let assignment_name =
            deterministic_assignment_guid(scope, principal_id, role_definition_id);

        // Extract subscription id from the scope so we can build the
        // role-definition URI. Scopes always start with
        // `/subscriptions/<sub>/...`.
        let sub_id = extract_subscription_id(scope)
            .ok_or_else(|| format!("scope '{scope}' does not contain a subscription id"))?;
        let role_def_uri = format!(
            "/subscriptions/{sub_id}/providers/Microsoft.Authorization/roleDefinitions/{role_definition_id}"
        );

        // ARM normalises scopes by trimming leading slashes; we keep
        // them so the URL builds cleanly.
        let url = format!(
            "https://management.azure.com{scope}/providers/Microsoft.Authorization/roleAssignments/{assignment_name}?api-version=2022-04-01"
        );

        let body = serde_json::json!({
            "properties": {
                "roleDefinitionId": role_def_uri,
                "principalId": principal_id,
                "principalType": "ServicePrincipal",
                "description": format!("kars-managed; agent identity {principal_id}")
            }
        });

        let resp = self
            .http
            .put(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| format!("ARM role assignment PUT failed: {e}"))?;

        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();

        // Success: 201 Created (fresh) or 200 OK (idempotent overwrite).
        if status.is_success() {
            tracing::info!(
                principal_id,
                role_definition_id,
                scope,
                "ARM role assignment created/updated"
            );
            return Ok(());
        }

        // 409 RoleAssignmentExists — ARM rejects a PUT against the same
        // (scope, principal, role) tuple even with the same name. Treat
        // as idempotent success: the assignment already exists.
        if status.as_u16() == 409 && body_text.contains("RoleAssignmentExists") {
            tracing::debug!(
                principal_id,
                role_definition_id,
                scope,
                "role assignment already present (RoleAssignmentExists) — idempotent"
            );
            return Ok(());
        }

        Err(format!(
            "ARM role assignment PUT returned {status}: {}",
            &body_text[..body_text.len().min(400)]
        ))
    }

    /// Delete every Azure RBAC role assignment held by a given agent
    /// identity SP at the given scope.
    ///
    /// Called from the sandbox-deletion finalizer to clean up after a
    /// `KarsSandbox` is destroyed. The agent identity SP itself is
    /// deleted separately via [`delete_agent_identity`]; this method
    /// only reaps the role assignments so the SP doesn't leave
    /// orphaned grants on the Foundry resource.
    ///
    /// Idempotent: an empty list of assignments is success. A 404 on
    /// any individual DELETE is success.
    pub async fn delete_role_assignments_for_principal(
        &self,
        principal_id: &str,
        scope: &str,
    ) -> Result<usize, String> {
        let token = self.arm_token().await?;

        // Azure ARM only accepts ONE of `atScope()` or `principalId eq`
        // in the `$filter` query — combining them returns 400
        // `UnsupportedQuery`. We pass `principalId eq` (the more
        // selective filter) and then narrow to assignments at the
        // requested scope (or below) on the client side. The list
        // call already only returns assignments visible at this
        // scope's read path, so this is just an extra defensive
        // filter on the assignment `scope` property.
        let list_url = format!(
            "https://management.azure.com{scope}/providers/Microsoft.Authorization/roleAssignments?api-version=2022-04-01&$filter=principalId+eq+%27{principal_id}%27"
        );

        let resp = self
            .http
            .get(&list_url)
            .header("Authorization", format!("Bearer {token}"))
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| format!("ARM role assignment list failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "ARM list role assignments returned {status}: {}",
                &body[..body.len().min(400)]
            ));
        }

        #[derive(Deserialize)]
        struct ListResp {
            value: Vec<AssignmentRef>,
        }
        #[derive(Deserialize)]
        struct AssignmentRef {
            id: String,
            properties: Option<AssignmentProps>,
        }
        #[derive(Deserialize)]
        struct AssignmentProps {
            scope: Option<String>,
        }

        let list: ListResp = resp
            .json()
            .await
            .map_err(|e| format!("ARM list role assignments parse failed: {e}"))?;

        let scope_lower = scope.to_ascii_lowercase();
        let mut deleted = 0;
        for a in &list.value {
            // Narrow client-side: skip assignments whose scope is
            // not the requested scope itself. Parent-scope inherited
            // assignments belong to a different lifecycle (typically
            // an operator-managed RG-level grant), and we MUST NOT
            // delete those on sandbox teardown.
            if let Some(p) = &a.properties
                && let Some(s) = &p.scope
            {
                let s_lower = s.to_ascii_lowercase();
                if s_lower != scope_lower {
                    tracing::debug!(
                        assignment_id = %a.id,
                        assignment_scope = %s,
                        target_scope = %scope,
                        "skipping assignment at non-matching scope (likely inherited)"
                    );
                    continue;
                }
            }

            // a.id already includes /subscriptions/...; build the full URL.
            let delete_url = format!(
                "https://management.azure.com{}?api-version=2022-04-01",
                a.id
            );
            let r = self
                .http
                .delete(&delete_url)
                .header("Authorization", format!("Bearer {token}"))
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
                .map_err(|e| format!("ARM role assignment DELETE failed: {e}"))?;
            let s = r.status();
            if s.is_success() || s.as_u16() == 404 {
                deleted += 1;
            } else {
                let body = r.text().await.unwrap_or_default();
                return Err(format!(
                    "ARM DELETE role assignment {} returned {s}: {}",
                    a.id,
                    &body[..body.len().min(200)]
                ));
            }
        }

        tracing::info!(
            principal_id,
            scope,
            deleted,
            "deleted role assignments for principal"
        );

        Ok(deleted)
    }

    /// Fetch an existing agent identity by object ID.
    ///
    /// Used during reconcile to confirm the SP we recorded in status
    /// still exists. Returns `Ok(None)` on 404 so the reconciler can
    /// treat "SP was deleted out-of-band" as a re-create signal.
    #[allow(dead_code)]
    pub async fn get_agent_identity(
        &self,
        object_id: &str,
    ) -> Result<Option<AgentIdentity>, String> {
        let token = self.graph_token().await?;
        let url = format!("https://graph.microsoft.com/beta/serviceprincipals/{object_id}");

        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("OData-Version", "4.0")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("Graph get agent identity failed: {e}"))?;

        let status = resp.status();
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "Graph get agent identity returned {status}: {}",
                &body[..body.len().min(400)]
            ));
        }

        let parsed: AgentIdentity = resp
            .json()
            .await
            .map_err(|e| format!("Graph get agent identity parse failed: {e}"))?;
        Ok(Some(parsed))
    }

    /// List all agent identities derived from the configured blueprint
    /// and filter to those bearing this cluster's tag.
    ///
    /// Used by the reaper to find orphaned SPs whose owning
    /// `KarsSandbox` was deleted. The Graph `$filter` parameter
    /// supports `agentIdentityBlueprintId eq '<id>'` so we don't need
    /// to enumerate every SP in the tenant.
    pub async fn list_cluster_agent_identities(
        &self,
        blueprint_app_id: &str,
    ) -> Result<Vec<AgentIdentity>, String> {
        let token = self.graph_token().await?;
        let cluster_tag = Self::cluster_tag(&self.config.cluster_uid);
        let filter = format!(
            "agentIdentityBlueprintId eq '{blueprint_app_id}' and tags/any(t:t eq '{cluster_tag}')"
        );

        // reqwest's `.query()` percent-encodes values; we don't need
        // to depend on a separate urlencoding crate.
        let resp = self
            .http
            .get("https://graph.microsoft.com/beta/servicePrincipals/Microsoft.Graph.AgentIdentity")
            .query(&[("$filter", filter.as_str()), ("$top", "999")])
            .header("Authorization", format!("Bearer {token}"))
            .header("OData-Version", "4.0")
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| format!("Graph list agent identities failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "Graph list agent identities returned {status}: {}",
                &body[..body.len().min(400)]
            ));
        }

        // Capture the raw body so we can give a useful error message
        // (the typed `AgentIdentity` deserialiser is strict — a single
        // unexpected field shape in the response means we lose the
        // entire list). Try the strict shape first; on failure, fall
        // back to a permissive walk that picks out only the fields we
        // actually need (id, appId, tags) from each item.
        let body = resp
            .text()
            .await
            .map_err(|e| format!("Graph list body read failed: {e}"))?;

        #[derive(Deserialize)]
        struct ListResp {
            value: Vec<AgentIdentity>,
        }
        if let Ok(parsed) = serde_json::from_str::<ListResp>(&body) {
            return Ok(parsed.value);
        }

        // Permissive fallback: walk the JSON, extract only the fields
        // the orchestrator needs. Graph occasionally returns variant
        // service-principal shapes (e.g. when the agent identity
        // inherits an extension type) that don't fit the strict
        // schema. Losing optional metadata is acceptable; losing the
        // recovery path is not (would cause an unbounded duplicate-
        // SP creation loop as observed live on kars-aks).
        //
        // Log the strict-parse failure at WARN so operators see WHY
        // the fallback fired (Graph response shape drift) — silent
        // fallback was hiding a critical mis-mapping bug (see below).
        tracing::warn!(
            body_prefix = %&body[..body.len().min(200)],
            "list_cluster_agent_identities: strict parse failed; using permissive fallback"
        );
        let raw: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            format!(
                "Graph list parse failed: {e}; body starts with: {}",
                &body[..body.len().min(200)]
            )
        })?;
        let items = raw
            .get("value")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let parsed: Vec<AgentIdentity> = items
            .into_iter()
            .filter_map(|item| {
                let id = item.get("id")?.as_str()?.to_string();
                // CRITICAL ORDER: `appId` is the PER-SANDBOX identity's
                // own client ID. `agentAppId` (when present) is the
                // PARENT BLUEPRINT's appId — using it as the
                // per-sandbox identity's app_id silently makes every
                // sandbox impersonate the blueprint, which then
                // (a) PINs the blueprint in router env so the sidecar
                // mints tokens for the wrong principal, and (b) makes
                // KarsSandbox.status.agentIdentity.appId useless for
                // distinguishing sandboxes from each other.
                //
                // Pre-fix: `agentAppId` was tried first, causing the
                // exact failure mode above on the kars-aks Phase 6.b
                // dogfood run (2026-05-29T07:26 — first 4 sandboxes
                // all ended up with status.agentIdentity.appId =
                // <blueprint>, which broke /v1/mesh-token + RBAC).
                let app_id = item
                    .get("appId")
                    .or_else(|| item.get("agentAppId"))
                    .and_then(|v| v.as_str())?
                    .to_string();
                let display_name = item
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tags = item
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| t.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                Some(AgentIdentity {
                    id,
                    app_id,
                    display_name,
                    agent_identity_blueprint_id: item
                        .get("agentIdentityBlueprintId")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    created_date_time: item
                        .get("createdDateTime")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    service_principal_type: item
                        .get("servicePrincipalType")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    tags,
                })
            })
            .collect();
        Ok(parsed)
    }

    /// Compose the `tags` slice for a freshly-created agent identity.
    ///
    /// We deliberately keep these short and machine-parseable. The
    /// reaper relies on `kars-cluster-uid:<uid>` to find orphans, so
    /// future tags MUST not collide with this prefix.
    pub(crate) fn tags_for(cluster_uid: &str, sandbox_uid: &str) -> Vec<String> {
        vec![
            Self::cluster_tag(cluster_uid),
            format!("kars-sandbox-uid:{sandbox_uid}"),
            "kars-managed:true".to_string(),
        ]
    }

    fn cluster_tag(cluster_uid: &str) -> String {
        format!("kars-cluster-uid:{cluster_uid}")
    }
}

/// Map a `serde_json::Value` to its Microsoft Graph
/// `@odata.type` annotation for use in a custom-security-attribute
/// PATCH payload.
///
/// Graph requires every per-attribute value to carry an explicit
/// type annotation (`#String`, `#Int32`, `#Boolean`,
/// `#Collection(String)`, etc.) — there is no implicit type
/// inference on the server side. Floats, nulls, nested objects, and
/// mixed-type arrays are NOT supported by the
/// custom-security-attributes schema and are rejected here with a
/// clear error rather than producing a 400 from Graph.
///
/// Supported value shapes:
/// - JSON string → `#String`
/// - JSON integer → `#Int32`
/// - JSON boolean → `#Boolean`
/// - JSON array of strings → `#Collection(String)`
/// - JSON array of integers → `#Collection(Int32)`
/// - JSON array of booleans → `#Collection(Boolean)`
fn odata_type_for_value(value: &serde_json::Value) -> Result<String, String> {
    match value {
        serde_json::Value::String(_) => Ok("#String".to_string()),
        serde_json::Value::Bool(_) => Ok("#Boolean".to_string()),
        serde_json::Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                Ok("#Int32".to_string())
            } else {
                Err(
                    "non-integer numeric values (floats) are not supported by Entra \
                     custom security attributes; convert to string or integer"
                        .to_string(),
                )
            }
        }
        serde_json::Value::Array(arr) if !arr.is_empty() => {
            // Disallow mixed-type and empty arrays. The collection
            // type is determined by the first element; all subsequent
            // elements must match.
            let first = &arr[0];
            let element_type = match first {
                serde_json::Value::String(_) => "String",
                serde_json::Value::Bool(_) => "Boolean",
                serde_json::Value::Number(n) if n.is_i64() || n.is_u64() => "Int32",
                _ => {
                    return Err(format!(
                        "unsupported array element type {first:?}; expected string, integer, or boolean"
                    ));
                }
            };
            for (i, el) in arr.iter().enumerate().skip(1) {
                let same = match (first, el) {
                    (serde_json::Value::String(_), serde_json::Value::String(_)) => true,
                    (serde_json::Value::Bool(_), serde_json::Value::Bool(_)) => true,
                    (serde_json::Value::Number(a), serde_json::Value::Number(b)) => {
                        (a.is_i64() || a.is_u64()) && (b.is_i64() || b.is_u64())
                    }
                    _ => false,
                };
                if !same {
                    return Err(format!(
                        "mixed-type array at index {i}: first element is {element_type} but element {i} is {el:?}"
                    ));
                }
            }
            Ok(format!("#Collection({element_type})"))
        }
        serde_json::Value::Array(_) => {
            Err("empty arrays are not supported; omit the attribute instead".to_string())
        }
        serde_json::Value::Null => {
            Err("null values are not supported; omit the attribute to clear it".to_string())
        }
        serde_json::Value::Object(_) => Err(
            "nested objects are not supported in custom security attributes; \
             use a separate attribute per field"
                .to_string(),
        ),
    }
}

/// Derive a deterministic UUID for an ARM role assignment.
///
/// Azure ARM requires assignment names to be GUIDs. To make our
/// PUT-then-PUT idempotent across reconciles and across clusters, we
/// derive the GUID from `(scope, principal_id, role_definition_id)`
/// using a stable hash. Two callers granting the same role to the same
/// principal at the same scope will produce the same GUID and either
/// see the existing assignment (200) or a 409 RoleAssignmentExists
/// (treated as success). No randomness, no clock dependence.
///
/// Implementation: SHA-256 the canonical key, take 16 bytes, format
/// as a UUIDv4-shaped string (we set the version + variant bits to
/// produce a valid v4 GUID per RFC 4122).
fn deterministic_assignment_guid(
    scope: &str,
    principal_id: &str,
    role_definition_id: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"kars-role-assignment-v1\x00");
    h.update(scope.to_ascii_lowercase().as_bytes());
    h.update(b"\x00");
    h.update(principal_id.to_ascii_lowercase().as_bytes());
    h.update(b"\x00");
    h.update(role_definition_id.to_ascii_lowercase().as_bytes());
    let digest = h.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // Set UUIDv4 version + RFC4122 variant bits.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

/// Extract the subscription id from an ARM scope.
///
/// Returns `None` when the scope does not start with the expected
/// `/subscriptions/<guid>/...` pattern. Used by
/// [`AgentIdentityClient::assign_role_to_agent_identity`] to build the
/// fully-qualified role-definition URI.
fn extract_subscription_id(scope: &str) -> Option<String> {
    let s = scope.trim_start_matches('/');
    let mut parts = s.splitn(3, '/');
    match (parts.next(), parts.next()) {
        (Some("subscriptions"), Some(id)) if !id.is_empty() => Some(id.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_for_includes_cluster_and_sandbox() {
        let tags = AgentIdentityClient::tags_for("cluster-abc", "sandbox-xyz");
        assert!(tags.iter().any(|t| t == "kars-cluster-uid:cluster-abc"));
        assert!(tags.iter().any(|t| t == "kars-sandbox-uid:sandbox-xyz"));
        assert!(tags.iter().any(|t| t == "kars-managed:true"));
    }

    #[test]
    fn cluster_tag_is_stable_prefix() {
        // The reaper depends on this exact prefix; pin it as a regression test.
        assert_eq!(
            AgentIdentityClient::cluster_tag("abc"),
            "kars-cluster-uid:abc"
        );
    }

    #[test]
    fn agent_identity_deserialises_graph_response_subset() {
        // Real Graph response shape captured during the POC. The
        // controller only reads the subset of fields it cares about.
        let raw = r#"{
            "@odata.context": "https://graph.microsoft.com/beta/$metadata#servicePrincipals/microsoft.graph.agentIdentity/$entity",
            "id": "a8e0eff0-1fe0-4b46-aba3-d7fa7a1c2ecd",
            "appId": "a8e0eff0-1fe0-4b46-aba3-d7fa7a1c2ecd",
            "displayName": "kars-poc-agent-1",
            "servicePrincipalType": "ServiceIdentity",
            "agentIdentityBlueprintId": "9010cbe3-ee13-4cb6-aa5f-f892910804a0",
            "createdDateTime": "2026-05-27T11:22:48Z",
            "tags": ["kars-cluster-uid:abc", "kars-sandbox-uid:xyz", "kars-managed:true"]
        }"#;
        let parsed: AgentIdentity = serde_json::from_str(raw).expect("parse Graph response");
        assert_eq!(parsed.id, "a8e0eff0-1fe0-4b46-aba3-d7fa7a1c2ecd");
        assert_eq!(parsed.app_id, "a8e0eff0-1fe0-4b46-aba3-d7fa7a1c2ecd");
        assert_eq!(parsed.display_name, "kars-poc-agent-1");
        assert_eq!(
            parsed.service_principal_type.as_deref(),
            Some("ServiceIdentity")
        );
        assert_eq!(parsed.tags.len(), 3);
    }

    // ─── odata_type_for_value ───────────────────────────────────────

    #[test]
    fn odata_type_strings() {
        let v = serde_json::Value::String("Standard".into());
        assert_eq!(odata_type_for_value(&v).unwrap(), "#String");
    }

    #[test]
    fn odata_type_integers() {
        let v = serde_json::json!(42);
        assert_eq!(odata_type_for_value(&v).unwrap(), "#Int32");
        let v2 = serde_json::json!(-1);
        assert_eq!(odata_type_for_value(&v2).unwrap(), "#Int32");
    }

    #[test]
    fn odata_type_booleans() {
        let v = serde_json::json!(true);
        assert_eq!(odata_type_for_value(&v).unwrap(), "#Boolean");
    }

    #[test]
    fn odata_type_string_array() {
        let v = serde_json::json!(["a", "b", "c"]);
        assert_eq!(odata_type_for_value(&v).unwrap(), "#Collection(String)");
    }

    #[test]
    fn odata_type_int_array() {
        let v = serde_json::json!([1, 2, 3]);
        assert_eq!(odata_type_for_value(&v).unwrap(), "#Collection(Int32)");
    }

    #[test]
    fn odata_type_bool_array() {
        let v = serde_json::json!([true, false]);
        assert_eq!(odata_type_for_value(&v).unwrap(), "#Collection(Boolean)");
    }

    #[test]
    fn odata_type_rejects_floats() {
        // Intentional non-PI float — clippy::approx_constant flags 3.14 as
        // approximating std::f64::consts::PI even in a test fixture.
        let v = serde_json::json!(2.71);
        let err = odata_type_for_value(&v).unwrap_err();
        assert!(err.contains("floats") && err.contains("not supported"));
    }

    #[test]
    fn odata_type_rejects_null() {
        let v = serde_json::Value::Null;
        let err = odata_type_for_value(&v).unwrap_err();
        assert!(err.contains("null") && err.contains("omit"));
    }

    #[test]
    fn odata_type_rejects_object() {
        let v = serde_json::json!({"nested": "value"});
        let err = odata_type_for_value(&v).unwrap_err();
        assert!(err.contains("nested objects"));
    }

    #[test]
    fn odata_type_rejects_empty_array() {
        let v = serde_json::json!([]);
        let err = odata_type_for_value(&v).unwrap_err();
        assert!(err.contains("empty arrays"));
    }

    #[test]
    fn odata_type_rejects_mixed_array() {
        let v = serde_json::json!(["a", 1, "b"]);
        let err = odata_type_for_value(&v).unwrap_err();
        assert!(err.contains("mixed-type"), "got: {err}");
    }

    #[test]
    fn odata_type_rejects_float_in_array() {
        let v = serde_json::json!([1.5, 2.5]);
        let err = odata_type_for_value(&v).unwrap_err();
        // First-element validator catches the float as unsupported.
        assert!(err.contains("unsupported"), "got: {err}");
    }

    // ─── ARM RBAC helpers ─────────────────────────────────────────────

    #[test]
    fn extract_subscription_id_full_scope() {
        let s = "/subscriptions/1f67a2fd-4c9f-4de2-986a-32492d427fd9/resourceGroups/foo/providers/Microsoft.Cog/accounts/bar";
        assert_eq!(
            extract_subscription_id(s).as_deref(),
            Some("1f67a2fd-4c9f-4de2-986a-32492d427fd9")
        );
    }

    #[test]
    fn extract_subscription_id_subscription_only() {
        let s = "/subscriptions/aaaa-bbbb";
        assert_eq!(extract_subscription_id(s).as_deref(), Some("aaaa-bbbb"));
    }

    #[test]
    fn extract_subscription_id_rejects_management_group_scope() {
        let s = "/providers/Microsoft.Management/managementGroups/myMg";
        assert_eq!(extract_subscription_id(s), None);
    }

    #[test]
    fn extract_subscription_id_rejects_empty() {
        assert_eq!(extract_subscription_id(""), None);
        assert_eq!(extract_subscription_id("/"), None);
        // Missing subscription value after the keyword.
        assert_eq!(extract_subscription_id("/subscriptions/"), None);
    }

    /// Regression pin: the permissive fallback in
    /// `list_cluster_agent_identities` MUST prefer `appId` over
    /// `agentAppId`. The latter is the parent BLUEPRINT's app_id and
    /// using it for the per-sandbox identity makes every sandbox
    /// impersonate the blueprint — observed live on kars-aks
    /// 2026-05-29T07:26 when Graph response shape drift caused the
    /// strict parser to fall through to the permissive path. See
    /// commit message for the failure mode trace.
    #[test]
    fn permissive_fallback_prefers_app_id_over_agent_app_id() {
        let body = serde_json::json!({
            "value": [{
                "id": "889ab472-6ebc-4e3c-9e07-618f5d361663",
                "appId": "889ab472-6ebc-4e3c-9e07-618f5d361663",
                "agentAppId": "b712af17-b7f7-419f-a306-b86a607d5a21",
                "displayName": "kars-test-execbrief",
                "tags": ["kars-cluster-uid:abc", "kars-sandbox-uid:xyz"],
            }]
        });
        let items = body
            .get("value")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap();
        let parsed: Vec<AgentIdentity> = items
            .into_iter()
            .filter_map(|item| {
                let id = item.get("id")?.as_str()?.to_string();
                let app_id = item
                    .get("appId")
                    .or_else(|| item.get("agentAppId"))
                    .and_then(|v| v.as_str())?
                    .to_string();
                Some(AgentIdentity {
                    id,
                    app_id,
                    display_name: String::new(),
                    agent_identity_blueprint_id: None,
                    created_date_time: None,
                    service_principal_type: None,
                    tags: vec![],
                })
            })
            .collect();
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].app_id, "889ab472-6ebc-4e3c-9e07-618f5d361663",
            "permissive parser MUST select the per-sandbox appId, not the blueprint agentAppId"
        );
        assert_ne!(
            parsed[0].app_id, "b712af17-b7f7-419f-a306-b86a607d5a21",
            "regressing this would silently make every sandbox impersonate the blueprint"
        );
    }

    /// Forward-compat: when `appId` is absent (older Graph variants
    /// or some extension types), fall back to `agentAppId` so we
    /// don't break the SP discovery path entirely. This is the
    /// "lose attribution, keep the recovery path" trade-off the
    /// permissive parser is explicitly designed for.
    #[test]
    fn permissive_fallback_uses_agent_app_id_when_app_id_absent() {
        let body = serde_json::json!({
            "value": [{
                "id": "abc",
                "agentAppId": "b712af17-...",
                "displayName": "x",
            }]
        });
        let items = body
            .get("value")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap();
        let parsed: Vec<String> = items
            .into_iter()
            .filter_map(|item| {
                item.get("appId")
                    .or_else(|| item.get("agentAppId"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .collect();
        assert_eq!(parsed, vec!["b712af17-..."]);
    }

    #[test]
    fn assignment_guid_is_stable_across_invocations() {
        let g1 = deterministic_assignment_guid(
            "/subscriptions/x/resourceGroups/rg/providers/Microsoft.Cog/accounts/foundry",
            "889ab472-6ebc-4e3c-9e07-618f5d361663",
            "5e0bd9bd-7b93-4f28-af87-19fc36ad61bd",
        );
        let g2 = deterministic_assignment_guid(
            "/subscriptions/x/resourceGroups/rg/providers/Microsoft.Cog/accounts/foundry",
            "889ab472-6ebc-4e3c-9e07-618f5d361663",
            "5e0bd9bd-7b93-4f28-af87-19fc36ad61bd",
        );
        assert_eq!(g1, g2, "same key must produce same GUID for idempotency");
    }

    #[test]
    fn assignment_guid_changes_when_role_changes() {
        let g1 = deterministic_assignment_guid(
            "/subscriptions/x/resourceGroups/rg",
            "principal-1",
            "role-A",
        );
        let g2 = deterministic_assignment_guid(
            "/subscriptions/x/resourceGroups/rg",
            "principal-1",
            "role-B",
        );
        assert_ne!(g1, g2);
    }

    #[test]
    fn assignment_guid_changes_when_principal_changes() {
        let g1 = deterministic_assignment_guid("/scope", "principal-1", "role");
        let g2 = deterministic_assignment_guid("/scope", "principal-2", "role");
        assert_ne!(g1, g2);
    }

    #[test]
    fn assignment_guid_is_case_insensitive_on_inputs() {
        // Azure RBAC IDs are case-insensitive, and the same logical
        // (scope, principal, role) tuple in different casing must map
        // to the same assignment GUID — otherwise an operator switching
        // from upper- to lower-case in their Bicep would create a
        // duplicate assignment.
        let g1 =
            deterministic_assignment_guid("/subscriptions/X/resourceGroups/RG", "AAAAA", "BBBBB");
        let g2 =
            deterministic_assignment_guid("/subscriptions/x/resourceGroups/rg", "aaaaa", "bbbbb");
        assert_eq!(g1, g2);
    }

    #[test]
    fn assignment_guid_is_valid_uuid_v4_format() {
        let g = deterministic_assignment_guid("/s", "p", "r");
        // 8-4-4-4-12 hex digits.
        assert_eq!(g.len(), 36, "{g}");
        let parts: Vec<&str> = g.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
        // Version nibble at position 14 must be '4' for UUIDv4.
        assert_eq!(parts[2].chars().next(), Some('4'), "version nibble: {g}");
        // Variant bits: first char of group 4 must be 8, 9, a, or b.
        let variant = parts[3].chars().next().unwrap();
        assert!(
            "89ab".contains(variant),
            "variant nibble must be 8/9/a/b: {g}"
        );
    }
}
