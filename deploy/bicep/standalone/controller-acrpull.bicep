// Standalone Bicep that grants the controller workload identity
// AcrPull on a specific ACR.
//
// Use this when you don't want to re-run the full main.bicep (e.g.,
// you're patching an existing deployment whose baseName/uniqueString
// scheme differs from the current template). Idempotent — safe to
// re-apply.
//
// Usage:
//   az deployment group create \
//     --resource-group <rg> \
//     --template-file deploy/bicep/standalone/controller-acrpull.bicep \
//     --parameters acrName=<acr> controllerIdentityName=<uami>
//
// Defaults match the live `kars-westus3` RG so the typical
// invocation reduces to:
//   az deployment group create -g kars-westus3 \
//     -f deploy/bicep/standalone/controller-acrpull.bicep

@description('ACR (registry) name in this resource group.')
param acrName string = 'karsacr'

@description('User-assigned managed identity used by the AKS sandbox / controller workload identity.')
param controllerIdentityName string = 'kars-aks-sandbox-wi'

resource acr 'Microsoft.ContainerRegistry/registries@2023-07-01' existing = {
  name: acrName
}

resource controllerIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' existing = {
  name: controllerIdentityName
}

// AcrPull built-in role
var acrPullRoleId = subscriptionResourceId(
  'Microsoft.Authorization/roleDefinitions',
  '7f951dda-4ed3-4680-a7ca-43fe172d538d'
)

resource controllerAcrPull 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(controllerIdentity.id, acr.id, 'acrpull')
  scope: acr
  properties: {
    roleDefinitionId: acrPullRoleId
    principalId: controllerIdentity.properties.principalId
    principalType: 'ServicePrincipal'
  }
}

output roleAssignmentId string = controllerAcrPull.id
output principalId string = controllerIdentity.properties.principalId
