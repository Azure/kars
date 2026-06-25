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

resource controllerIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' existing = {
  name: controllerIdentityName
}

// Idempotent assignment via the shared module: the name GUID includes the
// controller identity's principalId (passed as a string param — BCP120-safe),
// so re-applying after the UAMI is recreated CREATEs a fresh assignment instead
// of failing with RoleAssignmentUpdateNotPermitted.
module controllerAcrPull '../modules/acr-pull-assignment.bicep' = {
  name: 'controller-acrpull'
  params: {
    acrName: acrName
    principalId: controllerIdentity.properties.principalId
  }
}

output roleAssignmentId string = controllerAcrPull.outputs.roleAssignmentId
output principalId string = controllerIdentity.properties.principalId
