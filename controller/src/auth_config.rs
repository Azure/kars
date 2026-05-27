// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsAuthConfig` CRD — cluster-scoped singleton holding the Entra
//! Agent ID provisioning anchors.
//!
//! Status: **first-class** as of `feat/entra-agent-id`.
//!
//! ## Why this CRD exists
//!
//! Authentication for kars sandbox pods is configured exactly once per
//! kars deployment: a single `KarsAuthConfig` named `default` holds:
//!
//! - The **tenant-wide blueprint** (Entra application + service
//!   principal of type `agentIdentityBlueprint`) that all per-sandbox
//!   agent identities derive from.
//! - The **per-cluster controller managed identity** assigned to the
//!   AKS sandbox node pool VMSS. Its IMDS token is the credential the
//!   blueprint trusts (via MI-as-FIC on
//!   `issuer=login.microsoftonline.com/<tid>/v2.0`).
//! - Downstream API endpoint + scope configuration handed to the
//!   sidecar (Microsoft Entra SDK for Agent ID).
//!
//! When this CR is **absent**, kars sandbox pods start in the AGT
//! anonymous tier (trust score 0, no token acquisition). This is the
//! fallback path documented in
//! `docs/architecture/entra-agent-id/01-runtime-token-flow.md`.
//!
//! ## Scope
//!
//! Cluster-scoped, singleton by convention (`metadata.name == "default"`).
//! The reconciler in
//! `controller/src/auth_config_reconciler.rs` rejects any CR with a
//! different name and surfaces a `NotDefault` condition so an operator
//! can self-diagnose without trawling logs.
//!
//! ## Lifecycle
//!
//! 1. `kars mesh setup-trust` creates the blueprint via Microsoft Graph
//!    (delegated user auth), provisions the controller MI, and writes
//!    this CR.
//! 2. The reconciler materialises a sibling **ConfigMap** in the
//!    `azureclaw-system` namespace (`kars-auth-sidecar-env`) with the
//!    flat environment variables the Entra SDK sidecar consumes. Pods
//!    `envFrom` that ConfigMap rather than reading the CR directly.
//! 3. Sandbox reconciler reads this CR (or the materialised ConfigMap)
//!    to decide between agent-id and anonymous modes.
//!
//! ## Why a CRD instead of a ConfigMap or Secret directly
//!
//! Strongly-typed schema with validation, status conditions for human
//! diagnosis, and clear separation between user-facing intent
//! (the CR) and runtime-consumed projection (the ConfigMap). Mirrors
//! the existing kars pattern (`InferencePolicy`, `KarsMemory`, etc.).

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `KarsAuthConfig.spec` — cluster-wide Entra Agent ID provisioning
/// anchors.
///
/// All fields are required when the CR is created via
/// `kars mesh setup-trust`. The reconciler refuses to materialise the
/// sidecar ConfigMap until every field is populated.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsAuthConfig",
    status = "KarsAuthConfigStatus",
    shortname = "kac",
    printcolumn = r#"{"name":"Tenant","type":"string","jsonPath":".spec.tenant.tenantId"}"#,
    printcolumn = r#"{"name":"Blueprint","type":"string","jsonPath":".spec.agentId.blueprintClientId"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KarsAuthConfigSpec {
    /// Microsoft Entra tenant anchoring the blueprint + controller MI.
    pub tenant: TenantConfig,

    /// Entra Agent Identity blueprint provisioned by
    /// `kars mesh setup-trust`. One blueprint per kars deployment.
    pub agent_id: AgentIdConfig,

    /// Per-cluster controller managed identity. Assigned to the AKS
    /// sandbox node pool VMSS so its IMDS token is reachable from
    /// every sandbox pod's sidecar container.
    pub controller: ControllerIdentityConfig,

    /// Downstream APIs the sidecar should be pre-configured for. Each
    /// entry is rendered into `DownstreamApis__<Name>__*` environment
    /// variables on the sidecar container.
    ///
    /// Empty map is allowed (sandbox can still call sidecar
    /// `/AuthorizationHeaderUnauthenticated/<api>` with
    /// `optionsOverride.Scopes=` query params), but the recommended
    /// pattern is to centralise scope policy here.
    #[serde(default)]
    pub downstream_apis: std::collections::BTreeMap<String, DownstreamApiConfig>,
}

/// Tenant-level anchoring information.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TenantConfig {
    /// Microsoft Entra tenant GUID.
    pub tenant_id: String,

    /// Authority host. Defaults to
    /// `https://login.microsoftonline.com/` — overridden only for
    /// non-public Azure clouds (Gov, China).
    #[serde(default = "default_authority_host")]
    pub authority_host: String,

    /// Optional ServiceTree / service-management GUID required by some
    /// enterprise tenants (notably the Microsoft corporate tenant)
    /// when registering new Entra applications. When set, the CLI
    /// `kars mesh setup-trust` propagates this value as
    /// `serviceManagementReference` on the `POST /applications/`
    /// body that creates the blueprint. Recorded here for diagnostic
    /// auditability — the controller does NOT use this value at
    /// runtime, since per-sandbox agent identities derive from the
    /// already-tagged blueprint.
    ///
    /// Most non-Microsoft tenants leave this `None`. Operators in
    /// Microsoft corporate or similarly-policed tenants must supply
    /// their ServiceTree GUID at `kars mesh setup-trust` time
    /// (`--service-tree <guid>` or `KARS_SERVICE_TREE` env var).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_management_reference: Option<String>,
}

