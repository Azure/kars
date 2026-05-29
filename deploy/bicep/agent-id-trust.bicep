// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//
// Bicep deployment for the kars Entra Agent ID trust anchor.
//
// Why Bicep instead of `az rest` calls? Some Microsoft-corporate
// tenants apply Conditional Access "token-binding" policy
// (AADSTS530084) to the Azure CLI's first-party app when it tries to
// acquire a Microsoft Graph token. This blocks the imperative path
// (`kars mesh setup-trust --mode agent-id`) even when the user holds
// the `Agent ID Developer` role. Bicep goes through ARM's deployment
// engine, which has its own auth path to the Microsoft.Graph
// extension and is not subject to the same CA token-binding policy.
//
// What this template creates:
//
//   1. Microsoft.Graph/applications      — the blueprint app, tagged
//      `EntraAgentId` so the Entra Agents portal lists it.
//   2. Microsoft.Graph/servicePrincipals — blueprint service
//      principal (required for derivation).
//   3. Microsoft.ManagedIdentity/userAssignedIdentities — the
//      per-cluster controller MI (Pattern A only; skipped in
//      Pattern B).
//   4. Microsoft.Graph/applications/federatedIdentityCredentials
//      — one of TWO patterns:
//         A. `ManagedIdentityImds`: MI-as-FIC on the blueprint, using
//            the universally allow-listed
//            `login.microsoftonline.com/<tenant>/v2.0` issuer.
//            Required in tenants whose issuer allowlist policy
//            rejects the AKS OIDC issuer.
//         B. `WorkloadIdentity`: SA-as-FIC on the blueprint, using
//            the AKS cluster's OIDC issuer URL. Subject is
//            `system:serviceaccount:kars-system:entra-auth-sidecar`.
//            Simpler, no per-cluster MI required.
//
// Outputs are consumed by `kars mesh setup-trust --mode bicep`
// to render the `KarsAuthConfig/default` CR for the cluster.
//
// Subscription scope: chosen so engineers with Contributor/Owner on
// a personal sub (but not tenant deployment rights) can deploy.
// The Microsoft.Graph resources still land in the tenant.

targetScope = 'subscription'

extension microsoftGraphV1

@description('Cluster name suffix. Used in the blueprint display name and the controller MI name. Matches what `kars up --name <name>` would use.')
param clusterName string = 'kars'

@description('Blueprint display name. Tenant-wide; multiple clusters in the same tenant share it.')
param blueprintDisplayName string = 'kars-blueprint'

@description('Resource group for the controller managed identity. Created if it does not exist. Ignored when credentialMode=WorkloadIdentity.')
param resourceGroupName string = '${clusterName}-agentid-rg'

@description('Azure region for the controller managed identity. Ignored when credentialMode=WorkloadIdentity.')
param region string = 'eastus'

@description('Microsoft ServiceTree GUID for app-registration ownership. Required in Microsoft-corporate tenants. For non-Microsoft tenants, leave empty.')
param serviceManagementReference string = ''

@description('Tenant ID. Defaults to the deployment tenant. Override only for cross-tenant scenarios.')
param tenantId string = tenant().tenantId

@description('Credential mode for the auth-sidecar. `ManagedIdentityImds` creates a controller MI and an MI-as-FIC on the blueprint (Pattern A, corp-tenant safe). `WorkloadIdentity` creates a SA-as-FIC pointing at the cluster\'s AKS OIDC issuer (Pattern B, OSS / non-restricted tenants).')
@allowed([
  'ManagedIdentityImds'
  'WorkloadIdentity'
])
param credentialMode string = 'ManagedIdentityImds'

@description('AKS cluster\'s OIDC issuer URL. Required when credentialMode=WorkloadIdentity. Obtain via `az aks show -n <c> -g <rg> --query oidcIssuerProfile.issuerUrl -o tsv`.')
param aksOidcIssuerUrl string = ''

@description('K8s namespace hosting the shared auth-sidecar Deployment. Used to build the FIC subject in Pattern B.')
param sidecarNamespace string = 'kars-system'

@description('K8s ServiceAccount name of the shared auth-sidecar. Used to build the FIC subject in Pattern B.')
param sidecarServiceAccount string = 'entra-auth-sidecar'

// ── Mode-derived booleans ──────────────────────────────────────────
var useMi = credentialMode == 'ManagedIdentityImds'
var useWi = credentialMode == 'WorkloadIdentity'

// ── 1. Resource group for the controller MI (Pattern A only) ──────
resource agentIdRg 'Microsoft.Resources/resourceGroups@2024-03-01' = if (useMi) {
  name: resourceGroupName
  location: region
}

