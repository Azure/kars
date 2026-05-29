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
//!    `kars-system` namespace (`kars-auth-sidecar-env`) with the
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

    /// Per-agent ARM RBAC role assignments. The controller PUTs each
    /// listed role against each per-sandbox agent identity SP at
    /// provisioning time and DELETEs them on sandbox deprovision.
    ///
    /// Eliminates the manual `az role assignment create` step operators
    /// would otherwise run for each new sandbox (Phase 5b).
    ///
    /// Requires the controller MI to have
    /// `Microsoft.Authorization/roleAssignments/write` on each listed
    /// scope (typically the Foundry resource group). When the
    /// permission is missing, the assignment fails non-fatally: the
    /// agent identity is still recorded, the failure surfaces as
    /// `AgentRbacAssignmentFailed=False` on the KarsSandbox status,
    /// and the sandbox boots but inference returns 401 PermissionDenied
    /// until an operator grants the role out-of-band. See
    /// `docs/architecture/entra-agent-id/05-security-alignment.md`.
    ///
    /// Empty list is the safe default — preserves backward compat with
    /// clusters bootstrapped before this field existed; operators
    /// run the manual grants documented in the migration guide.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foundry_rbac: Vec<FoundryRbacAssignment>,

    /// Mesh authentication backend (Phase 6 scaffold).
    ///
    /// Determines how the per-sandbox AGT mesh peer authenticates to
    /// the relay/registry. Today only `Anonymous` is enforced
    /// end-to-end; `EntraAgentIdentity` is scaffolded for the next
    /// milestone (sandbox entrypoint + relay JWKS verification).
    ///
    /// Default: `Anonymous` — preserves backward compatibility.
    /// Operators on clusters that have completed Phase 6 deployment
    /// flip this to `EntraAgentIdentity` to require verified mesh peers.
    ///
    /// See `docs/architecture/entra-agent-id/06-mesh-trust-design.md`.
    #[serde(default)]
    pub mesh_auth_backend: MeshAuthBackend,

    /// Token audience for AGT mesh peer authentication when
    /// `meshAuthBackend == EntraAgentIdentity`. Defaults to
    /// `api://agentmesh`; operators may override with a per-deployment
    /// custom audience matching what their relay is configured to
    /// verify.
    ///
    /// Ignored when `meshAuthBackend == Anonymous`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_auth_audience: Option<String>,
}

/// AGT mesh peer authentication backend.
///
/// Variant selection determines whether sandboxes register with the
/// AGT relay anonymously (current behaviour) or with a verifiable
/// per-agent-identity token (Phase 6 target).
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, PartialEq, Eq, Default)]
pub enum MeshAuthBackend {
    /// Sandbox connects to the AGT relay without a token; trust
    /// threshold is forced to 0. This is the only mode fully
    /// implemented today.
    #[default]
    Anonymous,
    /// Sandbox acquires an Entra-signed agent identity token via the
    /// shared auth-sidecar and presents it on every relay connection.
    /// Relay verifies the JWT against Entra's JWKS, extracts `appid`
    /// as the peer DID, and assigns a trust tier from the custom
    /// security attribute lookup table. Requires the sandbox image's
    /// entrypoint mesh-token path AND a JWKS-verifying relay to be
    /// deployed together — see
    /// `docs/architecture/entra-agent-id/06-mesh-trust-design.md`.
    EntraAgentIdentity,
}

/// One declarative role assignment to apply to every per-sandbox agent
/// identity SP at provisioning time.
///
/// All assignment names are derived deterministically from
/// `guid(scope, principalId, roleDefinitionId)` so re-provisioning is
/// idempotent on Azure's side.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FoundryRbacAssignment {
    /// Full ARM scope to assign at. Typical values:
    /// - `/subscriptions/<sub>/resourceGroups/<rg>` — covers all
    ///   downstream Cognitive Services resources in the RG.
    /// - `/subscriptions/<sub>/resourceGroups/<rg>/providers/Microsoft.CognitiveServices/accounts/<name>`
    ///   — tighter, single Foundry account.
    pub scope: String,

    /// Built-in role definition GUIDs to grant at this scope. Each is
    /// looked up at `/providers/Microsoft.Authorization/roleDefinitions/<guid>`.
    /// kars-recommended defaults:
    /// - `5e0bd9bd-7b93-4f28-af87-19fc36ad61bd` — Cognitive Services OpenAI User
    /// - `53ca6127-db72-4b80-b1b0-d745d6d5456d` — Azure AI User
    pub role_definition_ids: Vec<String>,
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

    /// Entra user object IDs that are designated sponsors on every
    /// per-sandbox agent identity created from this blueprint.
    ///
    /// Tenants with stricter governance require at least one sponsor
    /// on agent identities — Graph rejects creation otherwise with
    /// `Request_BadRequest: No sponsor specified.` In Microsoft's
    /// production tenant this is enforced unconditionally. The
    /// sponsor list is propagated into the `sponsors@odata.bind`
    /// field on the Graph `POST /servicePrincipals/Microsoft.Graph.
    /// AgentIdentity` call.
    ///
    /// Conventionally seeded with the blueprint's own owner OIDs (so
    /// the human who set up the trust is also the human responsible
    /// for governance of the agents it produces). Operators can edit
    /// this list to add or rotate sponsors without rebuilding any
    /// runtime artefacts — the next sandbox reconcile uses the new
    /// list immediately.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sponsor_user_object_ids: Vec<String>,
}

