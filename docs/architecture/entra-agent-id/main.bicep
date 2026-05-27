// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//
// POC: provision an Entra Agent ID via Bicep's Microsoft.Graph extension.
//
// Bicep abstracts the underlying Graph API calls + handles auth via the
// deployment principal. Privileges still required: the user/MI running
// `az deployment` must have Graph permission to write applications +
// service principals. Concretely, one of:
//
//   • Application Administrator (Entra role)            — least privileged
//   • Cloud Application Administrator                   — same surface
//   • Global Administrator                              — overkill but works
//   • Custom role with microsoft.directory/applications/* permissions
//
// Subscription scope is used (instead of tenant scope) because most
// Microsoft engineers have Contributor/Owner on a personal sub but not
// tenant-level deployment rights. The Graph resources created here are
// still tenant-scoped — Bicep just uses the subscription as the
// deployment location for the deployment record.
//
// What this template creates (and tears down on `az deployment delete`):
//
//   1. Microsoft.Graph/applications      — the Entra "agent identity"
//   2. Microsoft.Graph/servicePrincipals — tenant-scoped instance
//   3. Microsoft.Graph/applications/federatedIdentityCredentials
//        — maps a synthetic K8s service-account JWT to this app
//
// Wired up to match the shape kars' fedcred.rs already produces:
//   issuer  = "https://oidc.example.test/poc-${runId}"
//   subject = "system:serviceaccount:kars-poc-${runId}:sandbox"

targetScope = 'subscription'

extension microsoftGraphV1

@description('Display name for the new Entra Agent ID. Stays human-readable in the portal.')
param displayName string = 'kars-entra-agentid-poc'

@description('Unique tag so re-runs cohabit. Defaults to a deployment-time UTC stamp.')
param runId string = utcNow('yyyyMMddHHmmss')

@description('OIDC issuer to federate from. The default is a stand-in URL; real kars uses the AKS cluster OIDC issuer.')
param fedCredIssuer string = 'https://oidc.example.test/poc-${runId}'

@description('Subject the federated credential will accept (K8s SA JWT sub claim).')
param fedCredSubject string = 'system:serviceaccount:kars-poc-${runId}:sandbox'

@description('Microsoft ServiceTree GUID for app-registration ownership. Required in corporate Microsoft tenants (the AAD app registration policy rejects creates without it). Find yours at https://aka.ms/servicetree or `az servicemanagement-reference list`. For non-Microsoft tenants, leave empty.')
param serviceManagementReference string = ''

@description('Set to false to skip federated credential creation. Useful in corp tenants where fedcred issuers are policy-whitelisted and the deployment principal cannot add new ones.')
param createFederatedCredential bool = true

// ── 1. Entra application (the "agent identity") ──────────────────
//
// uniqueName must be globally unique and immutable — use the runId
// to make re-runs trivially distinct. The displayName is what shows
// in the portal and what `az ad app list` returns.
resource agentApp 'Microsoft.Graph/applications@v1.0' = {
  uniqueName: '${displayName}-${runId}'
  displayName: '${displayName} (${runId})'
  signInAudience: 'AzureADMyOrg'

  // Tag with EntraAgentId so directory queries can filter agents
  // from regular app registrations. This is what the GA Entra Agent ID
  // portal uses to surface agents in the dedicated UI section.
  tags: ['EntraAgentId', 'kars-poc']

  // Microsoft corporate tenant requires every app registration to
  // declare a ServiceTree owner GUID. Outside Microsoft this is a
  // no-op (Graph ignores empty). Inside Microsoft it satisfies the
  // ServiceTreeValueMissing policy enforcement.
  serviceManagementReference: serviceManagementReference

  // No required API permissions for the POC. In production we'd add
  // Microsoft Graph Application.Read.All + Foundry scopes per role.
  requiredResourceAccess: []
}

// ── 2. Service principal — binds the app to this tenant ──────────
resource agentSp 'Microsoft.Graph/servicePrincipals@v1.0' = {
  appId: agentApp.appId
  displayName: '${displayName} (${runId})'
  tags: ['EntraAgentId', 'kars-poc']
}

// ── 3. Federated identity credential ─────────────────────────────
//
// This is the per-sandbox surface kars cares about most. In production
// the controller writes one of these per ClawSandbox using the AKS
// cluster's OIDC issuer URL + `system:serviceaccount:azureclaw-<name>:sandbox`.
// For the POC we use stand-in values so the operation completes
// without needing an AKS cluster.
resource agentFedCred 'Microsoft.Graph/applications/federatedIdentityCredentials@v1.0' = if (createFederatedCredential) {
  name: '${agentApp.uniqueName}/poc-fedcred-${runId}'
  audiences: ['api://AzureADTokenExchange']
  issuer: fedCredIssuer
  subject: fedCredSubject
  description: 'kars POC — synthetic K8s SA mapping (runId=${runId})'
}

// ── Outputs ──────────────────────────────────────────────────────
output appId string = agentApp.appId
output appObjectId string = agentApp.id
output spObjectId string = agentSp.id
output uniqueName string = agentApp.uniqueName
output displayName string = agentApp.displayName
output fedCredName string = createFederatedCredential ? agentFedCred.name : ''