// ── 2. Controller managed identity (Pattern A only) ────────────────
//
// Nested deployment because user-assigned MIs are scoped to a
// resource group, and we declared the deployment at subscription
// scope so we could create the RG above.
module controllerMi 'modules/controller-mi.bicep' = if (useMi) {
  name: 'controllerMi'
  scope: agentIdRg
  params: {
    name: '${clusterName}-controller-mi'
    location: region
  }
}

// ── 3. Blueprint application (always) ──────────────────────────────
//
// Tagged `EntraAgentId` so the Entra Agents portal page recognises
// it as a blueprint. Microsoft-corporate tenants additionally require
// the serviceManagementReference field — kept blank by default and
// passed through when supplied.
resource blueprintApp 'Microsoft.Graph/applications@v1.0' = {
  uniqueName: '${blueprintDisplayName}-${clusterName}'
  displayName: blueprintDisplayName
  signInAudience: 'AzureADMyOrg'
  tags: [ 'EntraAgentId', 'kars-managed' ]
  serviceManagementReference: serviceManagementReference
  requiredResourceAccess: []
}

// ── 4. Blueprint service principal (always) ────────────────────────
//
// The blueprint app must have a corresponding SP for two reasons:
// the Entra Agents portal filters by SP type, and agent identities
// derive from the SP (not the App). Skipping this leaves the
// blueprint invisible in the portal and unusable for derivation.
resource blueprintSp 'Microsoft.Graph/servicePrincipals@v1.0' = {
  appId: blueprintApp.appId
  displayName: blueprintDisplayName
  tags: [ 'EntraAgentId', 'kars-managed' ]
}

// ── 5a. MI-as-FIC on the blueprint (Pattern A only) ────────────────
//
// The federation issuer is the tenant's own login endpoint, which
// every Entra tenant allow-lists by default. Subject is the
// controller MI's principalId. This is the anti-loop-safe credential
// path proven during the POC — IMDS tokens from this MI are NOT
// FIC-derived, so presenting them as the blueprint's MI-as-FIC
// assertion does not trigger AADSTS700231.
//
// `environment().authentication.loginEndpoint` keeps this template
// portable across Azure Public / Gov / China clouds; the Bicep
// `no-hardcoded-env-urls` linter rule requires it.
var loginEndpoint = environment().authentication.loginEndpoint
resource blueprintMiFic 'Microsoft.Graph/applications/federatedIdentityCredentials@v1.0' = if (useMi) {
  name: '${blueprintApp.uniqueName}/kars-controller-mi'
  audiences: [ 'api://AzureADTokenExchange' ]
  issuer: '${loginEndpoint}${tenantId}/v2.0'
  subject: controllerMi!.outputs.principalId
  description: 'kars controller MI — trust hop into the blueprint (kars cluster ${clusterName})'
}

// ── 5b. SA-as-FIC on the blueprint (Pattern B only) ────────────────
//
// Subject is the K8s ServiceAccount the auth-sidecar pod uses. The
// AKS azure-wi-webhook projects an SA token signed by the cluster's
// AKS OIDC issuer; that token is presented as the federated
// assertion when the sidecar mints a blueprint token.
//
// Pattern B requires the tenant to accept the AKS OIDC issuer as a
// FIC issuer. The Microsoft corporate tenant rejects this with
// `InvalidFederatedIdentityCredentialValue`; in that case the
// caller MUST deploy with credentialMode=ManagedIdentityImds
// instead.
resource blueprintSaFic 'Microsoft.Graph/applications/federatedIdentityCredentials@v1.0' = if (useWi) {
  name: '${blueprintApp.uniqueName}/kars-auth-sidecar-sa'
  audiences: [ 'api://AzureADTokenExchange' ]
  issuer: aksOidcIssuerUrl
  subject: 'system:serviceaccount:${sidecarNamespace}:${sidecarServiceAccount}'
  description: 'kars auth-sidecar SA — trust hop into the blueprint (kars cluster ${clusterName})'
}

// ── Outputs consumed by the kars CLI ───────────────────────────────
output tenantId string = tenantId
output blueprintClientId string = blueprintApp.appId
output blueprintObjectId string = blueprintApp.id
output blueprintSpObjectId string = blueprintSp.id
output credentialMode string = credentialMode
output controllerMiClientId string = useMi ? controllerMi!.outputs.clientId : ''
output controllerMiResourceId string = useMi ? controllerMi!.outputs.resourceId : ''
output controllerMiPrincipalId string = useMi ? controllerMi!.outputs.principalId : ''
output serviceManagementReference string = serviceManagementReference
output ficName string = useMi ? blueprintMiFic!.name : blueprintSaFic!.name
output aksOidcIssuerUrl string = aksOidcIssuerUrl
