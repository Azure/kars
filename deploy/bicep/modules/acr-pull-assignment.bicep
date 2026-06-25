// Reusable, **idempotent** AcrPull role assignment scoped to an ACR.
//
// `principalId` is a STRING parameter (legal in a roleAssignment `name`, unlike
// a runtime `reference()` — Bicep BCP120), so the assignment is named
// `guid(acr.id, principalId, roleId)`. A re-deploy with the same principal is a
// no-op; a rotated identity (same UAMI name re-created, etc.) yields a NEW name
// → a clean CREATE instead of failing with `RoleAssignmentUpdateNotPermitted`.

@description('ACR (registry) name in this resource group.')
param acrName string

@description('Object ID (principalId) of the identity to grant AcrPull.')
param principalId string

var acrPullRoleId = '7f951dda-4ed3-4680-a7ca-43fe172d538d'

resource acr 'Microsoft.ContainerRegistry/registries@2023-07-01' existing = {
  name: acrName
}

resource assignment 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(acr.id, principalId, acrPullRoleId)
  scope: acr
  properties: {
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', acrPullRoleId)
    principalId: principalId
    principalType: 'ServicePrincipal'
  }
}

output roleAssignmentId string = assignment.id
