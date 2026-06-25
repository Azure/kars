// Sandbox + kubelet RBAC for the AKS cluster, with **idempotent** role-
// assignment names.
//
// Why this is a separate module
// ─────────────────────────────
// A `Microsoft.Authorization/roleAssignments` `name` must be a GUID that is
// calculable at the START of the deployment (Bicep BCP120). The Azure-
// recommended idempotent pattern is `guid(scope, principalId, roleDefId)` so
// that:
//   • re-deploying with the SAME principal reuses the same name (no-op), and
//   • a ROTATED identity (new principalId) yields a NEW name → a CREATE, not a
//     conflicting UPDATE that fails with `RoleAssignmentUpdateNotPermitted`
//     ("principal ID ... not allowed to be updated").
// But `principalId` read off a live resource (`x.properties.principalId`,
// `aks...kubeletidentity.objectId`) is a runtime `reference()` — NOT allowed in
// a role-assignment `name` (BCP120). A **module parameter**, however, IS a
// deploy-start value inside the module, so the parent passes the runtime
// principalIds in as string params here and the GUIDs become legal.
//
// The AKS kubelet identity rotates whenever the cluster is recreated (e.g.
// `kars up --from-scratch` after a teardown); the sandbox UAMI's principalId
// rotates if its resource group is deleted and re-created with the same name.
// Both used to wedge re-deploys with `RoleAssignmentUpdateNotPermitted`; with
// principalId in the GUID they now create fresh assignments cleanly. Orphaned
// old assignments (principal deleted) are harmless.

@description('Resource ID of the sandbox user-assigned identity (scope for the MI-Contributor grant).')
param sandboxIdentityId string

@description('principalId (objectId) of the sandbox user-assigned identity.')
param sandboxPrincipalId string

@description('principalId (objectId) of the AKS kubelet identity.')
param kubeletPrincipalId string

@description('ACR name (the sandbox UAMI gets AcrPull on it; kubelet gets AcrPull at RG scope).')
param acrName string

@description('Key Vault name (the sandbox UAMI gets Key Vault Secrets User on it).')
param keyVaultName string

@description('Full resource ID of the Azure OpenAI / AI Services account, or empty when using an external endpoint.')
param openAiAccountId string = ''

// Built-in role definition GUIDs.
var acrPullRole = '7f951dda-4ed3-4680-a7ca-43fe172d538d'
var miContributorRole = 'e40ec5ca-96e0-45a2-b4ff-59039f2c2b59'
var openAiUserRole = '5e0bd9bd-7b93-4f28-af87-19fc36ad61bd'
var kvSecretsUserRole = '4633458b-17de-408a-b874-0445c86b69e6'

resource sandboxIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' existing = {
  name: last(split(sandboxIdentityId, '/'))
}

resource acr 'Microsoft.ContainerRegistry/registries@2023-07-01' existing = {
  name: acrName
}

resource keyVault 'Microsoft.KeyVault/vaults@2023-07-01' existing = {
  name: keyVaultName
}

resource openAiAccount 'Microsoft.CognitiveServices/accounts@2024-10-01' existing = if (!empty(openAiAccountId)) {
  name: last(split(openAiAccountId, '/'))
}

// Sandbox MI "Managed Identity Contributor" on itself — controller can
// create/delete federated credentials on the UAMI.
resource miContributor 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(sandboxIdentityId, sandboxPrincipalId, miContributorRole)
  scope: sandboxIdentity
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', miContributorRole)
    principalId: sandboxPrincipalId
    principalType: 'ServicePrincipal'
  }
}

// AKS kubelet AcrPull (RG scope — pull node/system images).
resource kubeletAcrPull 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(resourceGroup().id, kubeletPrincipalId, acrPullRole)
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', acrPullRole)
    principalId: kubeletPrincipalId
    principalType: 'ServicePrincipal'
  }
}

// Sandbox/controller workload identity AcrPull on the ACR (fetch signed
// egress-allowlist / policy artifacts; anonymous pull is disabled by default).
resource sandboxAcrPull 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(acr.id, sandboxPrincipalId, acrPullRole)
  scope: acr
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', acrPullRole)
    principalId: sandboxPrincipalId
    principalType: 'ServicePrincipal'
  }
}

// Sandbox identity "Cognitive Services OpenAI User" on the AOAI resource
// (only when AOAI is deployed in-RG).
resource openAiRoleAssignment 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(openAiAccountId)) {
  name: guid(openAiAccountId, sandboxPrincipalId, openAiUserRole)
  scope: openAiAccount
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', openAiUserRole)
    principalId: sandboxPrincipalId
    principalType: 'ServicePrincipal'
  }
}

// Sandbox identity "Key Vault Secrets User" on Key Vault.
resource kvRoleAssignment 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(keyVault.id, sandboxPrincipalId, kvSecretsUserRole)
  scope: keyVault
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', kvSecretsUserRole)
    principalId: sandboxPrincipalId
    principalType: 'ServicePrincipal'
  }
}