/// Per-cluster controller managed identity.
///
/// Created in the customer's Azure subscription. Assigned to the AKS
/// sandbox node pool VMSS (`az vmss identity assign --identities <rid>`)
/// so pods on that pool can fetch the MI's token from IMDS at
/// `169.254.169.254`. The IMDS-issued token is **not** federated, so
/// presenting it as the blueprint's MI-as-FIC assertion does not
/// trigger the Entra anti-loop check (`AADSTS700231`).
///
/// **Pattern B (WorkloadIdentity)**: when the cluster's tenant
/// accepts the AKS OIDC issuer URL as a federated-identity issuer
/// (the default for most non-restricted Entra tenants), kars deploys
/// in Pattern B: the auth-sidecar pod's projected service-account
/// token is the credential, and no controller MI is needed. The MI
/// fields then stay empty.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ControllerIdentityConfig {
    /// Credential mode the auth-sidecar uses to authenticate AS the
    /// blueprint. Defaults to `ManagedIdentityImds` for backward
    /// compatibility with kars deployments prior to Phase 4.
    ///
    /// - `ManagedIdentityImds`: sidecar uses
    ///   `SignedAssertionFromManagedIdentity` against the controller
    ///   MI's IMDS endpoint. Required in Entra tenants whose FIC
    ///   issuer-allowlist policy rejects the AKS OIDC issuer
    ///   (notably Microsoft-corporate, observed:
    ///   `InvalidFederatedIdentityCredentialValue`).
    /// - `WorkloadIdentity`: sidecar uses `SignedAssertionFilePath`
    ///   against the K8s SA token projected at
    ///   `/var/run/secrets/azure/tokens/azure-identity-token`.
    ///   Simpler, no per-cluster MI, no VMSS identity assignment.
    ///   Requires the cluster tenant to accept the AKS OIDC issuer.
    #[serde(default)]
    pub credential_mode: CredentialMode,

    /// Managed identity `clientId`. Required when `credentialMode` is
    /// `ManagedIdentityImds`. Ignored (and may be empty) when
    /// `WorkloadIdentity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_identity_client_id: Option<String>,

    /// Managed identity full ARM resource ID. Required when
    /// `credentialMode` is `ManagedIdentityImds`. Ignored (and may be
    /// empty) when `WorkloadIdentity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_identity_resource_id: Option<String>,

    /// Optional managed identity `principalId` (the SP object id used
    /// as the subject in the blueprint's MI-as-FIC). Recorded for
    /// drift detection in `auth_config_reconciler`; not consumed by
    /// the sidecar at runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_identity_principal_id: Option<String>,
}

/// Auth-sidecar credential source mode.
///
/// Determines which `AzureAd__ClientCredentials__0__SourceType` value
/// the auth-sidecar is configured with — and consequently whether the
/// cluster needs a per-cluster controller managed identity (Pattern A)
/// or relies solely on the auth-sidecar Service-Account's Workload
/// Identity projection (Pattern B).
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, PartialEq, Eq, Default)]
pub enum CredentialMode {
    /// Sidecar uses `SignedAssertionFromManagedIdentity` against the
    /// controller MI's IMDS endpoint. Default for backward compat.
    #[default]
    ManagedIdentityImds,
    /// Sidecar uses `SignedAssertionFilePath` against the projected
    /// K8s SA token. Requires the cluster tenant to accept the AKS
    /// OIDC issuer as a FIC subject.
    WorkloadIdentity,
}

impl ControllerIdentityConfig {
    /// `true` when the spec is in a configurationally-valid state for
    /// the chosen credential mode.
    ///
    /// - `ManagedIdentityImds`: requires `managed_identity_client_id`
    ///   to be `Some(non-empty)`.
    /// - `WorkloadIdentity`: no field requirements (the SA-WI
    ///   credential lives in the K8s SA token, not in the CR).
    pub fn is_valid_for_mode(&self) -> bool {
        match self.credential_mode {
            CredentialMode::ManagedIdentityImds => self
                .managed_identity_client_id
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty()),
            CredentialMode::WorkloadIdentity => true,
        }
    }
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

#[cfg(test)]
mod mesh_auth_backend_tests {
    use super::*;

    #[test]
    fn default_is_anonymous_for_backward_compat() {
        assert_eq!(MeshAuthBackend::default(), MeshAuthBackend::Anonymous);
    }

    #[test]
    fn deserialize_anonymous_round_trips() {
        let json = r#""Anonymous""#;
        let v: MeshAuthBackend = serde_json::from_str(json).expect("deserialize");
        assert_eq!(v, MeshAuthBackend::Anonymous);
    }

    #[test]
    fn deserialize_entra_agent_identity_round_trips() {
        let json = r#""EntraAgentIdentity""#;
        let v: MeshAuthBackend = serde_json::from_str(json).expect("deserialize");
        assert_eq!(v, MeshAuthBackend::EntraAgentIdentity);
    }

    #[test]
    fn deserialize_unknown_variant_is_rejected() {
        let json = r#""FutureUnknownVariant""#;
        let r: Result<MeshAuthBackend, _> = serde_json::from_str(json);
        assert!(
            r.is_err(),
            "unknown variants must be rejected so the controller doesn't silently fall back"
        );
    }
}