fn default_authority_host() -> String {
    "https://login.microsoftonline.com/".to_string()
}

/// Blueprint identity references.
///
/// The blueprint is an Entra `Application` with
/// `@odata.type=#Microsoft.Graph.AgentIdentityBlueprint` plus its paired
/// `ServicePrincipal`. Created once per kars deployment via Graph by
/// `kars mesh setup-trust`. Both IDs are recorded here so the
/// controller can:
///
/// - Use `blueprintClientId` as the sidecar's `AzureAd__ClientId`.
/// - Use `blueprintObjectId` to add/remove federated identity
///   credentials and to derive per-sandbox agent identities via
///   `POST /beta/serviceprincipals/Microsoft.Graph.AgentIdentity`.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentIdConfig {
    /// Blueprint Application `appId` (client ID). Sidecar consumes this
    /// as `AzureAd__ClientId`.
    pub blueprint_client_id: String,

    /// Blueprint Application `id` (object ID). Required for Graph
    /// `PATCH /applications/{id}` operations and FIC management.
    pub blueprint_object_id: String,
}

/// Per-cluster controller managed identity.
///
/// Created in the customer's Azure subscription. Assigned to the AKS
/// sandbox node pool VMSS (`az vmss identity assign --identities <rid>`)
/// so pods on that pool can fetch the MI's token from IMDS at
/// `169.254.169.254`. The IMDS-issued token is **not** federated, so
/// presenting it as the blueprint's MI-as-FIC assertion does not
/// trigger the Entra anti-loop check (`AADSTS700231`).
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ControllerIdentityConfig {
    /// Managed identity `clientId`. Sidecar consumes this as
    /// `AzureAd__ClientCredentials__0__ManagedIdentityClientId`.
    pub managed_identity_client_id: String,

    /// Managed identity full ARM resource ID. Required for the
    /// controller to verify VMSS assignment and to delete on teardown.
    pub managed_identity_resource_id: String,

    /// Optional managed identity `principalId` (the SP object id used
    /// as the subject in the blueprint's MI-as-FIC). Recorded for
    /// drift detection in `auth_config_reconciler`; not consumed by
    /// the sidecar at runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_identity_principal_id: Option<String>,
}

/// One downstream API entry pre-configured on the sidecar.
///
/// Rendered into `DownstreamApis__<key>__BaseUrl`,
/// `DownstreamApis__<key>__Scopes__0..N`, and
/// `DownstreamApis__<key>__RequestAppToken` env vars on the sidecar.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownstreamApiConfig {
    /// Base URL of the downstream service (e.g.
    /// `https://<account>.cognitiveservices.azure.com/`).
    pub base_url: String,

    /// One or more OAuth scopes the sidecar should request. At least
    /// one entry required.
    pub scopes: Vec<String>,

    /// `true` for app-only flows (autonomous agents) — the default for
    /// kars. `false` requires an inbound user token (OBO flow), which
    /// is not used in current kars.
    #[serde(default = "default_request_app_token")]
    pub request_app_token: bool,
}

fn default_request_app_token() -> bool {
    true
}

/// `KarsAuthConfig.status` — surface reconciler decisions for humans.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KarsAuthConfigStatus {
    /// `Pending` | `Ready` | `Degraded` | `NotDefault`. Set by
    /// `auth_config_reconciler`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    /// The `metadata.generation` last observed by the reconciler.
    /// Consumers compare against `metadata.generation` to detect
    /// stale observations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,

    /// Number of federated identity credentials currently on the
    /// blueprint application. Surfaced so `kars doctor` can warn when
    /// approaching the per-app FIC quota (currently 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint_fic_count: Option<i32>,

    /// Soft upper bound on federated identity credentials per Entra
    /// application. Currently 20. Stored here so newer kars releases
    /// can carry an updated value without a CRD migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint_fic_quota: Option<i32>,

    /// Standard K8s Condition list. At most one entry per `type`.
    /// Maintained by `controller::status::conditions` helpers.
    ///
    /// Well-known types:
    /// - `BlueprintReady` — Graph reports the blueprint exists and is
    ///   enabled.
    /// - `ControllerMIReachable` — IMDS on the controller's node pool
    ///   returns a token for the configured MI.
    /// - `FederatedCredentialReady` — blueprint has an MI-as-FIC
    ///   entry matching the configured controller MI's principal id.
    /// - `SidecarConfigMaterialized` — the sibling sidecar-env
    ///   ConfigMap exists and matches the current spec hash.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

/// Conventional singleton name. The reconciler rejects CRs with any
/// other name and surfaces a `NotDefault` condition.
pub const DEFAULT_AUTH_CONFIG_NAME: &str = "default";
